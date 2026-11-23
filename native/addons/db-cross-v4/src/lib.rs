#![allow(unsafe_code, reason = "napi FFI: JsUnknown::cast is unsafe by API")]
mod binnet;
mod container;
mod crypto;
mod tlv;

use napi::{Env, JsArrayBuffer, JsBuffer, JsFunction, JsObject, JsTypedArray, JsUnknown, Result};
use napi_derive::napi;

#[napi(js_name = "decompressAndDecryptDb")]
pub fn decompress_and_decrypt_db(env: Env) -> Result<JsObject> {
    let mut ret = env.create_object()?;
    ret.set_named_property("result", -1)?;
    ret.set_named_property(
        "err_message",
        "V1 format not fully implemented in this build",
    )?;
    Ok(ret)
}

#[napi(js_name = "decompressAndDecryptDb_V2")]
pub fn decompress_and_decrypt_db_v2(
    env: Env,
    input_path: Option<String>,
    output_path: Option<String>,
    private_key: Option<String>,
    progress_cb: Option<JsFunction>,
) -> Result<JsObject> {
    let mut ret = env.create_object()?;

    let (Some(input), Some(output), Some(key)) = (input_path, output_path, private_key) else {
        ret.set_named_property("result", -1)?;
        ret.set_named_property("inner_error", "Invalid args")?;
        ret.set_named_property("error_message", "Invalid args")?;
        return Ok(ret);
    };

    let cb_closure = move || {
        if let Some(cb) = &progress_cb {
            let empty_args: [&JsUnknown; 0] = [];
            let _ = cb.call(None, &empty_args);
        }
    };

    let r = container::process_zdb4_v2(&input, &output, &key, cb_closure);

    ret.set_named_property("result", r.code)?;
    ret.set_named_property("inner_error", r.err.clone())?;
    ret.set_named_property("error_message", r.err)?;
    Ok(ret)
}

#[napi(js_name = "parseBinNet")]
pub fn parse_bin_net(env: Env, val: Option<JsUnknown>) -> Result<JsObject> {
    let mut ret = env.create_object()?;
    ret.set_named_property("result", 0)?;
    ret.set_named_property("error_message", "")?;
    ret.set_named_property("inner_error", "")?;

    let Some(val) = val else {
        ret.set_named_property("result", 1)?;
        ret.set_named_property("error_message", "Wrong arguments")?;
        ret.set_named_property("data", env.create_object()?)?;
        return Ok(ret);
    };

    if val.is_buffer()? {
        // SAFETY: `is_buffer()` above confirmed the value is a Buffer, so the
        // unchecked cast to `JsBuffer` matches the real napi type; `val` is owned
        // by the live `env` for the whole function scope.
        let buf: JsBuffer = unsafe { val.cast() };
        let value = buf.into_value()?;
        return binnet::parse_bin_net_internal(value.as_ref(), env);
    }

    if val.is_typedarray()? {
        // SAFETY: `is_typedarray()` above confirmed the value is a TypedArray, so
        // the unchecked cast to `JsTypedArray` matches the real napi type; `val`
        // is owned by the live `env` for the whole function scope.
        let ta: JsTypedArray = unsafe { val.cast() };
        let value = ta.into_value()?;
        return binnet::parse_bin_net_internal(value.as_ref(), env);
    }

    // SAFETY: the cast only reinterprets the napi handle; `into_value()` calls
    // `napi_get_arraybuffer_info`, which returns an error for non-ArrayBuffers.
    if let Ok(ab) = unsafe { val.cast::<JsArrayBuffer>() }.into_value() {
        return binnet::parse_bin_net_internal(ab.as_ref(), env);
    }

    ret.set_named_property("result", 1)?;
    ret.set_named_property("error_message", "Wrong arguments")?;
    ret.set_named_property("data", env.create_object()?)?;
    Ok(ret)
}
