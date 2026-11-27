mod image;
mod service;
mod types;

use napi::bindgen_prelude::*;
use napi_derive::napi;
use types::{
    BitmapToJxlOptions, DecompressMultiOptions, DecompressMultiOutput, GetJxlInfoOptions, JxlInfo,
    JxlToJpegOptions, LocalPathOptions, LocalPathOutput, ResizeJxlLimitOptions, ResizeJxlOptions,
};

type JxlCallback<T> = Option<Function<'static, FnArgs<(Option<Error>, Option<T>, u32)>, ()>>;

#[napi(js_name = "moduleReady")]
pub fn module_ready() -> bool {
    true
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "napi takes ownership of JS arguments"
)]
#[napi(js_name = "getJxlInfo")]
pub fn get_jxl_info(
    input: Either<GetJxlInfoOptions, Buffer>,
    callback: JxlCallback<JxlInfo>,
) -> Result<Option<JxlInfo>> {
    let buf = match &input {
        Either::A(opts) => &opts.buffer,
        Either::B(buf) => buf,
    };
    match service::get_info(buf) {
        Ok(info) => {
            if let Some(cb) = callback {
                let _: () = cb.call((None, Some(info), 1).into())?;
                Ok(None)
            } else {
                Ok(Some(info))
            }
        }
        Err(err) => {
            if let Some(cb) = callback {
                let _: () = cb.call((Some(err), None, 0).into())?;
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "napi takes ownership of JS arguments"
)]
#[napi(js_name = "jxlToJpeg")]
pub fn jxl_to_jpeg(
    options: JxlToJpegOptions,
    callback: JxlCallback<Buffer>,
) -> Result<Option<Buffer>> {
    match service::to_jpeg(&options) {
        Ok(buf) => {
            if let Some(cb) = callback {
                let _: () = cb.call((None, Some(buf), 1).into())?;
                Ok(None)
            } else {
                Ok(Some(buf))
            }
        }
        Err(err) => {
            if let Some(cb) = callback {
                let _: () = cb.call((Some(err), None, 0).into())?;
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "napi takes ownership of JS arguments"
)]
#[napi(js_name = "bitmapToJxl")]
pub fn bitmap_to_jxl(
    options: BitmapToJxlOptions,
    callback: JxlCallback<Buffer>,
) -> Result<Option<Buffer>> {
    match service::bitmap_to_jxl(&options) {
        Ok(buf) => {
            if let Some(cb) = callback {
                let _: () = cb.call((None, Some(buf), 1).into())?;
                Ok(None)
            } else {
                Ok(Some(buf))
            }
        }
        Err(err) => {
            if let Some(cb) = callback {
                let _: () = cb.call((Some(err), None, 0).into())?;
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "napi takes ownership of JS arguments"
)]
#[napi(js_name = "resizeJxl")]
pub fn resize_jxl(
    options: ResizeJxlOptions,
    callback: JxlCallback<Buffer>,
) -> Result<Option<Buffer>> {
    match service::resize_jxl(&options) {
        Ok(buf) => {
            if let Some(cb) = callback {
                let _: () = cb.call((None, Some(buf), 1).into())?;
                Ok(None)
            } else {
                Ok(Some(buf))
            }
        }
        Err(err) => {
            if let Some(cb) = callback {
                let _: () = cb.call((Some(err), None, 0).into())?;
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

#[napi(js_name = "resizeJxlLimit")]
pub fn resize_jxl_limit(
    options: ResizeJxlLimitOptions,
    callback: JxlCallback<Buffer>,
) -> Result<Option<Buffer>> {
    let resize_opts = ResizeJxlOptions {
        buffer: options.buffer,
        width: options.width,
        height: options.height,
        max_threads: options.max_threads,
        api_version: options.api_version,
    };
    match service::resize_jxl(&resize_opts) {
        Ok(buf) => {
            if let Some(cb) = callback {
                let _: () = cb.call((None, Some(buf), 1).into())?;
                Ok(None)
            } else {
                Ok(Some(buf))
            }
        }
        Err(err) => {
            if let Some(cb) = callback {
                let _: () = cb.call((Some(err), None, 0).into())?;
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

#[napi(js_name = "jxlToJpegFromLocalPath")]
pub fn jxl_to_jpeg_from_local_path(
    options: LocalPathOptions,
    callback: JxlCallback<LocalPathOutput>,
) -> Result<Option<LocalPathOutput>> {
    match service::to_jpeg_from_local_path(options) {
        Ok(res) => {
            if let Some(cb) = callback {
                let _: () = cb.call((None, Some(res), 1).into())?;
                Ok(None)
            } else {
                Ok(Some(res))
            }
        }
        Err(err) => {
            if let Some(cb) = callback {
                let _: () = cb.call((Some(err), None, 0).into())?;
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}

#[napi(js_name = "jxlDecompressMulti")]
pub fn jxl_decompress_multi(
    options: DecompressMultiOptions,
    callback: JxlCallback<Vec<DecompressMultiOutput>>,
) -> Result<Option<Vec<DecompressMultiOutput>>> {
    match service::decompress_multi(options) {
        Ok(res) => {
            if let Some(cb) = callback {
                let _: () = cb.call((None, Some(res), 1).into())?;
                Ok(None)
            } else {
                Ok(Some(res))
            }
        }
        Err(err) => {
            if let Some(cb) = callback {
                let _: () = cb.call((Some(err), None, 0).into())?;
                Ok(None)
            } else {
                Err(err)
            }
        }
    }
}
