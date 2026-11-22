//! SpeexDSP call processing: echo cancellation, optional noise suppression,
//! high-pass filtering and a slow speech-only microphone leveler.
use serde_json::Value;
use std::collections::VecDeque;
use std::os::raw::{c_int, c_void};

const FRAME_SAMPLES: usize = 320;
const SAMPLE_RATE: c_int = 16_000;
const ECHO_FILTER_SAMPLES: c_int = 3_200;
const ECHO_SET_SAMPLING_RATE: c_int = 24;
const PREPROCESS_SET_DENOISE: c_int = 0;
const PREPROCESS_SET_ECHO_STATE: c_int = 24;
const PREPROCESS_SET_NOISE_SUPPRESS: c_int = 18;

#[link(name = "speexdsp")]
unsafe extern "C" {
    fn speex_echo_state_init(frame_size: c_int, filter_length: c_int) -> *mut c_void;
    fn speex_echo_state_destroy(state: *mut c_void);
    fn speex_echo_ctl(state: *mut c_void, request: c_int, value: *mut c_void) -> c_int;
    fn speex_echo_playback(state: *mut c_void, playback: *const i16);
    fn speex_echo_capture(state: *mut c_void, recorded: *const i16, output: *mut i16);

    fn speex_preprocess_state_init(frame_size: c_int, sample_rate: c_int) -> *mut c_void;
    fn speex_preprocess_state_destroy(state: *mut c_void);
    fn speex_preprocess_ctl(state: *mut c_void, request: c_int, value: *mut c_void) -> c_int;
    fn speex_preprocess_run(state: *mut c_void, samples: *mut i16) -> c_int;
}

pub struct AudioProcessor {
    echo: *mut c_void,
    preprocess: *mut c_void,
    noise_suppression: bool,
    high_pass: bool,
    high_pass_prev_input: f32,
    high_pass_prev_output: f32,
    mic_gain: bool,
    gain_db: f32,
    noise_floor_db: f32,
    level_history: VecDeque<f32>,
    far_active: bool,
    playback_peak: f32,
    pub processed_frames: u64,
}

unsafe impl Send for AudioProcessor {}

impl AudioProcessor {
    pub fn new(config: &Value) -> Result<Self, String> {
        let echo_enabled = config
            .get("audioEchoCancellation")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let noise_suppression = config
            .get("audioNoiseSuppression")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || std::env::var("ZCALL_NOISE_SUPPRESSION").is_ok_and(|value| value == "1");
        let high_pass = config
            .get("audioHighPassFilter")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let mic_gain = config
            .get("audioMicGain")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| std::env::var("ZCALL_MIC_GAIN").map_or(true, |value| value != "0"));

        let echo = if echo_enabled {
            let state =
                unsafe { speex_echo_state_init(FRAME_SAMPLES as c_int, ECHO_FILTER_SAMPLES) };
            if state.is_null() {
                return Err("SpeexDSP echo state initialization failed".into());
            }
            let mut sample_rate = SAMPLE_RATE;
            let result = unsafe {
                speex_echo_ctl(
                    state,
                    ECHO_SET_SAMPLING_RATE,
                    &mut sample_rate as *mut c_int as *mut c_void,
                )
            };
            if result != 0 {
                unsafe { speex_echo_state_destroy(state) };
                return Err("SpeexDSP echo sample-rate configuration failed".into());
            }
            state
        } else {
            std::ptr::null_mut()
        };

        let preprocess = if echo_enabled || noise_suppression {
            let state = unsafe { speex_preprocess_state_init(FRAME_SAMPLES as c_int, SAMPLE_RATE) };
            if state.is_null() {
                if !echo.is_null() {
                    unsafe { speex_echo_state_destroy(echo) };
                }
                return Err("SpeexDSP preprocessor initialization failed".into());
            }
            let mut denoise = c_int::from(noise_suppression);
            unsafe {
                speex_preprocess_ctl(
                    state,
                    PREPROCESS_SET_DENOISE,
                    &mut denoise as *mut c_int as *mut c_void,
                );
                if noise_suppression {
                    let mut suppression_db: c_int = -20;
                    speex_preprocess_ctl(
                        state,
                        PREPROCESS_SET_NOISE_SUPPRESS,
                        &mut suppression_db as *mut c_int as *mut c_void,
                    );
                }
                if !echo.is_null() {
                    speex_preprocess_ctl(state, PREPROCESS_SET_ECHO_STATE, echo);
                }
            }
            state
        } else {
            std::ptr::null_mut()
        };

        Ok(Self {
            echo,
            preprocess,
            noise_suppression,
            high_pass,
            high_pass_prev_input: 0.0,
            high_pass_prev_output: 0.0,
            mic_gain,
            gain_db: 0.0,
            noise_floor_db: -70.0,
            level_history: VecDeque::with_capacity(32),
            far_active: false,
            playback_peak: 0.0,
            processed_frames: 0,
        })
    }

    /// Feed the exact PCM frame that the playback worker is about to render.
    /// Speex queues it internally to align with the microphone capture clock.
    pub fn playback(&mut self, samples: &[i16]) {
        if samples.len() != FRAME_SAMPLES {
            return;
        }
        self.playback_peak = samples
            .iter()
            .map(|sample| f32::from(*sample).abs())
            .fold(0.0, f32::max);
        self.far_active = self.playback_peak > 96.0;
        if !self.echo.is_null() {
            unsafe { speex_echo_playback(self.echo, samples.as_ptr()) };
        }
    }

    /// Process one 20 ms microphone frame in this order:
    /// AEC -> Speex residual/noise preprocessing -> high-pass -> adaptive level.
    pub fn capture(&mut self, samples: &mut [i16]) {
        if samples.len() != FRAME_SAMPLES {
            return;
        }

        if !self.echo.is_null() {
            let mut clean = [0i16; FRAME_SAMPLES];
            unsafe {
                speex_echo_capture(self.echo, samples.as_ptr(), clean.as_mut_ptr());
            }
            samples.copy_from_slice(&clean);
        }

        if !self.preprocess.is_null() {
            unsafe {
                speex_preprocess_run(self.preprocess, samples.as_mut_ptr());
            }
        }

        if self.high_pass {
            self.apply_high_pass(samples);
        }
        if self.mic_gain {
            self.apply_speech_leveler(samples);
        }
        self.processed_frames = self.processed_frames.saturating_add(1);
    }

    fn apply_high_pass(&mut self, samples: &mut [i16]) {
        // First-order 100 Hz high-pass filter at 16 kHz.
        const ALPHA: f32 = 0.962;
        for sample in samples {
            let input = f32::from(*sample);
            let output = ALPHA * (self.high_pass_prev_output + input - self.high_pass_prev_input);
            self.high_pass_prev_input = input;
            self.high_pass_prev_output = output;
            *sample = output.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
    }

    fn apply_speech_leveler(&mut self, samples: &mut [i16]) {
        let rms = (samples
            .iter()
            .map(|sample| {
                let value = f64::from(*sample);
                value * value
            })
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
            .max(1.0) as f32;
        let level_db = 20.0 * (rms / 32768.0).log10();

        self.level_history.push_back(level_db);
        while self.level_history.len() > 40 {
            self.level_history.pop_front();
        }
        if self.level_history.len() >= 20 {
            let mut sorted = self.level_history.iter().copied().collect::<Vec<_>>();
            sorted.sort_by(f32::total_cmp);
            self.noise_floor_db = sorted[sorted.len() / 10];
        }

        let previous_gain = 10.0f32.powf(self.gain_db / 20.0);
        let mut target_gain_db = self.gain_db;
        if !self.far_active && level_db > -55.0 && level_db > self.noise_floor_db + 9.0 {
            let noise_ceiling = (-38.0 - self.noise_floor_db).clamp(0.0, 24.0);
            let requested = (-23.0 - level_db).clamp(0.0, 24.0).min(noise_ceiling);
            target_gain_db = requested;
        }
        // Slow gain-up, faster gain-down, held during far-end playback to avoid
        // lifting the residual echo into the microphone stream.
        if !self.far_active {
            let delta = target_gain_db - self.gain_db;
            self.gain_db += delta.clamp(-2.0, 0.4);
        }
        let target = 10.0f32.powf(self.gain_db / 20.0);
        let mut gain = previous_gain;
        let gain_step = (target - previous_gain) / samples.len() as f32;
        for sample in samples {
            let value = f32::from(*sample) * gain;
            *sample = value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            gain += gain_step;
        }
    }

    pub fn is_echo_cancellation_enabled(&self) -> bool {
        !self.echo.is_null()
    }

    pub fn is_noise_suppression_enabled(&self) -> bool {
        self.noise_suppression
    }

    pub fn is_high_pass_enabled(&self) -> bool {
        self.high_pass
    }
}

impl Drop for AudioProcessor {
    fn drop(&mut self) {
        unsafe {
            if !self.preprocess.is_null() {
                speex_preprocess_state_destroy(self.preprocess);
                self.preprocess = std::ptr::null_mut();
            }
            if !self.echo.is_null() {
                speex_echo_state_destroy(self.echo);
                self.echo = std::ptr::null_mut();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn aec_and_preprocessor_process_synchronized_call_frames() {
        let mut processor = AudioProcessor::new(&json!({
            "audioEchoCancellation":true,
            "audioNoiseSuppression":true,
            "audioHighPassFilter":true
        }))
        .unwrap();
        assert!(processor.is_echo_cancellation_enabled());
        assert!(processor.is_noise_suppression_enabled());

        let mut far = [0i16; FRAME_SAMPLES];
        let mut near = [0i16; FRAME_SAMPLES];
        for frame in 0..50 {
            for index in 0..FRAME_SAMPLES {
                let sample = ((index + frame * FRAME_SAMPLES) as f32 * 0.08).sin() * 6000.0;
                far[index] = sample as i16;
                near[index] = sample as i16;
            }
            processor.playback(&far);
            processor.capture(&mut near);
            assert_eq!(near.len(), FRAME_SAMPLES);
        }
        assert_eq!(processor.processed_frames, 50);
    }

    #[test]
    fn can_disable_aec_and_leave_noise_suppression_off_by_default() {
        let processor = AudioProcessor::new(&json!({
            "audioEchoCancellation":false,
            "audioNoiseSuppression":false
        }))
        .unwrap();
        assert!(!processor.is_echo_cancellation_enabled());
        assert!(!processor.is_noise_suppression_enabled());
    }
}
