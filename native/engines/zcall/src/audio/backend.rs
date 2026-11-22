//! Native PulseAudio / PipeWire Simple API interface.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::process::Command;
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

#[link(name = "pulse-simple")]
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
}

#[link(name = "zcall_pulse_vol", kind = "static")]
unsafe extern "C" {
    fn zcall_pulse_get_source_info(
        source_name: *const c_char,
        out_pct: *mut c_int,
        out_muted: *mut c_int,
        out_resolved: *mut c_char,
        resolved_cap: c_int,
    ) -> c_int;
    fn zcall_pulse_get_source_volume(source_name: *const c_char, out_pct: *mut c_int) -> c_int;
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

/// Read the current PulseAudio/PipeWire source volume, mute state and resolved source name.
pub fn pulse_get_source_info(source: Option<&str>) -> Option<PulseSourceInfo> {
    let source_c = source
        .filter(|s| !s.is_empty())
        .and_then(|s| CString::new(s).ok());
    let source_ptr = source_c.as_ref().map_or(ptr::null(), |s| s.as_ptr());
    let mut pct = 0;
    let mut muted = 0;
    let mut resolved = [0 as c_char; 512];
    let rc = unsafe {
        zcall_pulse_get_source_info(
            source_ptr,
            &mut pct,
            &mut muted,
            resolved.as_mut_ptr(),
            resolved.len() as c_int,
        )
    };
    if rc != 0 {
        return None;
    }
    let resolved_name = unsafe {
        let value = std::ffi::CStr::from_ptr(resolved.as_ptr())
            .to_string_lossy()
            .into_owned();
        (!value.is_empty()).then_some(value)
    };
    Some(PulseSourceInfo {
        pct: pct.clamp(0, 150) as u8,
        muted: muted != 0,
        resolved_name,
    })
}

/// Set hardware/PulseAudio/PipeWire source volume (0..=100%) directly via libpulse.
pub fn pulse_set_source_volume(source: Option<&str>, percent: u8) -> Result<(), String> {
    let source_c = source
        .filter(|name| !name.is_empty())
        .and_then(|name| CString::new(name).ok());
    let source_ptr = source_c.as_ref().map_or(ptr::null(), |name| name.as_ptr());
    let result = unsafe { zcall_pulse_set_source_volume(source_ptr, percent.min(100) as c_int) };
    if result == 0 {
        Ok(())
    } else {
        Err("libpulse source volume update failed".into())
    }
}

/// Set the hardware/PulseAudio/PipeWire microphone mute state.
pub fn pulse_set_source_mute(source: Option<&str>, mute: bool) -> Result<(), String> {
    let source_c = source
        .filter(|s| !s.is_empty())
        .and_then(|s| CString::new(s).ok());
    let source_ptr = source_c.as_ref().map_or(ptr::null(), |s| s.as_ptr());
    let rc = unsafe { zcall_pulse_set_source_mute(source_ptr, i32::from(mute)) };
    if rc == 0 {
        Ok(())
    } else {
        Err("libpulse source mute update failed".into())
    }
}

/// Set source mute using libpulse, with pactl as a compatibility fallback.
pub fn set_source_mute(source: Option<&str>, mute: bool) -> Result<(), String> {
    if pulse_set_source_mute(source, mute).is_ok() {
        return Ok(());
    }
    let source_name = source
        .filter(|s| !s.is_empty())
        .unwrap_or("@DEFAULT_SOURCE@");
    let status = std::process::Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["set-source-mute", source_name, if mute { "1" } else { "0" }])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|error| format!("could not run pactl: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("PulseAudio/PipeWire rejected source mute update".into())
    }
}

/// Normalize this process's PulseAudio/PipeWire capture stream to unity and unmuted.
pub fn pulse_normalize_own_stream() {
    unsafe {
        let _ = zcall_pulse_normalize_own_stream(std::process::id() as c_int);
    }
}

#[link(name = "pulse")]
extern "C" {
    fn pa_strerror(error: c_int) -> *const c_char;
}

/// Read PulseAudio/PipeWire source volume through libpulse, falling back to pactl.
pub fn get_source_volume(source: Option<&str>) -> Option<u8> {
    let source_c = source
        .filter(|name| !name.is_empty())
        .and_then(|name| CString::new(name).ok());
    let source_ptr = source_c.as_ref().map_or(ptr::null(), |name| name.as_ptr());
    let mut percent = 0;
    let result = unsafe { zcall_pulse_get_source_volume(source_ptr, &mut percent) };
    if result == 0 {
        return Some(percent.clamp(0, 150) as u8);
    }
    let source_name = source
        .filter(|name| !name.is_empty())
        .unwrap_or("@DEFAULT_SOURCE@");
    let output = std::process::Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["get-source-volume", source_name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let percent = value
        .split('%')
        .next()?
        .split('/')
        .next_back()?
        .trim()
        .parse::<u16>()
        .ok()?;
    Some(percent.min(150) as u8)
}

/// Read-only startup diagnostics for the selected PulseAudio/PipeWire devices.
/// This mirrors the hardware probe from PR #220 without making audio startup
/// depend on pactl being installed or the server being available.
pub fn probe_audio_hardware(speaker: Option<&str>, microphone: Option<&str>) {
    let run = |args: &[&str]| -> Option<String> {
        let output = Command::new("pactl")
            .env("LC_ALL", "C")
            .args(args)
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };

    let Some(info) = run(&["info"]) else {
        crate::zcall_log!(
            "[zcall-rust] hw: audio backend probe unavailable (pactl/server not ready)"
        );
        return;
    };
    let backend = if info.to_ascii_lowercase().contains("pipewire") {
        "PulseAudio/PipeWire"
    } else {
        "PulseAudio"
    };
    crate::zcall_log!("[zcall-rust] hw: audio backend={backend}");
    crate::zcall_log!(
        "[zcall-rust] hw: target speaker='{}', mic='{}'",
        speaker.filter(|name| !name.is_empty()).unwrap_or("default"),
        microphone
            .filter(|name| !name.is_empty())
            .unwrap_or("default")
    );

    for (kind, label) in [("sinks", "sink"), ("sources", "source")] {
        if let Some(output) = run(&["list", kind, "short"]) {
            for line in output
                .lines()
                .filter(|line| line.to_ascii_lowercase().contains("bluez"))
            {
                crate::zcall_log!(
                    "[zcall-rust] hw: detected bluetooth {label}: {}",
                    line.trim()
                );
                if line.contains("headset-head-unit")
                    || line.contains("headset_head_unit")
                    || line.contains("handsfree")
                    || line.contains("hfp")
                    || line.contains("hsp")
                {
                    crate::zcall_log!("[zcall-rust] hw: bluetooth HFP/HSP speech profile active");
                } else if line.contains("a2dp") {
                    crate::zcall_log!("[zcall-rust] hw: bluetooth A2DP profile active");
                }
            }
        }
        if let Some(output) = run(&["list", kind]) {
            for line in output.lines().filter(|line| {
                line.contains("Sample Specification:") || line.contains("Sample Rate:")
            }) {
                crate::zcall_log!("[zcall-rust] hw: {label} {}", line.trim());
            }
        }
    }

    if let Some(cards) = run(&["list", "cards"]) {
        for line in cards
            .lines()
            .filter(|line| line.contains("Active Profile:"))
        {
            let profile = line.trim();
            if profile.to_ascii_lowercase().contains("headset")
                || profile.to_ascii_lowercase().contains("handsfree")
                || profile.to_ascii_lowercase().contains("hfp")
                || profile.to_ascii_lowercase().contains("hsp")
                || profile.to_ascii_lowercase().contains("a2dp")
            {
                crate::zcall_log!("[zcall-rust] hw: bluetooth profile {profile}");
            }
        }
    }
}

/// Set PulseAudio/PipeWire source volume. The UI slider is capped at 100%.
pub fn set_source_volume(source: Option<&str>, percent: u8) -> Result<(), String> {
    let percent = percent.min(100);
    if pulse_set_source_volume(source, percent).is_ok() {
        return Ok(());
    }
    let source_name = source
        .filter(|name| !name.is_empty())
        .unwrap_or("@DEFAULT_SOURCE@");
    let raw = (65_536.0f64 * f64::from(percent) / 100.0).round() as u32;
    let status = std::process::Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["set-source-volume", source_name, &raw.to_string()])
        .status()
        .map_err(|error| format!("could not run pactl: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("PulseAudio/PipeWire rejected source volume update".into())
    }
}

pub struct PulseRecorder {
    handle: *mut c_void,
}

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
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(format!("pa_simple_new (record) failed: {}", msg));
        }

        Ok(Self { handle })
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<(), String> {
        let mut error = 0;
        let res = unsafe {
            pa_simple_read(
                self.handle,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                &mut error,
            )
        };
        if res < 0 {
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
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
        let dev_cstr = device.and_then(|d| {
            if d.is_empty() {
                None
            } else {
                CString::new(d).ok()
            }
        });
        let dev_ptr = dev_cstr.as_ref().map(|d| d.as_ptr()).unwrap_or(ptr::null());

        let ss = PaSampleSpec {
            format: PaSampleFormat::S16Le,
            rate: 16000,
            channels: 1,
        };

        let attr = PaBufferAttr {
            maxlength: u32::MAX,
            tlength: 1920, // 60 ms hardware-clock queue, as in PR #220
            prebuf: u32::MAX,
            minreq: u32::MAX,
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
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
            };
            return Err(format!("pa_simple_new (playback) failed: {}", msg));
        }

        Ok(Self { handle })
    }

    pub fn write(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut error = 0;
        let res = unsafe {
            pa_simple_write(
                self.handle,
                buf.as_ptr() as *const c_void,
                buf.len(),
                &mut error,
            )
        };
        if res < 0 {
            let msg = unsafe {
                let err_str = pa_strerror(error);
                std::ffi::CStr::from_ptr(err_str)
                    .to_string_lossy()
                    .into_owned()
            };
            Err(format!("pa_simple_write failed: {}", msg))
        } else {
            Ok(())
        }
    }
    pub fn latency_ms(&self) -> Option<u64> {
        if self.handle.is_null() {
            return None;
        }
        let mut error = 0;
        let latency = unsafe { pa_simple_get_latency(self.handle, &mut error) };
        (error == 0).then_some(latency / 1000)
    }
}

impl Drop for PulsePlayer {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { pa_simple_free(self.handle) };
        }
    }
}
