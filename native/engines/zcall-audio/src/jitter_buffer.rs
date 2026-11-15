//! Audio Jitter Buffer and Packet Loss Concealment (PLC).

use crate::opus_codec::OpusDecoder;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JitterState {
    Buffering,
    Playing,
}

#[derive(Default, Debug, Clone)]
pub struct JitterStats {
    pub rx_packets: u64,
    pub played_frames: u64,
    pub plc_frames: u64,
    pub late_dropped: u64,
    pub underrun_count: u64,
}

pub struct JitterBuffer {
    decoder: OpusDecoder,
    queue: BTreeMap<u32, Vec<u8>>,
    next_idx: Option<u32>,
    min_target: usize,
    max_depth: usize,
    state: JitterState,
    consecutive_plc: usize,
    stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(sample_rate: i32, channels: i32, depth_frames: usize) -> Result<Self, String> {
        let decoder = OpusDecoder::new(sample_rate, channels)?;
        let min_target = 4.min(depth_frames).max(2);
        let max_depth = depth_frames.max(min_target + 4);
        Ok(Self {
            decoder,
            queue: BTreeMap::new(),
            next_idx: None,
            min_target,
            max_depth,
            state: JitterState::Buffering,
            consecutive_plc: 0,
            stats: JitterStats::default(),
        })
    }

    pub fn set_min_target(&mut self, target: usize) {
        self.min_target = target.max(2).min(self.max_depth);
    }

    pub fn min_target(&self) -> usize {
        self.min_target
    }

    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    #[allow(dead_code)]
    pub fn state(&self) -> JitterState {
        self.state
    }

    pub fn stats(&self) -> JitterStats {
        self.stats.clone()
    }

    pub fn push(&mut self, idx: u32, opus_data: Vec<u8>) {
        self.stats.rx_packets += 1;

        if let Some(next) = self.next_idx {
            // Drop late packets only when in Playing state and older than current playout pointer
            if self.state == JitterState::Playing && idx < next && (next - idx) < 1000 {
                self.stats.late_dropped += 1;
                eprintln!(
                    "[audio-io] warn: dropped late packet idx={} (current playout={})",
                    idx, next
                );
                return;
            }
        }

        self.queue.insert(idx, opus_data);

        // Prune queue if deeper than max_depth to prevent latency drift
        while self.queue.len() > self.max_depth {
            let oldest_key = *self.queue.keys().next().unwrap();
            self.queue.remove(&oldest_key);
        }

        // If currently buffering and target depth is reached, transition to Playing
        if self.state == JitterState::Buffering && self.queue.len() >= self.min_target {
            self.state = JitterState::Playing;
            if self.next_idx.is_none() {
                self.next_idx = self.queue.keys().next().copied();
            }
            eprintln!(
                "[audio-io] info: jitter buffer primed (depth={}), resuming playout",
                self.queue.len()
            );
        }
    }

    /// Decode next frame into out_pcm (320 samples).
    /// Returns true if valid frame or PLC generated.
    pub fn pop_frame(&mut self, out_pcm: &mut [i16]) -> bool {
        match self.state {
            JitterState::Buffering => {
                if self.queue.len() >= self.min_target {
                    self.state = JitterState::Playing;
                    self.next_idx = self.queue.keys().next().copied();
                    self.consecutive_plc = 0;
                    eprintln!(
                        "[audio-io] info: jitter buffer primed (depth={}), resuming playout",
                        self.queue.len()
                    );
                } else {
                    return false;
                }
            }
            JitterState::Playing => {}
        }

        if self.next_idx.is_none() {
            if let Some(&first) = self.queue.keys().next() {
                self.next_idx = Some(first);
            } else {
                self.state = JitterState::Buffering;
                return false;
            }
        }

        // Check if stream sequence jumped ahead by more than 20 frames (400ms)
        if let Some(&first) = self.queue.keys().next() {
            if let Some(cur) = self.next_idx {
                if first > cur && (first - cur) > 20 {
                    eprintln!(
                        "[audio-io] warn: sequence jump detected (expected {}, got {}), resyncing",
                        cur, first
                    );
                    self.next_idx = Some(first);
                    self.consecutive_plc = 0;
                }
            }
        }

        let cur_idx = self.next_idx.unwrap();
        self.next_idx = Some(cur_idx.wrapping_add(1));

        if let Some(payload) = self.queue.remove(&cur_idx) {
            self.consecutive_plc = 0;
            self.stats.played_frames += 1;
            let _ = self.decoder.decode(&payload, out_pcm, false);
            true
        } else {
            // Missing frame: run Opus Packet Loss Concealment (PLC) up to 5 consecutive frames
            if self.consecutive_plc < 5 {
                self.consecutive_plc += 1;
                self.stats.plc_frames += 1;
                let _ = self.decoder.decode(&[], out_pcm, false);
                true
            } else {
                // Buffer starvation: revert to Buffering mode to accumulate min_target frames again
                self.state = JitterState::Buffering;
                self.stats.underrun_count += 1;
                self.consecutive_plc = 0;
                self.next_idx = None;
                eprintln!(
                    "[audio-io] warn: jitter buffer starvation, entering buffering mode (target={} frames)",
                    self.min_target
                );
                false
            }
        }
    }

    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        self.queue.len() >= self.min_target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opus_codec::OpusEncoder;
    use crate::protocol::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE};

    fn generate_sine_frame(freq: f32, phase: &mut f32) -> Vec<i16> {
        let mut pcm = vec![0i16; FRAME_SAMPLES];
        let step = 2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32;
        for s in pcm.iter_mut() {
            *s = (phase.sin() * 15000.0) as i16;
            *phase = (*phase + step) % (2.0 * std::f32::consts::PI);
        }
        pcm
    }

    #[test]
    fn test_jitter_buffer_prebuffering_and_playout() {
        let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
        jb.set_min_target(3);
        let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

        let mut phase = 0.0;
        let mut opus_buf = vec![0u8; 1024];
        let mut out_pcm = vec![0i16; FRAME_SAMPLES];

        // Frame 0: still buffering, pop_frame returns false
        let pcm = generate_sine_frame(440.0, &mut phase);
        let len = enc.encode(&pcm, &mut opus_buf).unwrap();
        jb.push(0, opus_buf[..len].to_vec());
        assert_eq!(jb.state(), JitterState::Buffering);
        assert!(!jb.pop_frame(&mut out_pcm));

        // Frame 1: still buffering
        let pcm = generate_sine_frame(440.0, &mut phase);
        let len = enc.encode(&pcm, &mut opus_buf).unwrap();
        jb.push(1, opus_buf[..len].to_vec());
        assert_eq!(jb.state(), JitterState::Buffering);
        assert!(!jb.pop_frame(&mut out_pcm));

        // Frame 2: reaches min_target (3), transitions to Playing
        let pcm = generate_sine_frame(440.0, &mut phase);
        let len = enc.encode(&pcm, &mut opus_buf).unwrap();
        jb.push(2, opus_buf[..len].to_vec());
        assert_eq!(jb.state(), JitterState::Playing);

        // Now pop_frame succeeds for all 3 frames
        assert!(jb.pop_frame(&mut out_pcm));
        assert!(jb.pop_frame(&mut out_pcm));
        assert!(jb.pop_frame(&mut out_pcm));
    }

    #[test]
    fn test_jitter_buffer_out_of_order_and_plc() {
        let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
        jb.set_min_target(2);
        let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

        let mut phase = 0.0;
        let mut opus_buf = vec![0u8; 1024];

        // Generate frames 0, 1, 3 (frame 2 is missing, simulated packet loss)
        let pcm0 = generate_sine_frame(440.0, &mut phase);
        let len0 = enc.encode(&pcm0, &mut opus_buf).unwrap();
        let frame0 = opus_buf[..len0].to_vec();

        let pcm1 = generate_sine_frame(440.0, &mut phase);
        let len1 = enc.encode(&pcm1, &mut opus_buf).unwrap();
        let frame1 = opus_buf[..len1].to_vec();

        let pcm3 = generate_sine_frame(440.0, &mut phase);
        let len3 = enc.encode(&pcm3, &mut opus_buf).unwrap();
        let frame3 = opus_buf[..len3].to_vec();

        // Push out of order: 1 first, then 0, then 3
        jb.push(1, frame1);
        jb.push(0, frame0);
        jb.push(3, frame3);

        let mut out = vec![0i16; FRAME_SAMPLES];

        // Should pop frame 0
        assert!(jb.pop_frame(&mut out));
        // Should pop frame 1
        assert!(jb.pop_frame(&mut out));
        // Frame 2 is missing -> triggers Packet Loss Concealment (PLC), must still succeed
        assert!(jb.pop_frame(&mut out));
        // Should pop frame 3
        assert!(jb.pop_frame(&mut out));
    }

    #[test]
    fn test_jitter_buffer_starvation_and_rebuffering() {
        let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
        jb.set_min_target(2);
        let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

        let mut phase = 0.0;
        let mut opus_buf = vec![0u8; 1024];
        let mut out = vec![0i16; FRAME_SAMPLES];

        let pcm = generate_sine_frame(440.0, &mut phase);
        let len = enc.encode(&pcm, &mut opus_buf).unwrap();
        jb.push(0, opus_buf[..len].to_vec());
        jb.push(1, opus_buf[..len].to_vec());

        // Pop frames 0 and 1
        assert!(jb.pop_frame(&mut out));
        assert!(jb.pop_frame(&mut out));

        // Next 5 missing frames -> PLC 1..5 -> true
        for _ in 0..5 {
            assert!(jb.pop_frame(&mut out));
        }

        // 6th missing frame -> exceeds PLC limit -> starvation!
        // Should return false and transition back to Buffering state
        assert!(!jb.pop_frame(&mut out));
        assert_eq!(jb.state(), JitterState::Buffering);
        assert_eq!(jb.stats().underrun_count, 1);

        // New packets arrive: must buffer min_target (2) before resuming playout
        jb.push(50, opus_buf[..len].to_vec());
        assert_eq!(jb.state(), JitterState::Buffering);
        assert!(!jb.pop_frame(&mut out));

        jb.push(51, opus_buf[..len].to_vec());
        assert_eq!(jb.state(), JitterState::Playing);
        assert!(jb.pop_frame(&mut out));
        assert!(jb.pop_frame(&mut out));
    }
}
