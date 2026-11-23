//! Native `PulseAudio` / `PipeWire` Simple API interface.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;

#[repr(C)]
#[allow(dead_code)]
pub enum PaStreamDirection {
    NoDirection = 0,
    Playback = 1,
    Record = 2,
    Upload = 3,
}

#[repr(C)]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum PaSampleFormat {
    U8 = 0,
    Alaw = 1,
    Ulaw = 2,
    S16Le = 3,
    S16Be = 4,
    Float32Le = 5,
    Float32Be = 6,
    S32Le = 7,
    S32Be = 8,
    S24Le = 9,
    S24Be = 10,
    S2432Le = 11,
    S2432Be = 12,
    Max = 13,
    Invalid = -1,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PaSampleSpec {
    pub format: PaSampleFormat,
    pub rate: u32,
    pub channels: u8,
}

#[repr(C)]
pub struct PaBufferAttr {
    pub maxlength: u32,
    pub tlength: u32,
    pub prebuf: u32,
    pub minreq: u32,
    pub fragsize: u32,
}

extern "C" {
    fn pa_simple_new(
        server: *const c_char,
        name: *const c_char,
        dir: PaStreamDirection,
        dev: *const c_char,
        stream_name: *const c_char,
        ss: *const PaSampleSpec,
        map: *const c_void,
        attr: *const PaBufferAttr,
        error: *mut c_int,
    ) -> *mut c_void;

    fn pa_simple_free(s: *mut c_void);

    fn pa_simple_write(
        s: *mut c_void,
        data: *const c_void,
        bytes: usize,
        error: *mut c_int,
    ) -> c_int;

    fn pa_simple_read(s: *mut c_void, data: *mut c_void, bytes: usize, error: *mut c_int) -> c_int;

    fn pa_simple_get_latency(s: *mut c_void, error: *mut c_int) -> u64;

    fn pa_strerror(error: c_int) -> *const c_char;

    fn zcall_pulse_get_source_info(
        source_name: *const c_char,
        out_pct: *mut c_int,
        out_muted: *mut c_int,
        out_resolved: *mut c_char,
        resolved_cap: c_int,
    ) -> c_int;
    fn zcall_pulse_set_source_volume(source_name: *const c_char, pct: c_int) -> c_int;
    fn zcall_pulse_set_source_mute(source_name: *const c_char, mute: c_int) -> c_int;
    fn zcall_pulse_normalize_own_stream(target_pid: c_int) -> c_int;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PulseSourceInfo {
    pub pct: u8,
    pub muted: bool,
    pub resolved_name: Option<String>,
}

/// Query hardware/PulseAudio/PipeWire source volume, mute state, and resolved name via libpulse.
pub fn pulse_get_source_info(source: Option<&str>) -> Option<PulseSourceInfo> {
    let cstr = source.and_then(|s| {
        if s.is_empty() {
            None
        } else {
            CString::new(s).ok()
        }
    });
    let ptr = cstr.as_ref().map_or(ptr::null(), |c| c.as_ptr());
    let mut pct: c_int = 0;
    let mut muted: c_int = 0;
    let mut resolved_buf = [0u8; 512];
    // SAFETY: `ptr` is either null or a `CString` alive for the whole call; every
    // out-pointer refers to a live stack variable sized as passed to the FFI.
    let rc = unsafe {
        zcall_pulse_get_source_info(
            ptr,
            &raw mut pct,
            &raw mut muted,
            resolved_buf.as_mut_ptr().cast::<c_char>(),
            c_int::try_from(resolved_buf.len())
                .expect("resolved source buffer length fits in c_int"),
        )
    };
    if rc == 0 {
        let resolved_name = std::ffi::CStr::from_bytes_until_nul(&resolved_buf)
            .ok()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty());
        Some(PulseSourceInfo {
            pct: pct.clamp(0, 150) as u8,
            muted: muted != 0,
            resolved_name,
        })
    } else {
        None
    }
}

/// Set hardware/PulseAudio/PipeWire source volume (0..=100%) directly via libpulse.
pub fn pulse_set_source_volume(source: Option<&str>, pct: u8) -> Result<(), String> {
    let cstr = source.and_then(|s| {
        if s.is_empty() {
            None
        } else {
            CString::new(s).ok()
        }
    });
    let ptr = cstr.as_ref().map_or(ptr::null(), |c| c.as_ptr());
    // SAFETY: `ptr` is either null or a `CString` alive for the whole call.
    let rc = unsafe { zcall_pulse_set_source_volume(ptr, c_int::from(pct.min(100))) };
    if rc == 0 {
        Ok(())
    } else {
        Err("libpulse source volume update failed".to_string())
    }
}

/// Set hardware/PulseAudio/PipeWire source mute state directly via libpulse.
pub fn pulse_set_source_mute(source: Option<&str>, mute: bool) -> Result<(), String> {
    let cstr = source.and_then(|s| {
        if s.is_empty() {
            None
        } else {
            CString::new(s).ok()
        }
    });
    let ptr = cstr.as_ref().map_or(ptr::null(), |c| c.as_ptr());
    // SAFETY: `ptr` is either null or a `CString` alive for the whole call.
    let rc = unsafe { zcall_pulse_set_source_mute(ptr, i32::from(mute)) };
    if rc == 0 {
        Ok(())
    } else {
        Err("libpulse source mute update failed".to_string())
    }
}

/// Ensure this process's own PulseAudio/PipeWire capture stream (source-output) is unmuted and at 100%.
pub fn pulse_normalize_own_stream() {
    let pid = c_int::try_from(std::process::id()).expect("process id fits in pid_t");
    // SAFETY: `zcall_pulse_normalize_own_stream` takes only a pid scalar.
    unsafe {
        let _ = zcall_pulse_normalize_own_stream(pid);
    }
}

pub struct PulseRecorder {
    handle: *mut c_void,
}

// SAFETY: the `pa_simple` handle is only used by the thread that owns the recorder;
// moving it transfers exclusive access and no other thread can call into it.
unsafe impl Send for PulseRecorder {}

impl PulseRecorder {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let name = CString::new("ZaloCall").unwrap();
        let stream = CString::new("microphone").unwrap();
        let dev_cstr = device.and_then(|d| {
            if d.is_empty() {
                None
            } else {
                CString::new(d).ok()
            }
        });
        let dev_ptr = dev_cstr.as_ref().map_or(ptr::null(), |d| d.as_ptr());

        let ss = PaSampleSpec {
            format: PaSampleFormat::S16Le,
            rate: 16000,
            channels: 1,
        };

        let attr = PaBufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: 640, // 20 ms at 16 kHz s16le mono (320 samples * 2 bytes)
        };

        let mut error = 0;
        // SAFETY: `name`, `stream`, `dev_cstr`, `ss` and `attr` all live through the
        // call; the returned handle is null-checked before being stored.
        let handle = unsafe {
            pa_simple_new(
                ptr::null(),
                name.as_ptr(),
                PaStreamDirection::Record,
                dev_ptr,
                stream.as_ptr(),
                &raw const ss,
                ptr::null(),
                &raw const attr,
                &raw mut error,
            )
        };

        if handle.is_null() {
            // SAFETY: `pa_strerror` returns a static NUL-terminated string owned by
            // libpulse for the lifetime of the process.
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(format!("pa_simple_new (record) failed: {msg}"));
        }

        Ok(Self { handle })
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<(), String> {
        let mut error = 0;
        // SAFETY: `self.handle` came from a successful `pa_simple_new`; `buf` is a live
        // mutable slice and `buf.len()` matches the byte count passed.
        let res = unsafe {
            pa_simple_read(
                self.handle,
                buf.as_mut_ptr().cast::<c_void>(),
                buf.len(),
                &raw mut error,
            )
        };
        if res < 0 {
            // SAFETY: `pa_strerror` returns a static NUL-terminated string owned by
            // libpulse for the lifetime of the process.
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
            };
            Err(format!("pa_simple_read failed: {msg}"))
        } else {
            Ok(())
        }
    }
}

impl Drop for PulseRecorder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is non-null, came from a successful `pa_simple_new`, and
            // is freed exactly once here.
            unsafe { pa_simple_free(self.handle) };
        }
    }
}

pub struct PulsePlayer {
    handle: *mut c_void,
}

// SAFETY: the `pa_simple` handle is only used by the thread that owns the player;
// moving it transfers exclusive access and no other thread can call into it.
unsafe impl Send for PulsePlayer {}

impl PulsePlayer {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let name = CString::new("ZaloCall").unwrap();
        let stream = CString::new("playback").unwrap();
        let dev_cstr = device.and_then(|d| {
            if d.is_empty() {
                None
            } else {
                CString::new(d).ok()
            }
        });
        let dev_ptr = dev_cstr.as_ref().map_or(ptr::null(), |d| d.as_ptr());

        let ss = PaSampleSpec {
            format: PaSampleFormat::S16Le,
            rate: 16000,
            channels: 1,
        };

        let attr = PaBufferAttr {
            maxlength: u32::MAX,
            tlength: 1920, // 60 ms buffer latency (matches audio-io.py LATENCY_MS = 60 for tight A/V sync)
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: u32::MAX,
        };

        let mut error = 0;
        // SAFETY: `name`, `stream`, `dev_cstr`, `ss` and `attr` all live through the
        // call; the returned handle is null-checked before being stored.
        let handle = unsafe {
            pa_simple_new(
                ptr::null(),
                name.as_ptr(),
                PaStreamDirection::Playback,
                dev_ptr,
                stream.as_ptr(),
                &raw const ss,
                ptr::null(),
                &raw const attr,
                &raw mut error,
            )
        };

        if handle.is_null() {
            // SAFETY: `pa_strerror` returns a static NUL-terminated string owned by
            // libpulse for the lifetime of the process.
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(format!("pa_simple_new (playback) failed: {msg}"));
        }

        Ok(Self { handle })
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut error = 0;
        // SAFETY: `self.handle` came from a successful `pa_simple_new`; `buf` is a live
        // slice and `buf.len()` matches the byte count passed.
        let res = unsafe {
            pa_simple_write(
                self.handle,
                buf.as_ptr().cast::<c_void>(),
                buf.len(),
                &raw mut error,
            )
        };
        if res < 0 {
            // SAFETY: `pa_strerror` returns a static NUL-terminated string owned by
            // libpulse for the lifetime of the process.
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
            };
            Err(format!("pa_simple_write failed: {msg}"))
        } else {
            Ok(())
        }
    }

    pub fn latency_ms(&self) -> Option<u64> {
        if self.handle.is_null() {
            return None;
        }
        let mut error = 0;
        // SAFETY: `self.handle` is non-null (checked above) and came from a successful
        // `pa_simple_new`; `&mut error` points to a live stack integer.
        let us = unsafe { pa_simple_get_latency(self.handle, &raw mut error) };
        if error != 0 {
            None
        } else {
            Some(us / 1000)
        }
    }
}

impl Drop for PulsePlayer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            // SAFETY: `handle` is non-null, came from a successful `pa_simple_new`, and
            // is freed exactly once here.
            unsafe { pa_simple_free(self.handle) };
        }
    }
}
