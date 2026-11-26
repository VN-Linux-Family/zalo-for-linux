use super::*;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

#[test]
fn soft_limiter_preserves_linear_region() {
    let mut samples = vec![0i16, 1000, -1000, 15000, -20000, 20000];
    let original = samples.clone();
    apply_soft_limiter(&mut samples);
    assert_eq!(samples, original);
}

#[test]
fn soft_limiter_compresses_peaks_without_hard_clipping() {
    let mut samples = vec![25000, 30000, 32767, -25000, -30000, -32768];
    apply_soft_limiter(&mut samples);
    for &sample in &samples {
        assert!(sample.abs() <= 32000, "compressed sample: {sample}");
        assert!(sample.abs() > 20000);
    }
    assert_eq!(samples[2], 30626);
    assert_eq!(samples[5], -30626);
}

#[test]
fn clip_guard_steps_down_on_repeated_clipping() {
    let volume = Arc::new(AtomicU8::new(100));
    let mut guard = ClipGuard::new(None, Arc::clone(&volume));
    guard.last_step = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap();
    let clipped = vec![30500i16; 320];
    for _ in 0..10 {
        guard.feed(&clipped);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 90);

    volume.store(42, Ordering::Relaxed);
    guard.last_step = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap();
    for _ in 0..10 {
        guard.feed(&clipped);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 35);
    guard.last_step = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap();
    for _ in 0..10 {
        guard.feed(&clipped);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 35);
}

#[test]
fn clip_guard_raises_gain_for_near_end_quiet_speech_only() {
    let volume = Arc::new(AtomicU8::new(40));
    let far_active = Arc::new(AtomicBool::new(false));
    let manual = Arc::new(AtomicBool::new(false));
    let mut guard = ClipGuard::new(None, Arc::clone(&volume))
        .with_controls(Some(Arc::clone(&far_active)), Arc::clone(&manual));
    guard.config.enabled = true;
    guard.last_step = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(2))
        .unwrap();

    let noise = vec![20i16; 320];
    for _ in 0..25 {
        guard.feed(&noise);
    }
    far_active.store(true, Ordering::Relaxed);
    let quiet_speech = vec![500i16; 320];
    for _ in 0..30 {
        guard.feed(&quiet_speech);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 40);

    far_active.store(false, Ordering::Relaxed);
    for _ in 0..15 {
        guard.feed(&quiet_speech);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 48);

    manual.store(true, Ordering::Relaxed);
    guard.last_step = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(2))
        .unwrap();
    for _ in 0..15 {
        guard.feed(&quiet_speech);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 48);
}

#[derive(Default)]
struct MockState {
    pct: u8,
    muted: bool,
    resolved_name: Option<String>,
    normalized_calls: u32,
}

#[derive(Default)]
struct MockVolumeBackend {
    state: Mutex<MockState>,
}

impl MockVolumeBackend {
    fn new(pct: u8, muted: bool, resolved_name: Option<&str>) -> Self {
        Self {
            state: Mutex::new(MockState {
                pct,
                muted,
                resolved_name: resolved_name.map(str::to_owned),
                normalized_calls: 0,
            }),
        }
    }
}

impl VolumeBackend for MockVolumeBackend {
    fn get_source_state(&self, _source: Option<&str>) -> Option<SourceState> {
        let state = self.state.lock().unwrap();
        Some(SourceState {
            pct: state.pct,
            muted: state.muted,
            resolved_name: state.resolved_name.clone(),
        })
    }
    fn set_source_volume(&self, _source: Option<&str>, pct: u8) -> bool {
        self.state.lock().unwrap().pct = pct.min(100);
        true
    }
    fn set_source_mute(&self, _source: Option<&str>, muted: bool) -> bool {
        self.state.lock().unwrap().muted = muted;
        true
    }
    fn normalize_own_stream(&self) {
        self.state.lock().unwrap().normalized_calls += 1;
    }
}

#[test]
fn initialize_device_caps_excessive_startup_volume() {
    let backend = Arc::new(MockVolumeBackend::new(100, false, Some("alsa_input")));
    let volume = Arc::new(AtomicU8::new(100));
    let mut guard = ClipGuard::new(None, Arc::clone(&volume)).with_backend(backend.clone(), true);
    guard.initialize_device();
    assert_eq!(volume.load(Ordering::Relaxed), 70);
    assert_eq!(backend.state.lock().unwrap().pct, 70);
    assert_eq!(backend.state.lock().unwrap().normalized_calls, 1);
}

#[test]
fn sync_os_volume_detects_external_changes_and_default_source_hotplug() {
    let backend = Arc::new(MockVolumeBackend::new(0, false, Some("alsa_input.1")));
    let volume = Arc::new(AtomicU8::new(0));
    let manual = Arc::new(AtomicBool::new(true));
    let mut guard = ClipGuard::new(None, Arc::clone(&volume))
        .with_controls(None, Arc::clone(&manual))
        .with_backend(backend.clone(), true);

    assert!(!guard.sync_os_volume());
    assert_eq!(volume.load(Ordering::Relaxed), 0);
    {
        let mut state = backend.state.lock().unwrap();
        state.pct = 100;
        state.resolved_name = Some("bluez_input.2".to_owned());
    }
    guard.last_os_sync = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(2))
        .unwrap();
    assert!(guard.sync_os_volume());
    assert_eq!(volume.load(Ordering::Relaxed), 100);
}

#[test]
fn os_mute_pauses_auto_gain_for_privacy() {
    let backend = Arc::new(MockVolumeBackend::new(40, true, Some("alsa_input.1")));
    let volume = Arc::new(AtomicU8::new(40));
    let manual = Arc::new(AtomicBool::new(false));
    let mut guard = ClipGuard::new(None, Arc::clone(&volume))
        .with_controls(Some(Arc::new(AtomicBool::new(false))), Arc::clone(&manual))
        .with_backend(backend.clone(), true);
    guard.config.enabled = true;
    assert!(!guard.sync_os_volume());
    assert!(guard.is_os_muted());

    let noise = vec![20i16; 320];
    for _ in 0..25 {
        guard.feed(&noise);
    }
    let quiet_speech = vec![500i16; 320];
    for _ in 0..20 {
        guard.feed(&quiet_speech);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 40);
    assert!(backend.state.lock().unwrap().muted);
}

#[test]
fn hot_speech_and_soft_clipping_reduce_capture_gain() {
    let volume = Arc::new(AtomicU8::new(80));
    let manual = Arc::new(AtomicBool::new(false));
    let mut guard =
        ClipGuard::new(None, Arc::clone(&volume)).with_controls(None, Arc::clone(&manual));
    guard.config.enabled = true;
    guard.last_step = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(10))
        .unwrap();

    let soft_clipped = vec![28800i16; 320];
    for _ in 0..10 {
        guard.feed(&soft_clipped);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 70);

    guard.last_step = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(2))
        .unwrap();
    let hot_speech = vec![10000i16; 320];
    for _ in 0..16 {
        guard.feed(&hot_speech);
    }
    assert_eq!(volume.load(Ordering::Relaxed), 65);
}

#[test]
fn re_enabling_auto_unmutes_and_restores_low_capture_floor_even_during_sync_throttle() {
    let backend = Arc::new(MockVolumeBackend::new(0, true, Some("alsa_input.1")));
    let volume = Arc::new(AtomicU8::new(0));
    let manual = Arc::new(AtomicBool::new(true));
    let mut guard = ClipGuard::new(None, Arc::clone(&volume))
        .with_controls(None, Arc::clone(&manual))
        .with_backend(backend.clone(), true);
    guard.config.enabled = true;

    // Observe Manual mode once, then re-enable Auto while the normal poll is throttled.
    let silence = vec![0i16; 320];
    guard.feed(&silence);
    manual.store(false, Ordering::Relaxed);
    if let Ok(mut last_write) = guard.last_vol_write.lock() {
        *last_write = std::time::Instant::now();
    }
    assert!(!guard.sync_os_volume());

    let state = backend.state.lock().unwrap();
    assert!(!state.muted);
    assert_eq!(state.pct, 50);
    assert_eq!(state.normalized_calls, 1);
    assert_eq!(volume.load(Ordering::Relaxed), 50);
}
