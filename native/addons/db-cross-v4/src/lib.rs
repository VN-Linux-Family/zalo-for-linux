mod binnet;
mod container;
mod crypto;
mod tlv;

use napi::bindgen_prelude::{
    ArrayBuffer, BufferSlice, FromNapiValue, Function, JsObjectValue, JsValue as _, Object,
    TypedArray, Unknown,
};
use napi::{Env, Result};
use napi_derive::napi;

#[napi(js_name = "decompressAndDecryptDb")]
pub fn decompress_and_decrypt_db(env: &Env) -> Result<Object<'_>> {
    let mut ret = Object::new(env)?;
    ret.set_named_property("result", -1)?;
    ret.set_named_property(
        "err_message",
        "V1 format not fully implemented in this build",
    )?;
    Ok(ret)
}

#[napi(js_name = "decompressAndDecryptDb_V2")]
pub fn decompress_and_decrypt_db_v2<'env>(
    env: &'env Env,
    input_path: Option<String>,
    output_path: Option<String>,
    private_key: Option<String>,
    progress_cb: Option<Function<'env, (), ()>>,
) -> Result<Object<'env>> {
    let mut ret = Object::new(env)?;

    let (Some(input), Some(output), Some(key)) = (input_path, output_path, private_key) else {
        ret.set_named_property("result", -1)?;
        ret.set_named_property("inner_error", "Invalid args")?;
        ret.set_named_property("error_message", "Invalid args")?;
        return Ok(ret);
    };

    let cb_closure = move || {
        if let Some(cb) = &progress_cb {
            let _ = cb.call(());
        }
    };

    let r = container::process_zdb4_v2(&input, &output, &key, cb_closure);

    ret.set_named_property("result", r.code)?;
    ret.set_named_property("inner_error", r.err.clone())?;
    ret.set_named_property("error_message", r.err)?;
    Ok(ret)
}

#[napi(js_name = "parseBinNet")]
pub fn parse_bin_net<'env>(env: &'env Env, val: Option<Unknown<'env>>) -> Result<Object<'env>> {
    let mut ret = Object::new(env)?;
    ret.set_named_property("result", 0)?;
    ret.set_named_property("error_message", "")?;
    ret.set_named_property("inner_error", "")?;

    let Some(val) = val else {
        ret.set_named_property("result", 1)?;
        ret.set_named_property("error_message", "Wrong arguments")?;
        ret.set_named_property("data", Object::new(env)?)?;
        return Ok(ret);
    };

    if val.is_buffer()? {
        let buf = BufferSlice::from_unknown(val)?;
        return binnet::parse_bin_net_internal(&buf, env);
    }

    if val.is_typedarray()? {
        let ta = TypedArray::from_unknown(val)?;
        return binnet::parse_bin_net_internal(&ta.arraybuffer, env);
    }

    if let Ok(ab) = ArrayBuffer::from_unknown(val) {
        return binnet::parse_bin_net_internal(&ab, env);
    }

    ret.set_named_property("result", 1)?;
    ret.set_named_property("error_message", "Wrong arguments")?;
    ret.set_named_property("data", Object::new(env)?)?;
    Ok(ret)
}
