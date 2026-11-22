//! ZCall Rust engine core and its direct UI transport module. Zalo helper lifecycle stays in the binary.

pub mod audio;
pub mod camera;
pub mod crypto;
pub mod devices;
pub mod engine;
pub mod logging;
pub mod media;
pub mod protocol;
pub mod screen;
pub mod ui;
pub mod video;

#[macro_export]
macro_rules! zcall_log {
    ($($arg:tt)*) => { $crate::logging::log(format_args!($($arg)*)) };
}
