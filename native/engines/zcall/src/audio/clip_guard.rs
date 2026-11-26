//! PR #220 microphone source synchronization, mute handling, and bidirectional `ClipGuard`.

use super::backend::{
    pulse_get_source_info, pulse_normalize_own_stream, set_source_mute, set_source_volume,
};
use super::leveler::MicLeveler;
use std::env;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceState {
    pub pct: u8,
    pub muted: bool,
    pub resolved_name: Option<String>,
}

pub trait VolumeBackend: Send + Sync {
    fn get_source_state(&self, source: Option<&str>) -> Option<SourceState>;
    fn set_source_volume(&self, source: Option<&str>, pct: u8) -> bool;
    fn set_source_mute(&self, source: Option<&str>, mute: bool) -> bool;
    fn normalize_own_stream(&self) {}
}

struct SystemVolumeBackend;

impl VolumeBackend for SystemVolumeBackend {
    fn get_source_state(&self, source: Option<&str>) -> Option<SourceState> {
        pulse_get_source_info(source).map(|info| SourceState {
            pct: info.pct,
            muted: info.muted,
            resolved_name: info.resolved_name,
        })
    }

    fn set_source_volume(&self, source: Option<&str>, pct: u8) -> bool {
        set_source_volume(source, pct.min(100)).is_ok()
    }

    fn set_source_mute(&self, source: Option<&str>, mute: bool) -> bool {
        set_source_mute(source, mute).is_ok()
    }

    fn normalize_own_stream(&self) {
        pulse_normalize_own_stream();
    }
}

struct NullVolumeBackend;

impl VolumeBackend for NullVolumeBackend {
    fn get_source_state(&self, _source: Option<&str>) -> Option<SourceState> {
        None
    }
    fn set_source_volume(&self, _source: Option<&str>, _pct: u8) -> bool {
        false
    }
    fn set_source_mute(&self, _source: Option<&str>, _mute: bool) -> bool {
        false
    }
}

/// Apply smooth polynomial soft-saturation to prevent audio clipping.
///
/// For samples within [-20000, 20000], signal is untouched (strictly linear).
/// For samples exceeding 20000, a smooth C1 cubic curve compresses peaks asymptotically
/// towards 32000, completely eliminating harsh square-wave clipping.
pub fn apply_soft_limiter(samples: &mut [i16]) {
    const THRESHOLD: f32 = 20000.0;
    const HEADROOM: f32 = 18000.0; // 20000 + 18000 * (1 - 1/3) = 32000

    for s in samples.iter_mut() {
        let val = f32::from(*s);
        let abs_val = val.abs();
        if abs_val > THRESHOLD {
            let sign = if val > 0.0 { 1.0 } else { -1.0 };
            let u = ((abs_val - THRESHOLD) / HEADROOM).min(1.0);
            let compressed = THRESHOLD + HEADROOM * (u - (u * u * u) / 3.0);
            *s = (sign * compressed).clamp(-32767.0, 32767.0) as i16;
        }
    }
}

#[derive(Default)]
struct ClipGuardConfig {
    enabled: bool,
    apply_os: bool,
}

#[derive(Default)]
struct ClipGuardState {
    was_manual: bool,
    os_muted: bool,
}

/// `ClipGuard` monitors raw captured PCM samples for near-clipping (>= 28500),
/// sustained hot speech (> -12 dBFS), and sustained quiet near-end speech (< -28 dBFS),
/// adjusting hardware source volume bidirectionally (35%..100%) and syncing DE changes.
pub struct ClipGuard {
    device: Option<String>,
    current_vol: Arc<AtomicU8>,
    far_active: Option<Arc<AtomicBool>>,
    manual: Arc<AtomicBool>,
    config: ClipGuardConfig,
    state: ClipGuardState,
    last_resolved_default: Option<String>,
    backend: Arc<dyn VolumeBackend>,
    last_os_sync: std::time::Instant,
    last_vol_write: Arc<Mutex<std::time::Instant>>,
    clipped_frames_in_window: u8,
    window_frame_count: u8,
    last_step: std::time::Instant,
    last_clip: std::time::Instant,
    levels: [f32; 250],
    level_idx: usize,
    level_count: usize,
    noise_db: f32,
    quiet_speech_frames: u16,
    hot_speech_frames: u16,
}

impl ClipGuard {
    pub fn new(device: Option<String>, current_vol: Arc<AtomicU8>) -> Self {
        let enabled = env::var("ZCALL_MIC_AGC").map_or(true, |v| v != "0");
        let past = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(10))
            .unwrap();
        let backend: Arc<dyn VolumeBackend> = if cfg!(test) {
            Arc::new(NullVolumeBackend)
        } else {
            Arc::new(SystemVolumeBackend)
        };
        Self {
            device,
            current_vol,
            far_active: None,
            manual: Arc::new(AtomicBool::new(false)),
            config: ClipGuardConfig {
                enabled,
                apply_os: cfg!(not(test)),
            },
            state: ClipGuardState::default(),
            last_resolved_default: None,
            backend,
            last_os_sync: past,
            last_vol_write: Arc::new(Mutex::new(past)),
            clipped_frames_in_window: 0,
            window_frame_count: 0,
            last_step: std::time::Instant::now(),
            last_clip: past,
            levels: [-70.0; 250],
            level_idx: 0,
            level_count: 0,
            noise_db: -70.0,
            quiet_speech_frames: 0,
            hot_speech_frames: 0,
        }
    }

    pub fn initialize_device(&mut self) {
        if let Some(state) = self.backend.get_source_state(self.device.as_deref()) {
            self.state.os_muted = state.muted;
            self.last_resolved_default = state.resolved_name;
            let mut actual = state.pct.min(100);
            if self.config.enabled
                && !self.manual.load(Ordering::Relaxed)
                && actual > 80
                && self.backend.set_source_volume(self.device.as_deref(), 70)
            {
                actual = self
                    .backend
                    .get_source_state(self.device.as_deref())
                    .map_or(70, |value| value.pct.min(100));
            }
            self.current_vol.store(actual, Ordering::Relaxed);
            if self.config.enabled && !self.manual.load(Ordering::Relaxed) {
                self.backend.normalize_own_stream();
            }
        }
    }

    #[must_use]
    pub fn with_controls(
        mut self,
        far_active: Option<Arc<AtomicBool>>,
        manual: Arc<AtomicBool>,
    ) -> Self {
        self.far_active = far_active;
        self.manual = manual;
        self
    }

    pub fn mark_manual_write(&self) {
        if let Ok(mut ts) = self.last_vol_write.lock() {
            *ts = std::time::Instant::now();
        }
    }

    #[must_use]
    pub const fn is_os_muted(&self) -> bool {
        self.state.os_muted
    }

    #[must_use]
    pub fn with_backend(mut self, backend: Arc<dyn VolumeBackend>, apply_os: bool) -> Self {
        self.backend = backend;
        self.config.apply_os = apply_os;
        self
    }

    /// Syncs external DE/WirePlumber volume, mute state, and default-source hot-plugs.
    /// Returns `true` when `device` is `None` (`@DEFAULT_SOURCE@`) and the resolved default source changed.
    pub fn sync_os_volume(&mut self) -> bool {
        if !self.config.apply_os {
            return false;
        }

        // Handle the Auto transition before the periodic/external-write throttles.
        // Otherwise the first capture frame could consume the transition while the
        // OS sync is throttled, leaving the hardware source muted indefinitely.
        let manual_now = self.manual.load(Ordering::Relaxed);
        let entering_auto = self.state.was_manual && !manual_now;
        if entering_auto {
            self.clipped_frames_in_window = 0;
            self.window_frame_count = 0;
            self.quiet_speech_frames = 0;
            self.hot_speech_frames = 0;
            self.last_step = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(2))
                .unwrap();
            if self.backend.set_source_mute(self.device.as_deref(), false) {
                self.state.os_muted = false;
            }
            self.backend.normalize_own_stream();
            // PR #220 restores a useful capture floor when Auto is explicitly
            // re-enabled; startup and device switches do not boost low levels.
            if self.config.enabled {
                if let Some(state) = self.backend.get_source_state(self.device.as_deref()) {
                    if state.pct < 20 && self.backend.set_source_volume(self.device.as_deref(), 50)
                    {
                        self.current_vol.store(50, Ordering::Relaxed);
                    }
                }
            }
        }
        self.state.was_manual = manual_now;

        if self.last_os_sync.elapsed() < std::time::Duration::from_millis(500) {
            return false;
        }
        if let Ok(ts) = self.last_vol_write.lock() {
            if ts.elapsed() < std::time::Duration::from_millis(800) {
                return false;
            }
        }
        self.last_os_sync = std::time::Instant::now();
        let Some(state) = self.backend.get_source_state(self.device.as_deref()) else {
            return false;
        };

        let mut default_changed = false;
        if self.device.is_none() {
            if let Some(ref resolved) = state.resolved_name {
                if let Some(ref prev_resolved) = self.last_resolved_default {
                    if prev_resolved != resolved {
                        crate::zcall_log!(
                            "[audio-io] info: default microphone changed ({} -> {})",
                            prev_resolved,
                            resolved
                        );
                        default_changed = true;
                    }
                }
                self.last_resolved_default = Some(resolved.clone());
            }
        }

        if state.muted != self.state.os_muted {
            self.state.os_muted = state.muted;
            if state.muted {
                crate::zcall_log!("[audio-io] warn: OS microphone source is muted");
            } else {
                crate::zcall_log!("[audio-io] info: OS microphone source unmuted");
            }
        }

        let actual = state.pct.min(100);
        let prev = self.current_vol.swap(actual, Ordering::Relaxed);
        if prev != actual {
            crate::zcall_log!("[audio-io] mic volume: {}%", actual);
        }

        if self.config.enabled && !self.manual.load(Ordering::Relaxed) && !default_changed {
            self.backend.normalize_own_stream();
        }

        default_changed
    }

    pub fn set_device(&mut self, device: Option<String>) {
        self.device = device;
        self.state.os_muted = false;
        self.last_resolved_default = None;
        self.clipped_frames_in_window = 0;
        self.window_frame_count = 0;
        self.level_idx = 0;
        self.level_count = 0;
        self.noise_db = -70.0;
        self.quiet_speech_frames = 0;
        self.hot_speech_frames = 0;
    }

    fn apply_step(&mut self, new_vol: u8) {
        if self.config.apply_os {
            if !self
                .backend
                .set_source_volume(self.device.as_deref(), new_vol)
            {
                crate::zcall_log!(
                    "[audio-io] warn: failed to auto-adjust mic volume to {}%",
                    new_vol
                );
                self.last_step = std::time::Instant::now();
                return;
            }
            let actual = self
                .backend
                .get_source_state(self.device.as_deref())
                .map_or(new_vol, |s| s.pct.min(100));
            if let Ok(mut ts) = self.last_vol_write.lock() {
                *ts = std::time::Instant::now();
            }
            self.current_vol.store(actual, Ordering::Relaxed);
            crate::zcall_log!("[audio-io] mic volume: {}%", actual);
        } else {
            self.current_vol.store(new_vol, Ordering::Relaxed);
            crate::zcall_log!("[audio-io] mic volume: {}%", new_vol);
        }
        self.last_step = std::time::Instant::now();
    }

    pub fn feed(&mut self, raw_samples: &[i16]) {
        let has_clip = raw_samples.iter().any(|&s| s.abs() >= 28500);
        if has_clip {
            self.clipped_frames_in_window += 1;
            self.last_clip = std::time::Instant::now();
            self.quiet_speech_frames = 0;
            self.hot_speech_frames = 0;
        }
        self.window_frame_count += 1;

        let level_db = MicLeveler::compute_level_db(raw_samples);
        self.levels[self.level_idx] = level_db;
        self.level_idx = (self.level_idx + 1) % self.levels.len();
        if self.level_count < self.levels.len() {
            self.level_count += 1;
        }
        if self.level_count >= 20 && (self.level_idx.is_multiple_of(10) || self.level_count == 20) {
            let mut sorted = self.levels[..self.level_count].to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            self.noise_db = sorted[self.level_count / 10];
        }

        let is_manual = self.manual.load(Ordering::Relaxed);
        if self.state.was_manual && !is_manual {
            // Re-entering auto mode: clear stale counters so AGC reacts cleanly.
            self.clipped_frames_in_window = 0;
            self.window_frame_count = 0;
            self.quiet_speech_frames = 0;
            self.hot_speech_frames = 0;
            self.last_step = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(2))
                .unwrap();
        }
        self.state.was_manual = is_manual;
        let agc_active = self.config.enabled && !is_manual;

        if self.window_frame_count >= 10 {
            if agc_active
                && self.clipped_frames_in_window >= 3
                && self.last_step.elapsed() >= std::time::Duration::from_millis(400)
            {
                let cur = self.current_vol.load(Ordering::Relaxed);
                if cur > 35 {
                    let new_vol = cur.saturating_sub(10).max(35);
                    self.apply_step(new_vol);
                }
            }
            self.clipped_frames_in_window = 0;
            self.window_frame_count = 0;
        }

        if !agc_active || has_clip {
            return;
        }

        let far_is_talking = self
            .far_active
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed));
        if far_is_talking {
            return;
        }

        if level_db > -12.0 {
            self.hot_speech_frames = self.hot_speech_frames.saturating_add(1);
            self.quiet_speech_frames = 0;
            if self.hot_speech_frames >= 15
                && self.last_step.elapsed() >= std::time::Duration::from_millis(1000)
            {
                let cur = self.current_vol.load(Ordering::Relaxed);
                if cur > 35 {
                    let new_vol = cur.saturating_sub(5).max(35);
                    self.apply_step(new_vol);
                }
                self.hot_speech_frames = 0;
            }
            return;
        }
        self.hot_speech_frames = self.hot_speech_frames.saturating_sub(1);

        if self.state.os_muted {
            return;
        }

        if self.level_count >= 20 && level_db > -58.0 && level_db > self.noise_db + 9.0 {
            if level_db < -28.0 {
                self.quiet_speech_frames = self.quiet_speech_frames.saturating_add(1);
            } else if level_db >= -24.0 {
                self.quiet_speech_frames = self.quiet_speech_frames.saturating_sub(2);
            }
        }

        if self.quiet_speech_frames >= 10
            && self.last_clip.elapsed() >= std::time::Duration::from_secs(3)
            && self.last_step.elapsed() >= std::time::Duration::from_millis(1000)
        {
            let cur = self.current_vol.load(Ordering::Relaxed);
            if cur < 100 {
                let step = if level_db < -36.0 { 8 } else { 5 };
                let new_vol = cur.saturating_add(step).min(100);
                self.apply_step(new_vol);
            }
            self.quiet_speech_frames = 0;
        }
    }
}

#[cfg(test)]
#[path = "../tests/audio/clip_guard.rs"]
mod tests;
