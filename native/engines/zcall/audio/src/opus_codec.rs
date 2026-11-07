//! Safe FFI wrappers for libopus.

use std::os::raw::{c_int, c_uchar};

pub const OPUS_APPLICATION_VOIP: c_int = 2048;
pub const OPUS_SET_BITRATE_REQUEST: c_int = 4002;
pub const OPUS_SET_COMPLEXITY_REQUEST: c_int = 4010;
pub const OPUS_SET_INBAND_FEC_REQUEST: c_int = 4012;
pub const OPUS_SET_SIGNAL_REQUEST: c_int = 4024;
pub const OPUS_SIGNAL_VOICE: c_int = 3001;

extern "C" {
    fn opus_encoder_create(
        fs: i32,
        channels: c_int,
        application: c_int,
        error: *mut c_int,
    ) -> *mut std::ffi::c_void;

    fn opus_encode(
        st: *mut std::ffi::c_void,
        pcm: *const i16,
        frame_size: c_int,
        data: *mut c_uchar,
        max_data_bytes: i32,
    ) -> i32;

    fn opus_encoder_ctl(
        st: *mut std::ffi::c_void,
        request: c_int,
        val: c_int,
    ) -> c_int;

    fn opus_encoder_destroy(st: *mut std::ffi::c_void);

    fn opus_decoder_create(
        fs: i32,
        channels: c_int,
        error: *mut c_int,
    ) -> *mut std::ffi::c_void;

    fn opus_decode(
        st: *mut std::ffi::c_void,
        data: *const c_uchar,
        len: i32,
        pcm: *mut i16,
        frame_size: c_int,
        decode_fec: c_int,
    ) -> c_int;

    fn opus_decoder_destroy(st: *mut std::ffi::c_void);
}

pub struct OpusEncoder {
    handle: *mut std::ffi::c_void,
}

unsafe impl Send for OpusEncoder {}

impl OpusEncoder {
    pub fn new(sample_rate: i32, channels: i32, bitrate_bps: i32) -> Result<Self, String> {
        let mut err = 0;
        let handle = unsafe {
            opus_encoder_create(sample_rate, channels, OPUS_APPLICATION_VOIP, &mut err)
        };
        if err != 0 || handle.is_null() {
            return Err(format!("opus_encoder_create failed: error code {}", err));
        }

        unsafe {
            opus_encoder_ctl(handle, OPUS_SET_BITRATE_REQUEST, bitrate_bps);
            opus_encoder_ctl(handle, OPUS_SET_COMPLEXITY_REQUEST, 10);
            opus_encoder_ctl(handle, OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_VOICE);
            opus_encoder_ctl(handle, OPUS_SET_INBAND_FEC_REQUEST, 1);
        }

        Ok(Self { handle })
    }

    pub fn encode(&mut self, pcm: &[i16], out_buf: &mut [u8]) -> Result<usize, String> {
        let n = unsafe {
            opus_encode(
                self.handle,
                pcm.as_ptr(),
                pcm.len() as c_int,
                out_buf.as_mut_ptr() as *mut c_uchar,
                out_buf.len() as i32,
            )
        };
        if n < 0 {
            Err(format!("opus_encode failed: error code {}", n))
        } else {
            Ok(n as usize)
        }
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { opus_encoder_destroy(self.handle) };
        }
    }
}

pub struct OpusDecoder {
    handle: *mut std::ffi::c_void,
}

unsafe impl Send for OpusDecoder {}

impl OpusDecoder {
    pub fn new(sample_rate: i32, channels: i32) -> Result<Self, String> {
        let mut err = 0;
        let handle = unsafe { opus_decoder_create(sample_rate, channels, &mut err) };
        if err != 0 || handle.is_null() {
            return Err(format!("opus_decoder_create failed: error code {}", err));
        }
        Ok(Self { handle })
    }

    pub fn decode(
        &mut self,
        payload: &[u8],
        out_pcm: &mut [i16],
        decode_fec: bool,
    ) -> Result<usize, String> {
        let (data_ptr, len) = if payload.is_empty() {
            (std::ptr::null(), 0)
        } else {
            (payload.as_ptr() as *const c_uchar, payload.len() as i32)
        };

        let n = unsafe {
            opus_decode(
                self.handle,
                data_ptr,
                len,
                out_pcm.as_mut_ptr(),
                out_pcm.len() as c_int,
                if decode_fec { 1 } else { 0 },
            )
        };
        if n < 0 {
            Err(format!("opus_decode failed: error code {}", n))
        } else {
            Ok(n as usize)
        }
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { opus_decoder_destroy(self.handle) };
        }
    }
}
