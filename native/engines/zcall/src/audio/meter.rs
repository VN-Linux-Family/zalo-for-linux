//! Application-level PCM peak/RMS measurement for audio buffers.

use pipewire_client::{AudioSampleFormat, NegotiatedAudioFormat};
use std::io;

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// Peak and RMS statistics for one channel of a PCM stream, in linear amplitude
/// and dBFS. Values above 0 dBFS are retained for floating-point over-range input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioChannelMeter {
    pub peak: f32,
    pub rms: f32,
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
}

impl AudioChannelMeter {
    #[must_use]
    pub fn peak_level(&self) -> f32 {
        self.peak.clamp(0.0, 1.0)
    }

    #[must_use]
    pub fn rms_level(&self) -> f32 {
        self.rms.clamp(0.0, 1.0)
    }
}

/// PCM peak/RMS snapshot measured by Zcall from the buffers it consumes.
///
/// This is application-side signal analysis, not a `PipeWire` stream-control API
/// and does not query a server-side meter plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioMeter {
    /// Maximum absolute sample across all channels (linear amplitude).
    pub peak: f32,
    /// Root-mean-square across every sample and channel (linear amplitude).
    pub rms: f32,
    /// Peak amplitude in dBFS (`-inf` for digital silence).
    pub peak_dbfs: f32,
    /// RMS amplitude in dBFS (`-inf` for digital silence).
    pub rms_dbfs: f32,
    /// Per-channel peak/RMS statistics in channel order.
    pub channels: Vec<AudioChannelMeter>,
    /// Number of complete audio frames measured.
    pub frames: u64,
}

impl AudioMeter {
    /// Measure one complete interleaved PCM buffer using its negotiated format.
    pub fn measure(data: &[u8], format: NegotiatedAudioFormat) -> io::Result<Self> {
        if !(8_000..=768_000).contains(&format.rate) || !(1..=64).contains(&format.channels) {
            return Err(invalid_input("invalid PCM format parameters"));
        }
        if format.sample_format == AudioSampleFormat::F32P && format.channels != 1 {
            return Err(invalid_input(
                "AudioMeter requires interleaved PCM or single-channel planar DSP audio",
            ));
        }
        let sample_bytes = if format.sample_format == AudioSampleFormat::S16Le {
            2
        } else {
            4
        };
        let channels = usize::try_from(format.channels)
            .map_err(|_| invalid_input("Audio meter channel count overflow"))?;
        let frame_bytes = channels
            .checked_mul(sample_bytes)
            .ok_or_else(|| invalid_input("Audio meter frame size overflow"))?;
        if data.is_empty() || !data.len().is_multiple_of(frame_bytes) {
            return Err(invalid_input(
                "Audio meter input must contain complete non-empty PCM frames",
            ));
        }

        let mut channel_peaks = vec![0.0_f32; channels];
        let mut channel_squares = vec![0.0_f64; channels];
        let mut frames = 0_u64;
        for frame in data.chunks_exact(frame_bytes) {
            for (channel, sample) in frame.chunks_exact(sample_bytes).enumerate() {
                let value = match format.sample_format {
                    AudioSampleFormat::S16Le => {
                        f32::from(i16::from_le_bytes([sample[0], sample[1]])) / 32768.0
                    }
                    AudioSampleFormat::F32Le | AudioSampleFormat::F32P => {
                        let value =
                            f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                        if !value.is_finite() {
                            return Err(invalid_input(
                                "Audio meter input contains a non-finite float sample",
                            ));
                        }
                        value
                    }
                };
                channel_peaks[channel] = channel_peaks[channel].max(value.abs());
                channel_squares[channel] += f64::from(value) * f64::from(value);
            }
            frames += 1;
        }

        let frame_count = u32::try_from(frames)
            .map_err(|_| invalid_input("Audio meter frame count exceeds supported range"))?;
        let frame_count_f64 = f64::from(frame_count);
        let channel_meters = channel_peaks
            .iter()
            .zip(channel_squares)
            .map(|(&peak, squares)| {
                let rms = (squares / frame_count_f64).sqrt() as f32;
                AudioChannelMeter {
                    peak,
                    rms,
                    peak_dbfs: amplitude_to_dbfs(peak),
                    rms_dbfs: amplitude_to_dbfs(rms),
                }
            })
            .collect::<Vec<_>>();
        let peak = channel_meters
            .iter()
            .map(|meter| meter.peak)
            .fold(0.0, f32::max);
        let total_squares = channel_squares_sum(&channel_meters, frame_count);
        let sample_count = f64::from(frame_count) * f64::from(format.channels);
        let rms = (total_squares / sample_count).sqrt() as f32;
        Ok(Self {
            peak,
            rms,
            peak_dbfs: amplitude_to_dbfs(peak),
            rms_dbfs: amplitude_to_dbfs(rms),
            channels: channel_meters,
            frames,
        })
    }

    /// Clamped RMS level suitable for a 0.0..=1.0 UI meter.
    #[must_use]
    pub fn level(&self) -> f32 {
        self.rms.clamp(0.0, 1.0)
    }

    /// Clamped peak level suitable for a 0.0..=1.0 peak indicator.
    #[must_use]
    pub fn peak_level(&self) -> f32 {
        self.peak.clamp(0.0, 1.0)
    }
}

fn amplitude_to_dbfs(amplitude: f32) -> f32 {
    if amplitude <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * amplitude.log10()
    }
}

fn channel_squares_sum(channels: &[AudioChannelMeter], frames: u32) -> f64 {
    channels
        .iter()
        .map(|channel| f64::from(channel.rms).powi(2) * f64::from(frames))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_meter_reports_peak_and_rms() {
        let format = NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::S16Le,
            rate: 48_000,
            channels: 2,
        };
        let data = [i16::MIN, 16_384, 0, -16_384]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect::<Vec<_>>();
        let meter = AudioMeter::measure(&data, format).unwrap();
        assert_eq!(meter.peak, 1.0);
        assert!((meter.rms - (1.5_f32 / 4.0).sqrt()).abs() < 0.0001);
    }

    #[test]
    fn audio_meter_exposes_per_channel_peak_rms_and_dbfs() {
        let format = NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::F32Le,
            rate: 48_000,
            channels: 2,
        };
        let samples = [0.5_f32, 0.25, -0.5, -0.25];
        let data = samples
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect::<Vec<_>>();
        let meter = AudioMeter::measure(&data, format).unwrap();
        assert_eq!(meter.frames, 2);
        assert_eq!(meter.channels.len(), 2);
        assert_eq!(meter.peak, 0.5);
        assert!((meter.rms - 0.395_284_7).abs() < 0.0001);
        assert!((meter.channels[0].rms - 0.5).abs() < 0.0001);
        assert!((meter.channels[1].rms - 0.25).abs() < 0.0001);
        assert!((meter.peak_dbfs - -6.0206).abs() < 0.001);
        assert!((meter.rms_dbfs - 20.0 * meter.rms.log10()).abs() < 0.0001);
        assert!((meter.level() - meter.rms).abs() < f32::EPSILON);
    }

    #[test]
    fn audio_meter_reports_silence_as_negative_infinity_dbfs() {
        let format = NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::S16Le,
            rate: 16_000,
            channels: 1,
        };
        let meter = AudioMeter::measure(&[0; 4], format).unwrap();
        assert_eq!(meter.peak, 0.0);
        assert_eq!(meter.rms, 0.0);
        assert_eq!(meter.peak_dbfs, f32::NEG_INFINITY);
        assert_eq!(meter.rms_dbfs, f32::NEG_INFINITY);
        assert_eq!(meter.level(), 0.0);
    }

    #[test]
    fn audio_meter_rejects_partial_frames_and_non_finite_samples() {
        let stereo = NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::F32Le,
            rate: 48_000,
            channels: 2,
        };
        assert!(AudioMeter::measure(&[0; 4], stereo).is_err());
        assert!(AudioMeter::measure(&f32::NAN.to_le_bytes().repeat(2), stereo).is_err());
    }
}
