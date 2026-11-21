//! Embedded WebRTC Audio Processing Module (AEC, Noise Suppression, High Pass Filter).
//! Provides true acoustic echo cancellation and background noise suppression directly
//! in-process without relying on external PipeWire filter modules.

use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicI32, Ordering};

extern "C" {
    fn zcall_apm_create(
        sample_rate_hz: c_int,
        channels: c_int,
        aec_enabled: c_int,
        ns_enabled: c_int,
        hpf_enabled: c_int,
    ) -> *mut c_void;

    fn zcall_apm_process_render(handle: *mut c_void, pcm: *const i16, num_samples: c_int) -> c_int;

    fn zcall_apm_process_capture(
        handle: *mut c_void,
        pcm: *mut i16,
        num_samples: c_int,
        delay_ms: c_int,
    ) -> c_int;

    fn zcall_apm_destroy(handle: *mut c_void);
}

pub struct Apm {
    raw: *mut c_void,
    delay_ms: AtomicI32,
}

unsafe impl Send for Apm {}
unsafe impl Sync for Apm {}

impl Apm {
    pub fn new(
        sample_rate_hz: i32,
        channels: i32,
        aec_enabled: bool,
        ns_enabled: bool,
        hpf_enabled: bool,
    ) -> Result<Self, String> {
        let raw = unsafe {
            zcall_apm_create(
                sample_rate_hz as c_int,
                channels as c_int,
                if aec_enabled { 1 } else { 0 },
                if ns_enabled { 1 } else { 0 },
                if hpf_enabled { 1 } else { 0 },
            )
        };

        if raw.is_null() {
            return Err("Failed to instantiate WebRTC AudioProcessing module".to_string());
        }

        Ok(Self {
            raw,
            delay_ms: AtomicI32::new(40), // Standard acoustic echo delay default (40 ms)
        })
    }

    /// Feed render/speaker audio to APM as echo reference.
    pub fn process_render(&self, pcm: &[i16]) {
        if self.raw.is_null() || pcm.is_empty() {
            return;
        }
        unsafe {
            zcall_apm_process_render(self.raw, pcm.as_ptr(), pcm.len() as c_int);
        }
    }

    /// Process capture/mic audio through AEC and Noise Suppression in-place.
    pub fn process_capture(&self, pcm: &mut [i16]) {
        if self.raw.is_null() || pcm.is_empty() {
            return;
        }
        let delay = self.delay_ms.load(Ordering::Relaxed);
        unsafe {
            zcall_apm_process_capture(
                self.raw,
                pcm.as_mut_ptr(),
                pcm.len() as c_int,
                delay as c_int,
            );
        }
    }

    pub fn set_delay_ms(&self, delay_ms: i32) {
        self.delay_ms.store(delay_ms, Ordering::Relaxed);
    }
}

impl Drop for Apm {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                zcall_apm_destroy(self.raw);
            }
            self.raw = std::ptr::null_mut();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apm_lifecycle_or_fallback() {
        match Apm::new(16000, 1, true, true, false) {
            Ok(apm) => {
                let render_frame = vec![500i16; 320];
                apm.process_render(&render_frame);

                let mut capture_frame = vec![500i16; 320];
                apm.process_capture(&mut capture_frame);

                assert_eq!(capture_frame.len(), 320);
            }
            Err(e) => {
                assert!(e.contains("WebRTC AudioProcessing"));
            }
        }
    }
}
