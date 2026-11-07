//! IPC Protocol constants and framing for zcall-audio.
//!
//! Output to stdout (big endian):
//!   'L' (0x4c), u8 level, u16 len, Opus frame
//!
//! Input from stdin (big endian):
//!   'F' (0x46), u32 idx, u32 ssrc, u16 len, Opus frame
//!   'M' (0x4d), u8 on (mute mic)
//!   'S' (0x53), u8 off (speaker off)
//!   'V' (0x56), u8 percent (mic volume 0..100)
//!   'D' (0x44), u8 kind ('i' for mic, 'o' for speaker), u16 len, device name

pub const SAMPLE_RATE: u32 = 16000;
pub const CHANNELS: u32 = 1;
pub const FRAME_SAMPLES: usize = 320; // 20 ms at 16 kHz
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2; // 640 bytes s16le

pub const CMD_IN_FRAME: u8 = 0x46; // 'F'
pub const CMD_IN_MUTE: u8 = 0x4d; // 'M'
pub const CMD_IN_SPEAKER: u8 = 0x53; // 'S'
pub const CMD_IN_VOLUME: u8 = 0x56; // 'V'
pub const CMD_IN_DEVICE: u8 = 0x44; // 'D'

pub const CMD_OUT_LEVEL: u8 = 0x4c; // 'L'
