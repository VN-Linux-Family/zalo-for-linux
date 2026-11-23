//! Audio Jitter Buffer and Packet Loss Concealment (PLC).

use super::codec::OpusDecoder;
use std::collections::{BTreeMap, VecDeque};

const MAX_OPUS_FRAME_SAMPLES: usize = 1920; // 120 ms at 16 kHz (maximum Opus packet duration)

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
    decode_scratch: Vec<i16>,
    carry_pcm: VecDeque<i16>,
    next_idx: Option<u32>,
    current_ssrc: Option<u32>,
    min_target: usize,
    max_depth: usize,
    state: JitterState,
    consecutive_plc: usize,
    stats: JitterStats,
}

impl JitterBuffer {
    pub fn new(sample_rate: i32, channels: i32, depth_frames: usize) -> Result<Self, String> {
        let decoder = OpusDecoder::new(sample_rate, channels)?;
        let min_target = 3.min(depth_frames).max(2);
        let max_depth = depth_frames.max(min_target + 4);
        Ok(Self {
            decoder,
            queue: BTreeMap::new(),
            decode_scratch: vec![0i16; MAX_OPUS_FRAME_SAMPLES],
            carry_pcm: VecDeque::with_capacity(MAX_OPUS_FRAME_SAMPLES),
            next_idx: None,
            current_ssrc: None,
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

    #[must_use]
    pub const fn min_target(&self) -> usize {
        self.min_target
    }

    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
    }

    #[allow(dead_code)]
    #[must_use]
    pub const fn state(&self) -> JitterState {
        self.state
    }

    #[must_use]
    pub fn stats(&self) -> JitterStats {
        self.stats.clone()
    }

    pub fn push_with_ssrc(&mut self, idx: u32, ssrc: u32, opus_data: Vec<u8>) {
        if ssrc != 0 {
            if let Some(cur_ssrc) = self.current_ssrc {
                if cur_ssrc != ssrc {
                    eprintln!(
                        "[audio-io] info: stream ssrc changed ({cur_ssrc} -> {ssrc}), resetting jitter buffer"
                    );
                    self.queue.clear();
                    self.carry_pcm.clear();
                    self.next_idx = None;
                    self.state = JitterState::Buffering;
                    self.consecutive_plc = 0;
                    self.current_ssrc = Some(ssrc);
                }
            } else {
                self.current_ssrc = Some(ssrc);
            }
        }
        self.push(idx, opus_data);
    }

    pub fn push(&mut self, idx: u32, opus_data: Vec<u8>) {
        self.stats.rx_packets += 1;

        if self.queue.contains_key(&idx) {
            return;
        }

        if let Some(next) = self.next_idx {
            if idx < next {
                let diff = next - idx;
                if diff > 500 {
                    // Peer RTP sequence reset backward (e.g. after network handover)
                    eprintln!(
                        "[audio-io] info: sequence reset detected (prev={next}, new={idx}), flushing jitter buffer"
                    );
                    self.queue.clear();
                    self.carry_pcm.clear();
                    self.next_idx = None;
                    self.state = JitterState::Buffering;
                    self.consecutive_plc = 0;
                } else if self.consecutive_plc > 0 && diff <= (self.consecutive_plc as u32) {
                    // Packet arrived slightly delayed while we were concealing with PLC: accept & rewind
                    self.next_idx = Some(idx);
                } else if self.state == JitterState::Playing {
                    self.stats.late_dropped += 1;
                    return;
                }
            } else if (idx - next) > 500 {
                // Large forward sequence jump across network switch
                eprintln!(
                    "[audio-io] info: forward sequence jump detected (prev={next}, new={idx}), flushing jitter buffer"
                );
                self.queue.clear();
                self.carry_pcm.clear();
                self.next_idx = None;
                self.state = JitterState::Buffering;
                self.consecutive_plc = 0;
            }
        } else if let Some(&first) = self.queue.keys().next() {
            if idx.abs_diff(first) > 500 {
                self.queue.clear();
            }
        }

        self.queue.insert(idx, opus_data);

        // Prune queue if deeper than max_depth to prevent latency drift,
        // advancing next_idx so it never points to a pruned packet.
        while self.queue.len() > self.max_depth {
            let oldest_key = *self.queue.keys().next().unwrap();
            self.queue.remove(&oldest_key);
            if let Some(next) = self.next_idx {
                if next <= oldest_key {
                    self.next_idx = self.queue.keys().next().copied();
                }
            }
        }

        // If currently buffering and target depth is reached, transition to Playing
        if self.state == JitterState::Buffering && self.queue.len() >= self.min_target {
            self.state = JitterState::Playing;
            if self.next_idx.is_none() {
                self.next_idx = self.queue.keys().next().copied();
            }
            crate::zcall_log!(
                "[audio-io] info: jitter buffer primed (depth={}), resuming playout",
                self.queue.len()
            );
        }
    }

    /// Decode next frame into `out_pcm` (320 samples = 20 ms).
    /// Returns true if valid frame, carry frame, FEC frame, or PLC generated.
    pub fn pop_frame(&mut self, out_pcm: &mut [i16]) -> bool {
        // 1. Drain leftover samples from multi-frame (40ms/60ms) Opus packets first
        if self.carry_pcm.len() >= out_pcm.len() {
            for s in out_pcm.iter_mut() {
                *s = self.carry_pcm.pop_front().unwrap();
            }
            self.stats.played_frames += 1;
            return true;
        }

        match self.state {
            JitterState::Buffering => {
                if self.queue.len() >= self.min_target {
                    self.state = JitterState::Playing;
                    self.next_idx = self.queue.keys().next().copied();
                    self.consecutive_plc = 0;
                    crate::zcall_log!(
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

        // Prevent latency buildup on burst arrivals: if queue exceeds min_target + 4 (e.g. >140ms),
        // trim oldest frames down to min_target + 1 and advance next_idx.
        if self.queue.len() > self.min_target + 4 {
            while self.queue.len() > self.min_target + 1 {
                let oldest = *self.queue.keys().next().unwrap();
                self.queue.remove(&oldest);
            }
            self.next_idx = self.queue.keys().next().copied();
        }

        // Resync if sequence jumped or if a multi-packet hole occurred and min_target newer frames are ready
        if let Some(&first) = self.queue.keys().next() {
            if let Some(cur) = self.next_idx {
                if first < cur {
                    self.next_idx = Some(first);
                } else if first > cur {
                    let gap = first - cur;
                    if gap > 10 || (gap > 1 && self.queue.len() >= self.min_target) {
                        eprintln!(
                            "[audio-io] warn: sequence gap detected (expected {cur}, got {first}), resyncing"
                        );
                        self.next_idx = Some(first);
                        self.consecutive_plc = 0;
                    }
                }
            }
        }

        let cur_idx = self.next_idx.unwrap();
        self.next_idx = Some(cur_idx.wrapping_add(1));

        if let Some(payload) = self.queue.remove(&cur_idx) {
            self.consecutive_plc = 0;
            self.stats.played_frames += 1;
            match self
                .decoder
                .decode(&payload, &mut self.decode_scratch, false)
            {
                Ok(n) if n >= out_pcm.len() => {
                    out_pcm.copy_from_slice(&self.decode_scratch[..out_pcm.len()]);
                    if n > out_pcm.len() {
                        self.carry_pcm
                            .extend(&self.decode_scratch[out_pcm.len()..n]);
                    }
                }
                _ => {
                    let _ = self.decoder.decode(&[], out_pcm, false);
                }
            }
            true
        } else {
            // Single missing packet with next packet already present: recover using Opus in-band FEC
            let next_key = cur_idx.wrapping_add(1);
            if let Some(next_payload) = self.queue.get(&next_key) {
                self.consecutive_plc = 0;
                self.stats.plc_frames += 1;
                let _ = self.decoder.decode(next_payload, out_pcm, true);
                return true;
            }

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
                crate::zcall_log!(
                    "[audio-io] warn: jitter buffer starvation, entering buffering mode (target={} frames)",
                    self.min_target
                );
                false
            }
        }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.queue.len() >= self.min_target
    }
}

#[cfg(test)]
#[path = "../tests/audio/jitter.rs"]
mod tests;
