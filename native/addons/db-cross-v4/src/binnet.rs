use crate::tlv::{is_likely_utf8_printable, try_decode_text_smart, TlvBoxLite};
use napi::{Env, JsObject, Result};

/// Zalo BinNet TLV tag constants (describing rich-message metadata fields)
pub mod tag {
    // Top-level message TLV tags
    pub const PROPERTY: u32 = 0x05;
    pub const ATTACHMENTS: u32 = 0x06;
    pub const QUOTE: u32 = 0x07;
    pub const MENTIONS: u32 = 0x08;
    pub const QUOTE_STATUS: u32 = 0x09;
    pub const DATA_SOURCE: u32 = 0x0a;
    pub const DATA_CONTENT: u32 = 0x0b;

    // Property sub-tags (nested inside tag 0x05)
    pub const PROP_SIZE: u32 = 0x1e;
    pub const PROP_SUB_TYPE: u32 = 0x1f;
    pub const PROP_TYPE: u32 = 0x20;
    pub const PROP_EXT: u32 = 0x21;
    pub const PROP_COLOR: u32 = 0x22;

    // Attachment sub-tags (nested inside tag 0x06)
    pub const ATTACH_TYPE: u32 = 0x28;
    pub const ATTACH_CAT_ID: u32 = 0x29;
    pub const ATTACH_ID: u32 = 0x2a;
    pub const ATTACH_EXT_INFO: u32 = 0x2b;
    pub const ATTACH_CHILD_NUMBER: u32 = 0x2c;
    pub const ATTACH_ACTION: u32 = 0x2d;
    pub const ATTACH_PARAMS: u32 = 0x2e;
    pub const ATTACH_TITLE: u32 = 0x2f;
    pub const ATTACH_HREF: u32 = 0x30;
    pub const ATTACH_THUMB: u32 = 0x31;
    pub const ATTACH_DESCRIPTION: u32 = 0x32;
    pub const ATTACH_SIZE: u32 = 0x33;
    pub const ATTACH_EXT: u32 = 0x34;
    pub const ATTACH_SUB_TYPE: u32 = 0x35;
    pub const ATTACH_COLOR: u32 = 0x36;
    pub const ATTACH_PROPERTY: u32 = 0x37;
    pub const ATTACH_DATA_SOURCE: u32 = 0x40;
    pub const ATTACH_DATA: u32 = 0x41;
    pub const ATTACH_DATA_EXTRA: u32 = 0x42;
    pub const ATTACH_THUMB_URL: u32 = 0x43;
    pub const ATTACH_ORIGINAL_URL: u32 = 0x44;

    // Quote sub-tags (nested inside tag 0x07)
    pub const QUOTE_OWNER_ID: u32 = 0x50;
    pub const QUOTE_CLI_MSG_ID: u32 = 0x51;
    pub const QUOTE_GLOBAL_MSG_ID: u32 = 0x52;
    pub const QUOTE_CLI_MSG_TYPE: u32 = 0x53;
    pub const QUOTE_TS: u32 = 0x54;
    pub const QUOTE_TTL: u32 = 0x55;
    pub const QUOTE_MSG: u32 = 0x56;
    pub const QUOTE_ATTACH: u32 = 0x57;
    pub const QUOTE_STATUS_TEXT: u32 = 0x58;
    pub const QUOTE_FROM_D: u32 = 0x5a;

    // Mention sub-tags (nested inside tag 0x08)
    pub const MENTION_UID: u32 = 0x65;
    pub const MENTION_POS: u32 = 0x66;
    pub const MENTION_LEN: u32 = 0x67;
    pub const MENTION_TYPE: u32 = 0x68;
}

pub fn parse_attach_chat_msg_lite(buf: &[u8], env: &Env) -> Option<JsObject> {
    let mut tlv = TlvBoxLite::new();
    if tlv.parse(buf).is_err() {
        return None;
    }

    let mut out = env.create_object().ok()?;
    let mut has_props = false;

    for k in tlv.get_all_keys() {
        let v = match tlv.get_bytes_value(k) {
            Some(v) => v,
            None => continue,
        };

        match k {
            tag::ATTACH_TYPE => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("type", s);
                    has_props = true;
                }
            }
            tag::ATTACH_CAT_ID => {
                if let Some(iv) = tlv.get_int_value(k) {
                    let _ = out.set_named_property("catId", iv);
                    has_props = true;
                }
            }
            tag::ATTACH_ID => {
                if let Some(iv) = tlv.get_int_value(k) {
                    let _ = out.set_named_property("id", iv);
                    has_props = true;
                }
            }
            tag::ATTACH_EXT_INFO => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("extInfo", s);
                    has_props = true;
                }
            }
            tag::ATTACH_CHILD_NUMBER => {
                if let Some(iv) = tlv.get_int_value(k) {
                    let _ = out.set_named_property("childNumber", iv);
                    has_props = true;
                }
            }
            tag::ATTACH_ACTION => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("action", s);
                    has_props = true;
                }
            }
            tag::ATTACH_PARAMS => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("params", s);
                    has_props = true;
                }
            }
            tag::ATTACH_TITLE => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("title", s);
                    has_props = true;
                }
            }
            tag::ATTACH_HREF => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("href", s);
                    has_props = true;
                }
            }
            tag::ATTACH_THUMB => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("thumb", s);
                    has_props = true;
                }
            }
            tag::ATTACH_DESCRIPTION => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("description", s);
                    has_props = true;
                }
            }
            tag::ATTACH_SIZE => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("size", s);
                    has_props = true;
                }
            }
            tag::ATTACH_EXT => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("ext", s);
                    has_props = true;
                }
            }
            tag::ATTACH_SUB_TYPE => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("subType", s);
                    has_props = true;
                }
            }
            tag::ATTACH_COLOR => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("color", s);
                    has_props = true;
                }
            }
            tag::ATTACH_PROPERTY => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("property", s);
                    has_props = true;
                }
            }
            tag::ATTACH_DATA_SOURCE => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("dataSource", s);
                    has_props = true;
                }
            }
            tag::ATTACH_DATA => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("data", s);
                    has_props = true;
                }
            }
            tag::ATTACH_DATA_EXTRA => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("dataExtra", s);
                    has_props = true;
                }
            }
            tag::ATTACH_THUMB_URL => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("thumbUrl", s);
                    has_props = true;
                }
            }
            tag::ATTACH_ORIGINAL_URL => {
                if let Some(s) = try_decode_text_smart(v) {
                    let _ = out.set_named_property("originalUrl", s);
                    has_props = true;
                }
            }
            _ => {}
        }
    }
    if has_props {
        Some(out)
    } else {
        None
    }
}

pub fn parse_reference_or_quote_lite(buf: &[u8], env: &Env) -> Option<JsObject> {
    let mut tlv = TlvBoxLite::new();
    if tlv.parse(buf).is_err() {
        return None;
    }

    let mut out = env.create_object().ok()?;
    let mut has_props = false;

    if let Some(i32_val) = tlv.get_int_value(tag::QUOTE_OWNER_ID) {
        let _ = out.set_named_property("ownerId", i32_val);
        has_props = true;
    }
    if let Some(i64_val) = tlv.get_int64_value(tag::QUOTE_CLI_MSG_ID) {
        let _ = out.set_named_property("cliMsgId", i64_val as f64);
        has_props = true;
    }
    if let Some(i64_val) = tlv.get_int64_value(tag::QUOTE_GLOBAL_MSG_ID) {
        let _ = out.set_named_property("globalMsgId", i64_val as f64);
        has_props = true;
    }
    if let Some(i32_val) = tlv.get_int_value(tag::QUOTE_CLI_MSG_TYPE) {
        let _ = out.set_named_property("cliMsgType", i32_val);
        has_props = true;
    }
    if let Some(i64_val) = tlv.get_int64_value(tag::QUOTE_TS) {
        let _ = out.set_named_property("ts", i64_val as f64);
        has_props = true;
    }
    if let Some(i64_val) = tlv.get_int64_value(tag::QUOTE_TTL) {
        let _ = out.set_named_property("ttl", i64_val as f64);
        has_props = true;
    }

    if let Some(v) = tlv.get_bytes_value(tag::QUOTE_MSG) {
        if let Some(sv) = try_decode_text_smart(v) {
            let _ = out.set_named_property("msg", sv);
            has_props = true;
        }
    }
    if let Some(v) = tlv.get_bytes_value(tag::QUOTE_ATTACH) {
        if let Some(sv) = try_decode_text_smart(v) {
            let _ = out.set_named_property("attach", sv);
            has_props = true;
        }
    }
    if let Some(v) = tlv.get_bytes_value(tag::QUOTE_STATUS_TEXT) {
        if let Some(sv) = try_decode_text_smart(v) {
            let _ = out.set_named_property("quoteStatus", sv);
            has_props = true;
        }
    }
    if let Some(v) = tlv.get_bytes_value(tag::QUOTE_FROM_D) {
        if let Some(sv) = try_decode_text_smart(v) {
            let _ = out.set_named_property("fromD", sv);
            has_props = true;
        }
    }

    if has_props {
        Some(out)
    } else {
        None
    }
}

pub fn parse_mention_lite(buf: &[u8], env: &Env) -> Option<JsObject> {
    let mut tlv = TlvBoxLite::new();
    if tlv.parse(buf).is_err() {
        return None;
    }

    let mut out = env.create_object().ok()?;
    let mut has_props = false;

    if let Some(i) = tlv.get_int_value(tag::MENTION_UID) {
        let _ = out.set_named_property("uid", i);
        has_props = true;
    }
    if let Some(i) = tlv.get_int_value(tag::MENTION_POS) {
        let _ = out.set_named_property("pos", i);
        has_props = true;
    }
    if let Some(i) = tlv.get_int_value(tag::MENTION_LEN) {
        let _ = out.set_named_property("len", i);
        has_props = true;
    }
    if let Some(i) = tlv.get_int_value(tag::MENTION_TYPE) {
        let _ = out.set_named_property("type", i);
        has_props = true;
    }

    if has_props {
        Some(out)
    } else {
        None
    }
}

pub fn parse_bin_net_internal(p: &[u8], env: &Env) -> Result<JsObject> {
    let mut ret = env.create_object()?;
    ret.set_named_property("result", 0)?;
    ret.set_named_property("error_message", "")?;
    ret.set_named_property("inner_error", "")?;

    if p.is_empty() {
        ret.set_named_property("result", 2)?;
        ret.set_named_property("error_message", "Something went wrong while parsing")?;
        ret.set_named_property("inner_error", "Empty BinNet")?;
        ret.set_named_property("data", env.create_object()?)?;
        return Ok(ret);
    }

    let mut tlv = TlvBoxLite::new();
    if let Err(err) = tlv.parse(p) {
        ret.set_named_property("result", 2)?;
        ret.set_named_property("error_message", "Something went wrong while parsing")?;
        ret.set_named_property("inner_error", err)?;
        ret.set_named_property("data", env.create_object()?)?;
        return Ok(ret);
    }

    let mut debug = env.create_object()?;
    debug.set_named_property("inputLen", p.len() as f64)?;
    debug.set_named_property(
        "tlvEndian",
        if tlv.little_endian_used { "le" } else { "be" },
    )?;

    let all_keys = tlv.get_all_keys();
    let mut top_keys = env.create_array_with_length(all_keys.len())?;
    for (i, &k) in all_keys.iter().enumerate() {
        let num = env.create_uint32(k)?;
        top_keys.set_element(i as u32, num)?;
    }
    debug.set_named_property("topKeys", top_keys)?;
    debug.set_named_property("topKeyCount", all_keys.len() as f64)?;

    let mut attach_candidates = env.create_array_with_length(0)?;
    let mut attach_candidate_idx = 0u32;

    let mut data = env.create_object()?;
    let mut attachs = env.create_array_with_length(0)?;
    let mut attach_idx = 0u32;
    let mut mentions = env.create_array_with_length(0)?;
    let mut mention_idx = 0u32;

    for &k in &all_keys {
        let vals = match tlv.values.get(&k) {
            Some(v) if !v.is_empty() => v,
            _ => continue,
        };

        match k {
            tag::PROPERTY => {
                let v = &vals[0];
                let mut ptlv = TlvBoxLite::new();
                if ptlv.parse(v).is_ok() {
                    let mut prop = env.create_object()?;
                    let mut has_prop = false;
                    if let Some(iv) = ptlv.get_int_value(tag::PROP_SIZE) {
                        prop.set_named_property("size", iv)?;
                        has_prop = true;
                    }
                    if let Some(iv) = ptlv.get_int_value(tag::PROP_COLOR) {
                        prop.set_named_property("color", iv)?;
                        has_prop = true;
                    }
                    if let Some(iv) = ptlv.get_int_value(tag::PROP_TYPE) {
                        prop.set_named_property("type", iv)?;
                        has_prop = true;
                    }
                    if let Some(iv) = ptlv.get_int_value(tag::PROP_SUB_TYPE) {
                        prop.set_named_property("subType", iv)?;
                        has_prop = true;
                    }
                    if let Some(pv) = ptlv.get_bytes_value(tag::PROP_EXT) {
                        if let Some(sv) = try_decode_text_smart(pv) {
                            prop.set_named_property("ext", sv)?;
                            has_prop = true;
                        }
                    }
                    if has_prop {
                        data.set_named_property("property", prop)?;
                    }
                }
            }
            tag::ATTACHMENTS => {
                for v in vals {
                    if v.len() < 8 {
                        continue;
                    }
                    let att_opt = parse_attach_chat_msg_lite(v, env);
                    let accepted = att_opt.is_some();

                    if attach_candidate_idx < 32 {
                        let mut cand = env.create_object()?;
                        cand.set_named_property("source", "key_0x06")?;
                        cand.set_named_property("len", v.len() as f64)?;
                        cand.set_named_property("accepted", accepted)?;

                        let mut key_arr = env.create_array_with_length(0)?;
                        if let Some(att) = &att_opt {
                            if let Ok(prop_names) = att.get_property_names() {
                                let len = prop_names.get_array_length()?;
                                for i in 0..len.min(16) {
                                    let name_val: napi::JsString = prop_names.get_element(i)?;
                                    key_arr.set_element(i, name_val)?;
                                }
                            }
                        }
                        cand.set_named_property("keys", key_arr)?;
                        attach_candidates.set_element(attach_candidate_idx, cand)?;
                        attach_candidate_idx += 1;
                    }

                    if let Some(att) = att_opt {
                        attachs.set_element(attach_idx, att)?;
                        attach_idx += 1;
                    }
                }
            }
            tag::QUOTE => {
                let v = &vals[0];
                if let Some(q) = parse_reference_or_quote_lite(v, env) {
                    data.set_named_property("quote", q)?;
                }
            }
            tag::MENTIONS => {
                for v in vals {
                    if let Some(m) = parse_mention_lite(v, env) {
                        mentions.set_element(mention_idx, m)?;
                        mention_idx += 1;
                    }
                }
            }
            tag::QUOTE_STATUS => {
                if let Some(qst) = tlv.get_int_value(tag::QUOTE_STATUS) {
                    data.set_named_property("quoteStatus", qst)?;
                }
            }
            tag::DATA_SOURCE => {
                let v = &vals[0];
                if is_likely_utf8_printable(v) {
                    let s = String::from_utf8_lossy(v).into_owned();
                    data.set_named_property("dataSource", s)?;
                }
            }
            tag::DATA_CONTENT => {
                let v = &vals[0];
                if is_likely_utf8_printable(v) {
                    let s = String::from_utf8_lossy(v).into_owned();
                    data.set_named_property("data", s)?;
                }
            }
            _ => {}
        }
    }

    if attach_idx > 0 {
        data.set_named_property("attachs", attachs)?;
    }
    if mention_idx > 0 {
        data.set_named_property("mentions", mentions)?;
    }

    ret.set_named_property("data", data)?;
    debug.set_named_property("attachCandidates", attach_candidates)?;
    debug.set_named_property("attachCandidateCount", attach_candidate_idx as f64)?;
    debug.set_named_property("attachCount", attach_idx as f64)?;
    ret.set_named_property("debug", debug)?;

    Ok(ret)
}
