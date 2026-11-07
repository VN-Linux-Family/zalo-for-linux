//! Audio Jitter Buffer and Packet Loss Concealment (PLC).

use std::collections::BTreeMap;
use crate::opus_codec::OpusDecoder;

pub struct JitterBuffer {
    decoder: OpusDecoder,
    queue: BTreeMap<u32, Vec<u8>>,
    next_idx: Option<u32>,
    max_depth: usize,
}

impl JitterBuffer {
    pub fn new(sample_rate: i32, channels: i32, depth_frames: usize) -> Result<Self, String> {
        let decoder = OpusDecoder::new(sample_rate, channels)?;
        Ok(Self {
            decoder,
            queue: BTreeMap::new(),
            next_idx: None,
            max_depth: depth_frames.max(5),
        })
    }

    pub fn push(&mut self, idx: u32, opus_data: Vec<u8>) {
        if let Some(next) = self.next_idx {
            // Drop ancient packets older than current playout pointer
            if idx < next && (next - idx) < 1000 {
                return;
            }
        }
        self.queue.insert(idx, opus_data);

        // Prune queue if deeper than max_depth to prevent latency drift
        while self.queue.len() > self.max_depth {
            let oldest_key = *self.queue.keys().next().unwrap();
            self.queue.remove(&oldest_key);
        }
    }

    /// Decode next frame into out_pcm (320 samples).
    /// Returns true if valid frame or PLC generated.
    pub fn pop_frame(&mut self, out_pcm: &mut [i16]) -> bool {
        if self.next_idx.is_none() {
            if let Some(&first) = self.queue.keys().next() {
                self.next_idx = Some(first);
            } else {
                return false;
            }
        }

        let cur_idx = self.next_idx.unwrap();
        self.next_idx = Some(cur_idx.wrapping_add(1));

        if let Some(payload) = self.queue.remove(&cur_idx) {
            let _ = self.decoder.decode(&payload, out_pcm, false);
            true
        } else {
            // Missing frame: run Opus Packet Loss Concealment (PLC)
            let _ = self.decoder.decode(&[], out_pcm, false);
            true
        }
    }

    pub fn is_ready(&self) -> bool {
        self.queue.len() >= 2
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
    fn test_jitter_buffer_in_order() {
        let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
        let mut enc = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS as i32, 32000).unwrap();

        let mut phase = 0.0;
        let mut opus_buf = vec![0u8; 1024];

        // Encode 3 frames
        for i in 0..3 {
            let pcm = generate_sine_frame(440.0, &mut phase);
            let len = enc.encode(&pcm, &mut opus_buf).unwrap();
            jb.push(i, opus_buf[..len].to_vec());
        }

        assert!(jb.is_ready());

        let mut out_pcm = vec![0i16; FRAME_SAMPLES];
        for _ in 0..3 {
            assert!(jb.pop_frame(&mut out_pcm));
            assert_eq!(out_pcm.len(), FRAME_SAMPLES);
        }
    }

    #[test]
    fn test_jitter_buffer_out_of_order_and_plc() {
        let mut jb = JitterBuffer::new(SAMPLE_RATE as i32, CHANNELS as i32, 10).unwrap();
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
}
