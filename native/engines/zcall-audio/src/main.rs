//! zcall-audio: Native Linux low-latency audio engine for Zalo calls.
//!
//! Replaces python3 audio-io.py + parec + pacat subprocess pipeline with a single,
//! real-time native binary using PulseAudio Simple API and libopus directly.

mod audio_backend;
mod jitter_buffer;
mod level;
mod opus_codec;
mod protocol;

use std::env;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use audio_backend::{PulsePlayer, PulseRecorder};
use jitter_buffer::JitterBuffer;
use level::compute_rfc6464_level;
use opus_codec::OpusEncoder;
use protocol::*;

struct Config {
    mic_device: Option<String>,
    speaker_device: Option<String>,
    is_group: bool,
    bitrate_bps: i32,
    test_tone: bool,
}

fn parse_args() -> Config {
    let mut mic_device = None;
    let mut speaker_device = None;
    let mut is_group = false;
    let mut bitrate_bps = 32000;
    let mut test_tone = false;

    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mic" => {
                if i + 1 < args.len() {
                    mic_device = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--speaker" => {
                if i + 1 < args.len() {
                    speaker_device = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--mix" => {
                is_group = true;
                bitrate_bps = 20000;
            }
            "--bitrate" => {
                if i + 1 < args.len() {
                    if let Ok(b) = args[i + 1].parse::<i32>() {
                        bitrate_bps = b * 1000;
                    }
                    i += 1;
                }
            }
            "--tone" => {
                test_tone = true;
            }
            "--min-delay" | "--max-delay" => {
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    Config {
        mic_device,
        speaker_device,
        is_group,
        bitrate_bps,
        test_tone,
    }
}
fn get_mic_volume(source: Option<&str>) -> u8 {
    let source_name = source.unwrap_or("@DEFAULT_SOURCE@");
    let output = std::process::Command::new("pactl")
        .args(&["get-source-volume", source_name])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(pos) = s.find('%') {
                let prefix = &s[..pos];
                if let Some(slash_pos) = prefix.rfind('/') {
                    let num_str = prefix[slash_pos + 1..].trim();
                    if let Ok(val) = num_str.parse::<u8>() {
                        return val.min(100);
                    }
                }
            }
        }
    }
    100
}

fn set_mic_volume(source: Option<&str>, pct: u8) {
    let source_name = source.unwrap_or("@DEFAULT_SOURCE@");
    let pct = pct.min(100);
    let raw = (65536.0 * (pct as f64) / 100.0).round() as u32;
    let _ = std::process::Command::new("pactl")
        .args(&["set-source-volume", source_name, &raw.to_string()])
        .spawn();
}

/// Apply smooth polynomial soft-saturation to prevent audio clipping.
/// For samples within [-20000, 20000], signal is untouched (strictly linear).
/// For samples exceeding 20000, a smooth C1 cubic curve compresses peaks asymptotically
/// towards 32000, completely eliminating harsh square-wave clipping.
pub fn apply_soft_limiter(samples: &mut [i16]) {
    const THRESHOLD: f32 = 20000.0;
    const HEADROOM: f32 = 18000.0; // 20000 + 18000 * (1 - 1/3) = 32000

    for s in samples.iter_mut() {
        let val = *s as f32;
        let abs_val = val.abs();
        if abs_val > THRESHOLD {
            let sign = if val > 0.0 { 1.0 } else { -1.0 };
            let u = ((abs_val - THRESHOLD) / HEADROOM).min(1.0);
            let compressed = THRESHOLD + HEADROOM * (u - (u * u * u) / 3.0);
            *s = (sign * compressed).clamp(-32767.0, 32767.0) as i16;
        }
    }
}

/// ClipGuard monitors input PCM samples for near-clipping (>= 30000).
/// When excessive clipping occurs in a 200ms window (>= 3 out of 10 frames),
/// it automatically steps down pactl hardware source volume by 5% (floor 50%).
pub struct ClipGuard {
    device: Option<String>,
    current_vol: Arc<AtomicU8>,
    clipped_frames_in_window: u8,
    window_frame_count: u8,
    last_step: std::time::Instant,
}

impl ClipGuard {
    pub fn new(device: Option<String>, current_vol: Arc<AtomicU8>) -> Self {
        Self {
            device,
            current_vol,
            clipped_frames_in_window: 0,
            window_frame_count: 0,
            last_step: std::time::Instant::now(),
        }
    }

    pub fn feed(&mut self, raw_samples: &[i16]) {
        let has_clip = raw_samples.iter().any(|&s| s.abs() >= 30000);
        if has_clip {
            self.clipped_frames_in_window += 1;
        }
        self.window_frame_count += 1;

        if self.window_frame_count >= 10 {
            if self.clipped_frames_in_window >= 3
                && self.last_step.elapsed() >= std::time::Duration::from_millis(400)
            {
                let cur = self.current_vol.load(Ordering::Relaxed);
                if cur > 35 {
                    let new_vol = cur.saturating_sub(10).max(35);
                    self.current_vol.store(new_vol, Ordering::Relaxed);
                    set_mic_volume(self.device.as_deref(), new_vol);
                    eprintln!("[audio-io] mic volume: {}%", new_vol);
                    self.last_step = std::time::Instant::now();
                }
            }
            self.clipped_frames_in_window = 0;
            self.window_frame_count = 0;
        }
    }
}

fn main() {
    let config = parse_args();
    eprintln!(
        "[audio-io] Starting native zcall-audio engine (bitrate: {} bps, group: {})",
        config.bitrate_bps, config.is_group
    );

    let is_muted = Arc::new(AtomicBool::new(false));
    let speaker_off = Arc::new(AtomicBool::new(false));
    let initial_mic_vol = get_mic_volume(config.mic_device.as_deref());
    let mic_volume = Arc::new(AtomicU8::new(initial_mic_vol));
    eprintln!("[audio-io] mic volume: {}%", initial_mic_vol);
    let running = Arc::new(AtomicBool::new(true));

    // 1. Microphone capture thread
    let mic_running = Arc::clone(&running);
    let mic_muted = Arc::clone(&is_muted);
    let mic_vol = Arc::clone(&mic_volume);
    let mic_dev = config.mic_device.clone();
    let bitrate = config.bitrate_bps;
    let tone_mode = config.test_tone;

    let mic_handle = thread::spawn(move || {
        let mut recorder = match PulseRecorder::new(mic_dev.as_deref()) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("[audio-io] fault: mic: {}", e);
                return;
            }
        };

        let mut encoder = match OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, bitrate) {
            Ok(enc) => enc,
            Err(e) => {
                eprintln!("[audio-io] fault: mic: opus encoder init failed: {}", e);
                return;
            }
        };

        let mut pcm_raw = vec![0u8; FRAME_BYTES];
        let mut opus_buf = vec![0u8; 1024];
        let mut stdout = io::stdout();

        let mut tone_phase: f32 = 0.0;
        let tone_step = 2.0 * std::f32::consts::PI * 440.0 / SAMPLE_RATE as f32;

        let mut guard = ClipGuard::new(mic_dev.clone(), Arc::clone(&mic_vol));

        while mic_running.load(Ordering::Relaxed) {
            if tone_mode {
                // Generate 440 Hz test tone
                let samples_ptr = pcm_raw.as_mut_ptr() as *mut i16;
                for j in 0..FRAME_SAMPLES {
                    let val = (tone_phase.sin() * 10000.0) as i16;
                    unsafe { *samples_ptr.add(j) = val };
                    tone_phase = (tone_phase + tone_step) % (2.0 * std::f32::consts::PI);
                }
                thread::sleep(std::time::Duration::from_millis(20));
            } else {
                if let Err(e) = recorder.read(&mut pcm_raw) {
                    eprintln!("[audio-io] fault: mic read: {}", e);
                    break;
                }
            }

            let muted = mic_muted.load(Ordering::Relaxed);
            let samples = unsafe {
                std::slice::from_raw_parts_mut(pcm_raw.as_mut_ptr() as *mut i16, FRAME_SAMPLES)
            };

            if muted {
                for s in samples.iter_mut() {
                    *s = 0;
                }
            } else {
                if !tone_mode {
                    guard.feed(samples);
                }
                apply_soft_limiter(samples);
            }

            let level = compute_rfc6464_level(samples);

            match encoder.encode(samples, &mut opus_buf) {
                Ok(len) => {
                    let mut hdr = [0u8; 4];
                    hdr[0] = CMD_OUT_LEVEL; // 'L'
                    hdr[1] = level;
                    hdr[2] = (len >> 8) as u8;
                    hdr[3] = (len & 0xff) as u8;

                    let mut out = io::BufWriter::new(&mut stdout);
                    if out.write_all(&hdr).is_err()
                        || out.write_all(&opus_buf[..len]).is_err()
                        || out.flush().is_err()
                    {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[audio-io] Opus encode error: {}", e);
                }
            }
        }
    });

    // 2. Playback thread (Decoupled with 20 ms steady clock)
    let playout_running = Arc::clone(&running);
    let playout_speaker_off = Arc::clone(&speaker_off);
    let playout_jb = Arc::new(Mutex::new(
        match JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10) {
            Ok(jb) => jb,
            Err(e) => {
                eprintln!(
                    "[audio-io] fault: speaker: jitter buffer init failed: {}",
                    e
                );
                return;
            }
        },
    ));
    let playout_jb_thread = Arc::clone(&playout_jb);
    let speaker_dev = config.speaker_device.clone();

    let playout_handle = thread::spawn(move || {
        let mut player = match PulsePlayer::new(speaker_dev.as_deref()) {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("[audio-io] fault: speaker: {}", e);
                None
            }
        };

        let mut pcm_out = vec![0i16; FRAME_SAMPLES];
        let silence = vec![0u8; FRAME_BYTES];
        let tick = std::time::Duration::from_millis(20);
        let mut next_tick = std::time::Instant::now();

        while playout_running.load(Ordering::Relaxed) {
            let off = playout_speaker_off.load(Ordering::Relaxed);

            if let Some(ply) = &mut player {
                if off {
                    let _ = ply.write(&silence);
                } else {
                    let has_frame = {
                        if let Ok(mut jb) = playout_jb_thread.lock() {
                            jb.pop_frame(&mut pcm_out)
                        } else {
                            false
                        }
                    };

                    if has_frame {
                        let bytes_len = FRAME_SAMPLES * 2;
                        let pcm_bytes = unsafe {
                            std::slice::from_raw_parts(pcm_out.as_ptr() as *const u8, bytes_len)
                        };
                        let _ = ply.write(pcm_bytes);
                    } else {
                        let _ = ply.write(&silence);
                    }
                }
            }

            next_tick += tick;
            let now = std::time::Instant::now();
            if next_tick > now {
                thread::sleep(next_tick - now);
            } else {
                next_tick = now;
            }
        }
    });

    // 3. Inbound IPC loop (Main thread)
    let mut stdin = io::stdin();

    loop {
        let mut cmd_buf = [0u8; 1];
        if stdin.read_exact(&mut cmd_buf).is_err() {
            // EOF: stdin closed by Node process
            break;
        }

        match cmd_buf[0] {
            CMD_IN_FRAME => {
                let mut hdr = [0u8; 10];
                if stdin.read_exact(&mut hdr).is_err() {
                    break;
                }
                let idx = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                let len = ((hdr[8] as usize) << 8) | (hdr[9] as usize);
                let mut opus_data = vec![0u8; len];
                if stdin.read_exact(&mut opus_data).is_err() {
                    break;
                }

                if let Ok(mut jb) = playout_jb.lock() {
                    jb.push(idx, opus_data);
                }
            }
            CMD_IN_MUTE => {
                let mut arg = [0u8; 1];
                if stdin.read_exact(&mut arg).is_err() {
                    break;
                }
                is_muted.store(arg[0] != 0, Ordering::Relaxed);
            }
            CMD_IN_SPEAKER => {
                let mut arg = [0u8; 1];
                if stdin.read_exact(&mut arg).is_err() {
                    break;
                }
                speaker_off.store(arg[0] != 0, Ordering::Relaxed);
            }
            CMD_IN_VOLUME => {
                let mut arg = [0u8; 1];
                if stdin.read_exact(&mut arg).is_err() {
                    break;
                }
                let pct = arg[0].min(100);
                mic_volume.store(pct, Ordering::Relaxed);
                set_mic_volume(config.mic_device.as_deref(), pct);
                eprintln!("[audio-io] mic volume: {}%", pct);
            }
            CMD_IN_DEVICE => {
                let mut hdr = [0u8; 3]; // u8 kind, u16 len
                if stdin.read_exact(&mut hdr).is_err() {
                    break;
                }
                let len = ((hdr[1] as usize) << 8) | (hdr[2] as usize);
                let mut dev_name = vec![0u8; len];
                if stdin.read_exact(&mut dev_name).is_err() {
                    break;
                }
                let name = String::from_utf8_lossy(&dev_name);
                let kind_str = if hdr[0] == b'i' {
                    "microphone"
                } else {
                    "speaker"
                };
                eprintln!(
                    "[audio-io] Requested {} device change to: {}",
                    kind_str, name
                );
                if hdr[0] == b'i' {
                    let dev_str = if name.is_empty() {
                        None
                    } else {
                        Some(name.as_ref())
                    };
                    let new_vol = get_mic_volume(dev_str);
                    mic_volume.store(new_vol, Ordering::Relaxed);
                    eprintln!("[audio-io] mic volume: {}%", new_vol);
                }
            }
            _ => {
                // Unknown command tag, ignore or resync
            }
        }
    }

    running.store(false, Ordering::Relaxed);
    let _ = mic_handle.join();
    let _ = playout_handle.join();
    eprintln!("[audio-io] zcall-audio shutdown complete.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_soft_limiter_linear_region() {
        let mut samples = vec![0i16, 1000, -1000, 15000, -20000, 20000];
        let original = samples.clone();
        apply_soft_limiter(&mut samples);
        assert_eq!(samples, original);
    }

    #[test]
    fn test_soft_limiter_compression_bounds() {
        let mut samples = vec![25000, 30000, 32767, -25000, -30000, -32768];
        apply_soft_limiter(&mut samples);
        for &s in &samples {
            assert!(
                s.abs() <= 32000,
                "Compressed sample must not exceed 32000, got {}",
                s
            );
            assert!(
                s.abs() > 20000,
                "Compressed sample must exceed threshold 20000"
            );
        }
        // Positive full scale should be ~30626
        assert_eq!(samples[2], 30626);
        // Negative full scale should be ~-30626
        assert_eq!(samples[5], -30626);
    }

    #[test]
    fn test_clip_guard_stepping() {
        let vol = Arc::new(AtomicU8::new(100));
        let mut guard = ClipGuard::new(None, Arc::clone(&vol));
        // Artificially reset last_step to 1 second ago so it can step
        guard.last_step = std::time::Instant::now() - std::time::Duration::from_millis(1000);

        // Feed 10 frames with clipping (30000)
        let clipped_frame = vec![30500i16; 320];
        for _ in 0..10 {
            guard.feed(&clipped_frame);
        }

        // Must have stepped down by 10%
        assert_eq!(vol.load(Ordering::Relaxed), 90);

        // Test floor limit at 35%
        vol.store(42, Ordering::Relaxed);
        guard.last_step = std::time::Instant::now() - std::time::Duration::from_millis(1000);
        for _ in 0..10 {
            guard.feed(&clipped_frame);
        }
        assert_eq!(vol.load(Ordering::Relaxed), 35);

        // Feed again, must not go below 35
        guard.last_step = std::time::Instant::now() - std::time::Duration::from_millis(1000);
        for _ in 0..10 {
            guard.feed(&clipped_frame);
        }
        assert_eq!(vol.load(Ordering::Relaxed), 35);
    }
}
