//! Audio processing and hardware interface module.

pub mod apm;
pub mod backend;
pub mod clip_guard;
pub mod codec;
pub mod dsp;
pub mod group;
pub mod jitter;
pub mod level;
pub mod leveler;

pub use apm::Apm;
pub use backend::{PulsePlayer, PulseRecorder};
pub use codec::{OpusDecoder, OpusEncoder};
pub use jitter::JitterBuffer;
pub use level::compute_rfc6464_level;
pub use leveler::MicLeveler;

pub const SAMPLE_RATE: u32 = 16000;
pub const CHANNELS: u32 = 1;
pub const FRAME_SAMPLES: usize = 320; // 20 ms at 16 kHz
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2; // 640 bytes s16le
