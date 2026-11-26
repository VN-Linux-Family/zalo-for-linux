//! Pure-Rust WebRTC-style audio processing adapter.

use std::sync::{
    atomic::{AtomicI32, Ordering},
    Mutex,
};

use sonora::config::{EchoCanceller, HighPassFilter, NoiseSuppression, NoiseSuppressionLevel};
use sonora::{AudioProcessing, Config, StreamConfig};

struct ApmInner {
    processing: AudioProcessing,
    render_scratch: Vec<i16>,
    capture_scratch: Vec<i16>,
}

pub struct Apm {
    inner: Mutex<ApmInner>,
    delay_ms: AtomicI32,
    samples_per_chunk: usize,
}

impl Apm {
    pub fn new(
        sample_rate_hz: i32,
        channels: i32,
        aec_enabled: bool,
        ns_enabled: bool,
        hpf_enabled: bool,
    ) -> Result<Self, String> {
        let sample_rate_hz =
            u32::try_from(sample_rate_hz).map_err(|_| "invalid audio sample rate".to_string())?;
        let channels =
            u16::try_from(channels).map_err(|_| "invalid audio channel count".to_string())?;
        let samples_per_chunk = (sample_rate_hz as usize / 100)
            .checked_mul(channels as usize)
            .filter(|&samples| samples > 0)
            .ok_or_else(|| "invalid 10 ms audio frame size".to_string())?;
        let stream = StreamConfig::new(sample_rate_hz, channels);
        let config = Config {
            echo_canceller: aec_enabled.then(|| EchoCanceller {
                enforce_high_pass_filtering: hpf_enabled,
                ..EchoCanceller::default()
            }),
            noise_suppression: ns_enabled.then(|| NoiseSuppression {
                level: NoiseSuppressionLevel::High,
                ..NoiseSuppression::default()
            }),
            high_pass_filter: hpf_enabled.then(HighPassFilter::default),
            ..Config::default()
        };
        let processing = AudioProcessing::builder()
            .config(config)
            .capture_config(stream)
            .render_config(stream)
            .build();
        Ok(Self {
            inner: Mutex::new(ApmInner {
                processing,
                render_scratch: vec![0; samples_per_chunk],
                capture_scratch: vec![0; samples_per_chunk],
            }),
            delay_ms: AtomicI32::new(40),
            samples_per_chunk,
        })
    }

    /// Feed speaker audio as the far-end reference for echo cancellation.
    pub fn process_render(&self, pcm: &[i16]) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let ApmInner {
            processing,
            render_scratch,
            ..
        } = &mut *inner;
        for chunk in pcm.chunks_exact(self.samples_per_chunk) {
            let _ = processing.process_render_i16(chunk, render_scratch);
        }
    }

    /// Apply echo cancellation, noise suppression, and optional high-pass filtering.
    pub fn process_capture(&self, pcm: &mut [i16]) {
        let delay_ms = self.delay_ms.load(Ordering::Relaxed);
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        let ApmInner {
            processing,
            capture_scratch,
            ..
        } = &mut *inner;
        let _ = processing.set_stream_delay_ms(delay_ms);
        for chunk in pcm.chunks_exact_mut(self.samples_per_chunk) {
            capture_scratch.copy_from_slice(chunk);
            if processing
                .process_capture_i16(capture_scratch, chunk)
                .is_err()
            {
                chunk.copy_from_slice(capture_scratch);
            }
        }
    }

    pub fn set_delay_ms(&self, delay_ms: i32) {
        self.delay_ms.store(delay_ms, Ordering::Relaxed);
    }
}

#[cfg(test)]
#[path = "../tests/audio/apm.rs"]
mod tests;
