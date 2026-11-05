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
use std::sync::Arc;
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

fn main() {
    let config = parse_args();
    eprintln!(
        "[audio-io] Starting native zcall-audio engine (bitrate: {} bps, group: {})",
        config.bitrate_bps, config.is_group
    );

    let is_muted = Arc::new(AtomicBool::new(false));
    let speaker_off = Arc::new(AtomicBool::new(false));
    let mic_volume = Arc::new(AtomicU8::new(100));
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
                let vol = mic_vol.load(Ordering::Relaxed);
                if vol < 100 {
                    let scale = vol as f32 / 100.0;
                    for s in samples.iter_mut() {
                        *s = (*s as f32 * scale) as i16;
                    }
                }
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
                    if out.write_all(&hdr).is_err() || out.write_all(&opus_buf[..len]).is_err() || out.flush().is_err() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[audio-io] Opus encode error: {}", e);
                }
            }
        }
    });

    // 2. Playback & Inbound IPC loop (Main thread)
    let mut player = match PulsePlayer::new(config.speaker_device.as_deref()) {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("[audio-io] fault: speaker: {}", e);
            None
        }
    };

    let mut jitter_buffer = match JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10) {
        Ok(jb) => Some(jb),
        Err(e) => {
            eprintln!("[audio-io] fault: speaker: jitter buffer init failed: {}", e);
            None
        }
    };

    let mut stdin = io::stdin();
    let mut pcm_out = vec![0i16; FRAME_SAMPLES];
    let silence = vec![0u8; FRAME_BYTES];

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

                let off = speaker_off.load(Ordering::Relaxed);
                if let (Some(jb), Some(ply)) = (&mut jitter_buffer, &mut player) {
                    if off {
                        let _ = ply.write(&silence);
                    } else {
                        jb.push(idx, opus_data);
                        while jb.pop_frame(&mut pcm_out) {
                            let bytes_len = FRAME_SAMPLES * 2;
                            let pcm_bytes = unsafe {
                                std::slice::from_raw_parts(pcm_out.as_ptr() as *const u8, bytes_len)
                            };
                            let _ = ply.write(pcm_bytes);
                            if !jb.is_ready() {
                                break;
                            }
                        }
                    }
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
                let pct = arg[0];
                mic_volume.store(pct, Ordering::Relaxed);
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
                let kind_str = if hdr[0] == b'i' { "microphone" } else { "speaker" };
                eprintln!("[audio-io] Requested {} device change to: {}", kind_str, name);
            }
            _ => {
                // Unknown command tag, ignore or resync
            }
        }
    }

    running.store(false, Ordering::Relaxed);
    let _ = mic_handle.join();
    eprintln!("[audio-io] zcall-audio shutdown complete.");
}
