#![cfg(target_os = "linux")]
#![allow(unsafe_code, reason = "napi cast and libc statvfs FFI")]

use napi::{Error, JsUnknown, Result, Status, ValueType};
use napi_derive::napi;
use std::ffi::CString;
use std::mem;

#[napi(object)]
pub struct DiskUsage {
    pub available: f64,
    pub free: f64,
    pub total: f64,
}

#[napi(ts_args_type = "path: string")]
#[allow(
    clippy::cast_precision_loss,
    reason = "disk sizes in bytes as f64; precision below 2^52 is irrelevant"
)]
pub fn get_disk_usage(path: Option<JsUnknown>) -> Result<DiskUsage> {
    let Some(path_arg) = path else {
        return Err(Error::new(
            Status::InvalidArg,
            "DISKUSAGE_WRONG_NUMBER_OF_ARGS".to_string(),
        ));
    };

    if path_arg.get_type()? != ValueType::String {
        return Err(Error::new(
            Status::InvalidArg,
            "DISKUSAGE_WRONG_NUMBER_OF_ARGS:  The \"path\" argument must be one of type string"
                .to_string(),
        ));
    }

    // SAFETY: `get_type()` above returned `ValueType::String` (else we returned
    // `InvalidArg`), so the unchecked cast to `JsString` matches the real napi
    // type; `path_arg` lives in the caller's live `env`.
    let js_string = unsafe { path_arg.cast::<napi::JsString>() };
    let path_str = js_string.into_utf8()?.into_owned()?;

    let c_path = CString::new(path_str).map_err(|_| {
        Error::new(
            Status::InvalidArg,
            "DISKUSAGE_INVALID_ARG_TYPE: Path contains null byte".to_string(),
        )
    })?;

    // SAFETY: `libc::statvfs` is plain-old-data; an all-zero byte pattern is a
    // valid value of the struct, and it is fully overwritten by the syscall.
    let mut stat: libc::statvfs = unsafe { mem::zeroed() };
    // SAFETY: `c_path` is a NUL-terminated `CString` that outlives this call, and
    // `&mut stat` points at a live, aligned, writable `statvfs`; the struct is
    // only read after `res` was checked to be 0.
    let res = unsafe { libc::statvfs(c_path.as_ptr(), &raw mut stat) };

    if res != 0 {
        return Err(Error::new(
            Status::GenericFailure,
            "DISKUSAGE_RUNTIME_ERROR: Get diskusage failed".to_string(),
        ));
    }

    let factor = if stat.f_frsize > 0 {
        stat.f_frsize
    } else {
        stat.f_bsize
    } as f64;

    let disk_info = DiskUsage {
        available: stat.f_bavail as f64 * factor,
        free: stat.f_bfree as f64 * factor,
        total: stat.f_blocks as f64 * factor,
    };

    Ok(disk_info)
}
