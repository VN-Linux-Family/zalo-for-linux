#![cfg(target_os = "linux")]
#![allow(unsafe_code, reason = "napi cast")]

use napi::{Error, JsUnknown, Result, Status, ValueType};
use napi_derive::napi;

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

    if path_str.as_bytes().contains(&0) {
        return Err(Error::new(
            Status::InvalidArg,
            "DISKUSAGE_INVALID_ARG_TYPE: Path contains null byte".to_string(),
        ));
    }

    let stat = rustix::fs::statvfs(std::path::Path::new(&path_str)).map_err(|_| {
        Error::new(
            Status::GenericFailure,
            "DISKUSAGE_RUNTIME_ERROR: Get diskusage failed".to_string(),
        )
    })?;

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
