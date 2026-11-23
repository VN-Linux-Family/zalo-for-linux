//! `MicLeveler`: DSP software make-up gain with adaptive noise-floor ceiling,
//! speech-triggered gain adaptation, far-end hold, sample-by-sample smooth ramp,
//! and peak limiting.
//!
//! Ported directly from Zalo native engine `audio-io.py` (Zalo-Linux-Native-Dev).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const TARGET_DB: f32 = -22.0; // wanted speech level, dBFS rms
pub const NOISE_CEIL_DB: f32 = -38.0; // noise floor after gain stays below this
pub const MAX_GAIN_DB: f32 = 24.0;
pub const SPEECH_OVER_NOISE_DB: f32 = 9.0;
pub const FLOOR_DB: f32 = -60.0; // below this a frame is never speech
pub const LEARN_FRAMES: usize = 20; // 0.4 s of noise floor learning before any adaptation
pub const UP_DB: f32 = 0.5; // per speech frame: +25 dB/s at most
pub const DOWN_DB: f32 = 1.0; // per speech frame: loud word comes down in 0.2 s
pub const LOG_EVERY: usize = 250; // frames: 5 s
pub const LEVEL_WINDOW: usize = 250; // 5 s of frame levels for noise estimation

pub struct MicLeveler {
    gain_db: f32,
    g_prev: f32,
    noise_db: f32,
    levels: Vec<f32>,
    level_idx: usize,
    level_count: usize,
    n: usize,
    speech_frames: usize,
    held_frames: usize,
    speech_db: Vec<f32>,
    far_active: Option<Arc<AtomicBool>>,
}

impl MicLeveler {
    #[must_use]
    pub fn new(far_active: Option<Arc<AtomicBool>>) -> Self {
        Self {
            gain_db: 0.0,
            g_prev: 1.0,
            noise_db: -70.0,
            levels: vec![0.0; LEVEL_WINDOW],
            level_idx: 0,
            level_count: 0,
            n: 0,
            speech_frames: 0,
            held_frames: 0,
            speech_db: Vec::with_capacity(LOG_EVERY),
            far_active,
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "metrics/percent; precision below 2^52 is irrelevant"
    )]
    pub fn compute_level_db(samples: &[i16]) -> f32 {
        if samples.is_empty() {
            return -120.0;
        }
        let sum_sq: f64 = samples.iter().map(|&v| f64::from(v) * f64::from(v)).sum();
        let rms = (sum_sq / samples.len() as f64).sqrt().max(1.0);
        20.0 * (rms / 32768.0).log10() as f32
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "metrics/percent; precision below 2^52 is irrelevant"
    )]
    pub fn process(&mut self, samples: &mut [i16]) {
        if samples.is_empty() {
            return;
        }

        let level = Self::compute_level_db(samples);

        // Store level in circular buffer
        self.levels[self.level_idx] = level;
        self.level_idx = (self.level_idx + 1) % LEVEL_WINDOW;
        if self.level_count < LEVEL_WINDOW {
            self.level_count += 1;
        }
        self.n += 1;

        // Learn noise floor (10th percentile of pauses)
        if self.level_count >= LEARN_FRAMES
            && (self.n.is_multiple_of(25) || self.level_count == LEARN_FRAMES)
        {
            let mut sorted = self.levels[..self.level_count].to_vec();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let p10_idx = self.level_count / 10;
            self.noise_db = sorted[p10_idx];
        }

        // Speech detection and gain adaptation
        if self.level_count >= LEARN_FRAMES
            && level > FLOOR_DB
            && level > (self.noise_db + SPEECH_OVER_NOISE_DB)
        {
            let far_is_talking = self
                .far_active
                .as_ref()
                .is_some_and(|f| f.load(Ordering::Relaxed));

            if far_is_talking {
                self.held_frames += 1;
            } else {
                self.speech_frames += 1;
                self.speech_db.push(level);

                let ceiling = MAX_GAIN_DB.min((NOISE_CEIL_DB - self.noise_db).max(0.0));
                let want = (TARGET_DB - level).max(0.0).min(ceiling);

                let diff = want - self.gain_db;
                let step = diff.clamp(-DOWN_DB, UP_DB);
                self.gain_db += step;
            }
        }

        // Periodic diagnostics
        if self.n.is_multiple_of(LOG_EVERY) {
            if !self.speech_db.is_empty() {
                self.speech_db
                    .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let median_speech = self.speech_db[self.speech_db.len() / 2];
                let held_pct = 100 * self.held_frames / LOG_EVERY;
                let speech_pct = 100 * self.speech_frames / LOG_EVERY;
                crate::zcall_log!(
                    "[audio-io] mic level: noise {:.0} dBFS, speech {:.0} dBFS, gain +{:.1} dB, speech in {}% of frames (held {}% far end)",
                    self.noise_db, median_speech, self.gain_db, speech_pct, held_pct
                );
            }
            self.speech_frames = 0;
            self.held_frames = 0;
            self.speech_db.clear();
        }

        // Calculate gain multiplier and ramp
        let mut g = 10.0f32.powf(self.gain_db / 20.0);
        let mut g0 = self.g_prev;
        let n_samples = samples.len();

        let peak = samples
            .iter()
            .map(|&v| i32::from(v).abs())
            .max()
            .unwrap_or(0);

        if peak > 0 {
            let lim = 32000.0 / peak as f32;
            g = g.min(lim);
            g0 = g0.min(lim);
        }

        self.g_prev = g;

        if (g - 1.0).abs() < 0.01 && (g0 - 1.0).abs() < 0.01 {
            return;
        }

        let step = (g - g0) / n_samples as f32;
        for (i, s) in samples.iter_mut().enumerate() {
            let factor = step.mul_add((i + 1) as f32, g0);
            let val = (f32::from(*s) * factor).round() as i32;
            *s = val.clamp(-32768, 32767) as i16;
        }
    }
    #[cfg(test)]
    #[must_use]
    pub const fn gain_db(&self) -> f32 {
        self.gain_db
    }

    #[cfg(test)]
    #[must_use]
    pub const fn noise_db(&self) -> f32 {
        self.noise_db
    }
}

#[cfg(test)]
#[path = "../tests/audio/leveler.rs"]
mod tests;
