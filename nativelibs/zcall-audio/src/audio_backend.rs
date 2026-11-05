//! Native PulseAudio / PipeWire Simple API interface.

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

    fn pa_simple_read(
        s: *mut c_void,
        data: *mut c_void,
        bytes: usize,
        error: *mut c_int,
    ) -> c_int;

    fn pa_strerror(error: c_int) -> *const c_char;
}

pub struct PulseRecorder {
    handle: *mut c_void,
}

unsafe impl Send for PulseRecorder {}

impl PulseRecorder {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let name = CString::new("ZaloCall").unwrap();
        let stream = CString::new("microphone").unwrap();
        let dev_cstr = device.and_then(|d| if d.is_empty() { None } else { CString::new(d).ok() });
        let dev_ptr = dev_cstr.as_ref().map(|d| d.as_ptr()).unwrap_or(ptr::null());

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
        let handle = unsafe {
            pa_simple_new(
                ptr::null(),
                name.as_ptr(),
                PaStreamDirection::Record,
                dev_ptr,
                stream.as_ptr(),
                &ss,
                ptr::null(),
                &attr,
                &mut error,
            )
        };

        if handle.is_null() {
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str).to_string_lossy().into_owned()
            };
            return Err(format!("pa_simple_new (record) failed: {}", msg));
        }

        Ok(Self { handle })
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<(), String> {
        let mut error = 0;
        let res = unsafe {
            pa_simple_read(self.handle, buf.as_mut_ptr() as *mut c_void, buf.len(), &mut error)
        };
        if res < 0 {
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str).to_string_lossy().into_owned()
            };
            Err(format!("pa_simple_read failed: {}", msg))
        } else {
            Ok(())
        }
    }
}

impl Drop for PulseRecorder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { pa_simple_free(self.handle) };
        }
    }
}

pub struct PulsePlayer {
    handle: *mut c_void,
}

unsafe impl Send for PulsePlayer {}

impl PulsePlayer {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let name = CString::new("ZaloCall").unwrap();
        let stream = CString::new("playback").unwrap();
        let dev_cstr = device.and_then(|d| if d.is_empty() { None } else { CString::new(d).ok() });
        let dev_ptr = dev_cstr.as_ref().map(|d| d.as_ptr()).unwrap_or(ptr::null());

        let ss = PaSampleSpec {
            format: PaSampleFormat::S16Le,
            rate: 16000,
            channels: 1,
        };

        let attr = PaBufferAttr {
            maxlength: u32::MAX,
            tlength: 1920, // ~60 ms buffer latency
            prebuf: u32::MAX,
            minreq: 640,  // 20 ms frame
            fragsize: u32::MAX,
        };

        let mut error = 0;
        let handle = unsafe {
            pa_simple_new(
                ptr::null(),
                name.as_ptr(),
                PaStreamDirection::Playback,
                dev_ptr,
                stream.as_ptr(),
                &ss,
                ptr::null(),
                &attr,
                &mut error,
            )
        };

        if handle.is_null() {
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str).to_string_lossy().into_owned()
            };
            return Err(format!("pa_simple_new (playback) failed: {}", msg));
        }

        Ok(Self { handle })
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut error = 0;
        let res = unsafe {
            pa_simple_write(self.handle, buf.as_ptr() as *const c_void, buf.len(), &mut error)
        };
        if res < 0 {
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str).to_string_lossy().into_owned()
            };
            Err(format!("pa_simple_write failed: {}", msg))
        } else {
            Ok(())
        }
    }
}

impl Drop for PulsePlayer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { pa_simple_free(self.handle) };
        }
    }
}
