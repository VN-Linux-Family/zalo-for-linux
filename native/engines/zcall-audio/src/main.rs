//! zcall-audio: Native Linux low-latency audio engine for Zalo calls.
//!
//! Replaces python3 audio-io.py + parec + pacat subprocess pipeline with a single,
//! real-time native binary using PulseAudio Simple API and libopus directly.

mod apm;
mod audio_backend;
mod jitter_buffer;
mod level;
mod leveler;
mod opus_codec;
mod protocol;

use std::env;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use audio_backend::{pulse_get_source_volume, pulse_set_source_volume, PulsePlayer, PulseRecorder};
use jitter_buffer::JitterBuffer;
use level::compute_rfc6464_level;
use leveler::MicLeveler;
use opus_codec::OpusEncoder;
use protocol::*;

struct Config {
    mic_device: Option<String>,
    speaker_device: Option<String>,
    is_group: bool,
    bitrate_bps: i32,
    test_tone: bool,
    no_aec: bool,
    no_ns: bool,
    no_high_pass: bool,
    min_delay_ms: Option<i32>,
}

fn parse_args() -> Config {
    let mut mic_device = None;
    let mut speaker_device = None;
    let mut is_group = false;
    let mut bitrate_bps = 32000;
    let mut test_tone = false;
    let mut no_aec = false;
    let mut no_ns = false;
    let mut no_high_pass = false;
    let mut min_delay_ms = None;
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
            "--no-aec" => {
                no_aec = true;
            }
            "--no-ns" => {
                no_ns = true;
            }
            "--no-high-pass" => {
                no_high_pass = true;
            }
            "--min-delay" => {
                if i + 1 < args.len() {
                    if let Ok(d) = args[i + 1].parse::<i32>() {
                        min_delay_ms = Some(d);
                    }
                    i += 1;
                }
            }
            "--max-delay" => {
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
        no_aec,
        no_ns,
        no_high_pass,
        min_delay_ms,
    }
}
fn get_mic_volume_opt(source: Option<&str>) -> Option<u8> {
    if let Some(pct) = pulse_get_source_volume(source) {
        return Some(pct);
    }
    let source_name = source
        .filter(|s| !s.is_empty())
        .unwrap_or("@DEFAULT_SOURCE@");
    let output = std::process::Command::new("pactl")
        .env("LC_ALL", "C")
        .args(&["get-source-volume", source_name])
        .output();
    if let Ok(out) = output {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout);
            if let Some(pos) = s.find('%') {
                let prefix = &s[..pos];
                if let Some(slash_pos) = prefix.rfind('/') {
                    let num_str = prefix[slash_pos + 1..].trim();
                    if let Ok(val) = num_str.parse::<u16>() {
                        return Some(val.min(150) as u8);
                    }
                }
            }
        }
    }
    None
}

fn set_mic_volume(source: Option<&str>, pct: u8) -> bool {
    let pct = pct.min(100);
    if pulse_set_source_volume(source, pct).is_ok() {
        return true;
    }
    let source_name = source
        .filter(|s| !s.is_empty())
        .unwrap_or("@DEFAULT_SOURCE@");
    let raw = (65536.0 * (pct as f64) / 100.0).round() as u32;
    if let Ok(status) = std::process::Command::new("pactl")
        .env("LC_ALL", "C")
        .args(&["set-source-volume", source_name, &raw.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        return status.success();
    }
    false
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

/// ClipGuard monitors raw captured PCM samples for near-clipping (>= 30000) and
/// sustained quiet near-end speech (< -28 dBFS), adjusting hardware source volume
/// bidirectionally (35%..85%) unless disabled via ZCALL_MIC_AGC=0 or manual slider use.
pub struct ClipGuard {
    device: Option<String>,
    current_vol: Arc<AtomicU8>,
    far_active: Option<Arc<AtomicBool>>,
    manual: Arc<AtomicBool>,
    enabled: bool,
    apply_os: bool,
    clipped_frames_in_window: u8,
    window_frame_count: u8,
    last_step: std::time::Instant,
    last_clip: std::time::Instant,
    levels: [f32; 250],
    level_idx: usize,
    level_count: usize,
    noise_db: f32,
    quiet_speech_frames: u16,
}

impl ClipGuard {
    pub fn new(device: Option<String>, current_vol: Arc<AtomicU8>) -> Self {
        let enabled = env::var("ZCALL_MIC_AGC").map(|v| v != "0").unwrap_or(true);
        let past = std::time::Instant::now() - std::time::Duration::from_secs(10);
        Self {
            device,
            current_vol,
            far_active: None,
            manual: Arc::new(AtomicBool::new(false)),
            enabled,
            apply_os: cfg!(not(test)),
            clipped_frames_in_window: 0,
            window_frame_count: 0,
            last_step: std::time::Instant::now(),
            last_clip: past,
            levels: [-70.0; 250],
            level_idx: 0,
            level_count: 0,
            noise_db: -70.0,
            quiet_speech_frames: 0,
        }
    }

    pub fn with_controls(
        mut self,
        far_active: Option<Arc<AtomicBool>>,
        manual: Arc<AtomicBool>,
    ) -> Self {
        self.far_active = far_active;
        self.manual = manual;
        self
    }

    pub fn set_device(&mut self, device: Option<String>) {
        self.device = device;
        self.clipped_frames_in_window = 0;
        self.window_frame_count = 0;
        self.level_idx = 0;
        self.level_count = 0;
        self.noise_db = -70.0;
        self.quiet_speech_frames = 0;
    }

    fn apply_step(&mut self, new_vol: u8) {
        if self.apply_os {
            if !set_mic_volume(self.device.as_deref(), new_vol) {
                eprintln!(
                    "[audio-io] warn: failed to auto-adjust mic volume to {}%",
                    new_vol
                );
                self.last_step = std::time::Instant::now();
                return;
            }
            let actual = get_mic_volume_opt(self.device.as_deref())
                .map(|v| v.min(100))
                .unwrap_or(new_vol);
            self.current_vol.store(actual, Ordering::Relaxed);
            eprintln!("[audio-io] mic volume: {}%", actual);
        } else {
            self.current_vol.store(new_vol, Ordering::Relaxed);
            eprintln!("[audio-io] mic volume: {}%", new_vol);
        }
        self.last_step = std::time::Instant::now();
    }

    pub fn feed(&mut self, raw_samples: &[i16]) {
        let has_clip = raw_samples.iter().any(|&s| s.abs() >= 30000);
        if has_clip {
            self.clipped_frames_in_window += 1;
            self.last_clip = std::time::Instant::now();
            self.quiet_speech_frames = 0;
        }
        self.window_frame_count += 1;

        let level_db = MicLeveler::compute_level_db(raw_samples);
        self.levels[self.level_idx] = level_db;
        self.level_idx = (self.level_idx + 1) % self.levels.len();
        if self.level_count < self.levels.len() {
            self.level_count += 1;
        }
        if self.level_count >= 20 && (self.level_idx % 10 == 0 || self.level_count == 20) {
            let mut sorted = self.levels[..self.level_count].to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            self.noise_db = sorted[self.level_count / 10];
        }

        let agc_active = self.enabled && !self.manual.load(Ordering::Relaxed);

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
            .map(|f| f.load(Ordering::Relaxed))
            .unwrap_or(false);
        if far_is_talking {
            return;
        }

        if self.level_count >= 20
            && level_db > -58.0
            && level_db > self.noise_db + 9.0
        {
            if level_db < -28.0 {
                self.quiet_speech_frames = self.quiet_speech_frames.saturating_add(1);
            } else if level_db >= -24.0 {
                self.quiet_speech_frames = self.quiet_speech_frames.saturating_sub(2);
            }
        }

        if self.quiet_speech_frames >= 25
            && self.last_clip.elapsed() >= std::time::Duration::from_secs(3)
            && self.last_step.elapsed() >= std::time::Duration::from_millis(1500)
        {
            let cur = self.current_vol.load(Ordering::Relaxed);
            if cur < 85 {
                let new_vol = cur.saturating_add(5).min(85);
                self.apply_step(new_vol);
            }
            self.quiet_speech_frames = 0;
        }
    }
}

fn probe_hardware(speaker: Option<&str>, mic: Option<&str>) {
    let sink_name = speaker.unwrap_or("@DEFAULT_SINK@");
    let source_name = mic.unwrap_or("@DEFAULT_SOURCE@");
    eprintln!(
        "[audio-io] hw: audio backend=PulseAudio/PipeWire, stream_rate={}Hz, channels={}",
        SAMPLE_RATE, CHANNELS
    );
    eprintln!(
        "[audio-io] hw: target speaker='{}', mic='{}'",
        sink_name, source_name
    );

    // Detect Bluetooth sinks/sources
    if let Ok(out) = std::process::Command::new("pactl")
        .args(&["list", "sinks", "short"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("bluez") {
                eprintln!("[audio-io] hw: detected bluetooth sink: {}", line);
            }
        }
    }
    if let Ok(out) = std::process::Command::new("pactl")
        .args(&["list", "sources", "short"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("bluez") {
                eprintln!("[audio-io] hw: detected bluetooth source: {}", line);
                if line.contains("8000Hz") || line.contains("16000Hz") {
                    eprintln!(
                        "[audio-io] hw: note: bluetooth headset using HFP profile (narrowband/wideband speech)"
                    );
                }
            }
        }
    }
}

fn main() {
    let config = parse_args();
    eprintln!(
        "[audio-io] Starting native zcall-audio engine (bitrate: {} bps, group: {})",
        config.bitrate_bps, config.is_group
    );
    probe_hardware(config.speaker_device.as_deref(), config.mic_device.as_deref());

    let is_muted = Arc::new(AtomicBool::new(false));
    let speaker_off = Arc::new(AtomicBool::new(false));
    let far_active = Arc::new(AtomicBool::new(false));
    let manual_mic_vol = Arc::new(AtomicBool::new(false));
    let agc_enabled = env::var("ZCALL_MIC_AGC").map(|v| v != "0").unwrap_or(true);
    let leveler_enabled = env::var("ZCALL_MIC_GAIN").map(|v| v != "0").unwrap_or(true);

    let active_mic: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(config.mic_device.clone()));
    let mic_switch: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));

    let mic_volume_val = if config.test_tone {
        get_mic_volume_opt(config.mic_device.as_deref())
            .map(|v| v.min(100))
            .unwrap_or(100)
    } else {
        match get_mic_volume_opt(config.mic_device.as_deref()) {
            Some(initial_mic_vol) => {
                if agc_enabled && initial_mic_vol > 50 {
                    if set_mic_volume(config.mic_device.as_deref(), 40) {
                        get_mic_volume_opt(config.mic_device.as_deref())
                            .map(|v| v.min(100))
                            .unwrap_or(40)
                    } else {
                        eprintln!("[audio-io] warn: could not set initial mic volume to 40%");
                        initial_mic_vol.min(100)
                    }
                } else {
                    initial_mic_vol.min(100)
                }
            }
            None => {
                eprintln!("[audio-io] warn: could not query initial mic volume");
                100
            }
        }
    };
    let mic_volume = Arc::new(AtomicU8::new(mic_volume_val));
    eprintln!("[audio-io] mic volume: {}%", mic_volume_val);
    let running = Arc::new(AtomicBool::new(true));

    // Initialize Embedded WebRTC AudioProcessing Module (AEC, Noise Suppression, High Pass Filter)
    let aec_on = !config.no_aec;
    let ns_on = !config.no_ns;
    let hpf_on = !config.no_high_pass;
    let apm_instance = match apm::Apm::new(
        SAMPLE_RATE as i32,
        CHANNELS as i32,
        aec_on,
        ns_on,
        hpf_on,
    ) {
        Ok(a) => {
            eprintln!(
                "[audio-io] Embedded WebRTC AudioProcessing enabled (AEC: {}, NS: {}, HPF: {})",
                aec_on, ns_on, hpf_on
            );
            Some(Arc::new(a))
        }
        Err(e) => {
            eprintln!(
                "[audio-io] warning: WebRTC APM unavailable ({}); continuing without embedded AEC",
                e
            );
            None
        }
    };

    if let (Some(a), Some(d)) = (&apm_instance, config.min_delay_ms) {
        a.set_delay_ms(d);
    }

    // 1. Microphone capture thread
    let mic_running = Arc::clone(&running);
    let mic_muted = Arc::clone(&is_muted);
    let mic_vol = Arc::clone(&mic_volume);
    let mic_manual = Arc::clone(&manual_mic_vol);
    let mic_far_active = Arc::clone(&far_active);
    let mic_apm = apm_instance.as_ref().map(Arc::clone);
    let mic_dev = config.mic_device.clone();
    let cap_mic_switch = Arc::clone(&mic_switch);
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

        let leveler_far_active = Arc::clone(&mic_far_active);
        let mut guard = ClipGuard::new(mic_dev.clone(), Arc::clone(&mic_vol))
            .with_controls(Some(mic_far_active), Arc::clone(&mic_manual));
        let mut leveler = MicLeveler::new(Some(Arc::clone(&leveler_far_active)));
        while mic_running.load(Ordering::Relaxed) {
            if !tone_mode {
                if let Ok(mut req) = cap_mic_switch.lock() {
                    if let Some(new_dev) = req.take() {
                        match PulseRecorder::new(new_dev.as_deref()) {
                            Ok(new_rec) => {
                                recorder = new_rec;
                                guard.set_device(new_dev);
                                leveler = MicLeveler::new(Some(Arc::clone(&leveler_far_active)));
                            }
                            Err(e) => {
                                eprintln!("[audio-io] warn: mic switch failed: {}", e);
                            }
                        }
                    }
                }
            }
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
                    // ClipGuard must inspect raw capture samples before WebRTC APM / leveler.
                    guard.feed(samples);
                    if let Some(apm) = &mic_apm {
                        apm.process_capture(samples);
                    }
                    if leveler_enabled {
                        leveler.process(samples);
                    }
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

    // 2. Playback thread (Decoupled with 20 ms steady clock & buffer monitoring)
    let playout_running = Arc::clone(&running);
    let playout_speaker_off = Arc::clone(&speaker_off);
    let playout_far_active = Arc::clone(&far_active);
    let playout_apm = apm_instance.as_ref().map(Arc::clone);
    let target_frames = config
        .min_delay_ms
        .map(|d| (d / 20).clamp(2, 4) as usize)
        .unwrap_or(3);
    let playout_jb = Arc::new(Mutex::new(
        match JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10) {
            Ok(mut jb) => {
                jb.set_min_target(target_frames);
                jb
            }
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
    let playout_mic_vol = Arc::clone(&mic_volume);
    let speaker_switch: Arc<Mutex<Option<Option<String>>>> = Arc::new(Mutex::new(None));
    let playout_speaker_switch = Arc::clone(&speaker_switch);
    let mut speaker_dev = config.speaker_device.clone();

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
        let mut last_far_speech = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let mut last_diag = std::time::Instant::now();
        let mut last_reopen_attempt = std::time::Instant::now();
        let mut write_block_accum_ms = 0u64;
        let mut write_block_count = 0u32;
        let mut max_write_block_ms = 0u64;
        let mut non_blocking_streak = 0u32;

        while playout_running.load(Ordering::Relaxed) {
            let loop_start = std::time::Instant::now();
            let off = playout_speaker_off.load(Ordering::Relaxed);

            if let Ok(mut req) = playout_speaker_switch.lock() {
                if let Some(new_dev) = req.take() {
                    speaker_dev = new_dev;
                    if let Ok(p) = PulsePlayer::new(speaker_dev.as_deref()) {
                        player = Some(p);
                        non_blocking_streak = 0;
                    }
                }
            }

            if player.is_none() && last_reopen_attempt.elapsed() >= std::time::Duration::from_millis(500) {
                last_reopen_attempt = std::time::Instant::now();
                if let Ok(p) = PulsePlayer::new(speaker_dev.as_deref()) {
                    eprintln!("[audio-io] info: speaker stream recovered");
                    player = Some(p);
                    non_blocking_streak = 0;
                }
            }

            let mut write_ok = false;

            if let Some(ply) = &mut player {
                let write_elapsed: u64;
                let res = if off {
                    let t0 = std::time::Instant::now();
                    let r = ply.write(&silence);
                    write_elapsed = t0.elapsed().as_millis() as u64;
                    r
                } else {
                    let has_frame = {
                        if let Ok(mut jb) = playout_jb_thread.lock() {
                            jb.pop_frame(&mut pcm_out)
                        } else {
                            false
                        }
                    };
                    if has_frame {
                        if let Some(apm) = &playout_apm {
                            apm.process_render(&pcm_out);
                        }

                        let sum_sq: i64 = pcm_out.iter().map(|&v| (v as i64) * (v as i64)).sum();
                        let mean_sq = sum_sq / FRAME_SAMPLES as i64;
                        if mean_sq > 100_000 {
                            last_far_speech = std::time::Instant::now();
                            playout_far_active.store(true, Ordering::Relaxed);
                        }
                        let bytes_len = FRAME_SAMPLES * 2;
                        let pcm_bytes = unsafe {
                            std::slice::from_raw_parts(pcm_out.as_ptr() as *const u8, bytes_len)
                        };
                        let t0 = std::time::Instant::now();
                        let r = ply.write(pcm_bytes);
                        write_elapsed = t0.elapsed().as_millis() as u64;
                        r
                    } else {
                        let t0 = std::time::Instant::now();
                        let r = ply.write(&silence);
                        write_elapsed = t0.elapsed().as_millis() as u64;
                        r
                    }
                };

                if !off && last_far_speech.elapsed() >= std::time::Duration::from_millis(300) {
                    playout_far_active.store(false, Ordering::Relaxed);
                }

                match res {
                    Ok(()) => {
                        write_ok = true;
                        write_block_accum_ms += write_elapsed;
                        write_block_count += 1;
                        if write_elapsed > max_write_block_ms {
                            max_write_block_ms = write_elapsed;
                        }
                        if write_elapsed == 0 {
                            non_blocking_streak = non_blocking_streak.saturating_add(1);
                        } else {
                            non_blocking_streak = 0;
                        }
                    }
                    Err(e) => {
                        eprintln!("[audio-io] warn: speaker write failed ({}), re-opening stream", e);
                        player = None;
                        last_reopen_attempt = std::time::Instant::now();
                    }
                }
            } else {
                // Drain jitter buffer while speaker device is unavailable so frames do not pile up
                if let Ok(mut jb) = playout_jb_thread.lock() {
                    let _ = jb.pop_frame(&mut pcm_out);
                }
            }

            // Periodic diagnostic telemetry every 5 seconds
            if last_diag.elapsed() >= std::time::Duration::from_secs(5) {
                if let Ok(jb) = playout_jb_thread.lock() {
                    let stats = jb.stats();
                    let q_len = jb.queue_len();
                    let target = jb.min_target();
                    let avg_write = if write_block_count > 0 {
                        write_block_accum_ms / (write_block_count as u64)
                    } else {
                        0
                    };
                    let pa_lat = player.as_ref().and_then(|p| p.latency_ms()).unwrap_or(0);
                    let cur_mic_vol = playout_mic_vol.load(Ordering::Relaxed);
                    eprintln!(
                        "[audio-io] diag: playout [rx={} play={} q={}/{} plc={} late_drop={} under={} write_block={}ms pa_lat={}ms] mic [vol={}%]",
                        stats.rx_packets, stats.played_frames, q_len, target,
                        stats.plc_frames, stats.late_dropped, stats.underrun_count,
                        avg_write, pa_lat, cur_mic_vol
                    );
                }
                last_diag = std::time::Instant::now();
                write_block_accum_ms = 0;
                write_block_count = 0;
                max_write_block_ms = 0;
            }

            // Soundcard hardware clock pacing (matches audio-io.py PulseOut):
            // pa_simple_write blocks until the soundcard consumes 20ms of audio after the
            // initial 60ms (3-frame) buffer fills. Only sleep if player is unavailable
            // or if a virtual/dummy sink is non-blocking for > 6 consecutive frames.
            if !write_ok || non_blocking_streak > 6 {
                let elapsed = loop_start.elapsed();
                if elapsed < tick {
                    thread::sleep(tick - elapsed);
                }
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
                let ssrc = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
                let len = ((hdr[8] as usize) << 8) | (hdr[9] as usize);
                let mut opus_data = vec![0u8; len];
                if stdin.read_exact(&mut opus_data).is_err() {
                    break;
                }

                if let Ok(mut jb) = playout_jb.lock() {
                    jb.push_with_ssrc(idx, ssrc, opus_data);
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
                manual_mic_vol.store(true, Ordering::Relaxed);
                let dev = active_mic.lock().ok().and_then(|g| g.clone());
                if !config.test_tone && set_mic_volume(dev.as_deref(), pct) {
                    let actual = get_mic_volume_opt(dev.as_deref())
                        .map(|v| v.min(100))
                        .unwrap_or(pct);
                    mic_volume.store(actual, Ordering::Relaxed);
                    eprintln!("[audio-io] mic volume: {}%", actual);
                } else {
                    if !config.test_tone {
                        eprintln!("[audio-io] warn: failed to set mic volume to {}%", pct);
                    }
                    mic_volume.store(pct, Ordering::Relaxed);
                    eprintln!("[audio-io] mic volume: {}%", pct);
                }
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
                    let new_dev = if name.is_empty() {
                        None
                    } else {
                        Some(name.into_owned())
                    };
                    if let Ok(mut am) = active_mic.lock() {
                        *am = new_dev.clone();
                    }
                    if let Ok(mut req) = mic_switch.lock() {
                        *req = Some(new_dev.clone());
                    }
                    let new_vol = match get_mic_volume_opt(new_dev.as_deref()) {
                        Some(v) if agc_enabled && !manual_mic_vol.load(Ordering::Relaxed) && v > 50 => {
                            if set_mic_volume(new_dev.as_deref(), 40) {
                                get_mic_volume_opt(new_dev.as_deref())
                                    .map(|x| x.min(100))
                                    .unwrap_or(40)
                            } else {
                                v.min(100)
                            }
                        }
                        Some(v) => v.min(100),
                        None => 100,
                    };
                    mic_volume.store(new_vol, Ordering::Relaxed);
                    eprintln!("[audio-io] mic volume: {}%", new_vol);
                } else if hdr[0] == b'o' {
                    let new_dev = if name.is_empty() {
                        None
                    } else {
                        Some(name.into_owned())
                    };
                    if let Ok(mut req) = speaker_switch.lock() {
                        *req = Some(new_dev);
                    }
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

    #[test]
    fn test_clip_guard_auto_raise_quiet_speech() {
        let vol = Arc::new(AtomicU8::new(40));
        let far_active = Arc::new(AtomicBool::new(false));
        let manual = Arc::new(AtomicBool::new(false));
        let mut guard = ClipGuard::new(None, Arc::clone(&vol))
            .with_controls(Some(Arc::clone(&far_active)), Arc::clone(&manual));
        guard.enabled = true;
        guard.last_step = std::time::Instant::now() - std::time::Duration::from_millis(2000);

        // Learn quiet noise floor (~ -64 dBFS -> amplitude ~ 20)
        let noise = vec![20i16; 320];
        for _ in 0..25 {
            guard.feed(&noise);
        }

        // Far-end talking must hold auto-raise
        far_active.store(true, Ordering::Relaxed);
        let quiet_speech = vec![500i16; 320]; // ~ -36 dBFS
        for _ in 0..30 {
            guard.feed(&quiet_speech);
        }
        assert_eq!(vol.load(Ordering::Relaxed), 40);

        // Near-end quiet speech must step volume up by +5%
        far_active.store(false, Ordering::Relaxed);
        for _ in 0..30 {
            guard.feed(&quiet_speech);
        }
        assert_eq!(vol.load(Ordering::Relaxed), 45);

        // Manual slider lock stops further auto-adjustments
        manual.store(true, Ordering::Relaxed);
        guard.last_step = std::time::Instant::now() - std::time::Duration::from_millis(2000);
        for _ in 0..30 {
            guard.feed(&quiet_speech);
        }
        assert_eq!(vol.load(Ordering::Relaxed), 45);
    }
}
