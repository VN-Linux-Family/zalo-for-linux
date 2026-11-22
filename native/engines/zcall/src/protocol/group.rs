//! Group/SFU control packet codecs.
///
/// These layouts are mirrored from the source-only ZCall protocol reference.
/// The group InitZRTP payload has several observed candidate layouts; callers
/// must select one explicitly instead of silently assuming a single layout.
use super::zrtp::{build_control_header, commands, msg_types, CONTROL_HEADER_LEN, MAGIC_BYTE};
use std::collections::HashSet;

const MAX_GROUP_VIDEO_QUALITY_ENTRIES: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupInitLayout {
    /// subCmd 4, zero call/peer fields.
    Zero,
    /// subCmd 6, call id followed by host byte and two config bytes.
    Sub6,
    /// subCmd 4, call id followed by zero.
    CallId,
    /// subCmd 4, call id followed by host-call UID.
    CallIdHost,
}

impl GroupInitLayout {
    pub const ALL: [Self; 4] = [Self::Zero, Self::Sub6, Self::CallId, Self::CallIdHost];

    pub fn name(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::Sub6 => "sub6",
            Self::CallId => "callId",
            Self::CallIdHost => "callIdHost",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|layout| layout.name() == value)
    }
}

fn push_u16_blob(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len().min(u16::MAX as usize) as u16).to_le_bytes());
    out.extend_from_slice(&bytes[..bytes.len().min(u16::MAX as usize)]);
}

/// Build an SFU InitZRTP request. The payload layout is chosen by the caller
/// because legacy captures have not established one universally accepted form.
/// InitZRTP for the second, independent SFU peer used by group screen share.
pub fn build_share_init_zrtp_packet(uid: u32, sess_id: &[u8]) -> Vec<u8> {
    let mut packet = build_control_header(
        msg_types::CONTROL,
        MAGIC_BYTE,
        0,
        uid,
        0,
        commands::INIT_CALL_CALLEE,
        0x04,
    )
    .to_vec();
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet.extend_from_slice(&(sess_id.len().min(u16::MAX as usize) as u16).to_le_bytes());
    packet.extend_from_slice(&sess_id[..sess_id.len().min(u16::MAX as usize)]);
    packet.extend_from_slice(&4u16.to_le_bytes());
    packet.extend_from_slice(&[0, 0xff, 1, 0]);
    packet
}

/// The share peer's keepalive has no group-extra/video-state extension.
pub fn build_share_zavi_ping_packet(uid: u32, token: u32, seq: u32, sess_id: &[u8]) -> Vec<u8> {
    let mut packet = build_control_header(
        msg_types::CONTROL,
        MAGIC_BYTE,
        seq,
        uid,
        token,
        commands::INIT_ZAVI_PING,
        0x01,
    )
    .to_vec();
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet.extend_from_slice(&0u32.to_le_bytes());
    packet.extend_from_slice(&(sess_id.len().min(u16::MAX as usize) as u16).to_le_bytes());
    packet.extend_from_slice(&sess_id[..sess_id.len().min(u16::MAX as usize)]);
    packet
}

pub fn build_group_init_zrtp_packet(
    is_host: bool,
    uid: u32,
    session_id: &[u8],
    call_id: u32,
    host_call: u32,
    video_on: bool,
    layout: GroupInitLayout,
) -> Vec<u8> {
    let sub_cmd = if layout == GroupInitLayout::Sub6 {
        0x06
    } else {
        0x04
    };
    let cmd = if is_host {
        commands::INIT_CALL_CALLER
    } else {
        commands::INIT_CALL_CALLEE
    };
    let header = build_control_header(msg_types::CONTROL, MAGIC_BYTE, 0, uid, 0, cmd, sub_cmd);
    let mut out = Vec::with_capacity(CONTROL_HEADER_LEN + session_id.len() + 24);
    out.extend_from_slice(&header);

    match layout {
        GroupInitLayout::Zero => {
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        GroupInitLayout::Sub6 => {
            out.extend_from_slice(&call_id.to_le_bytes());
            out.extend_from_slice(&[u8::from(is_host), 5, 0]);
        }
        GroupInitLayout::CallId => {
            out.extend_from_slice(&call_id.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
        }
        GroupInitLayout::CallIdHost => {
            out.extend_from_slice(&call_id.to_le_bytes());
            out.extend_from_slice(&host_call.to_le_bytes());
        }
    }

    push_u16_blob(&mut out, session_id);
    push_u16_blob(&mut out, &[0, 5, u8::from(video_on), 1]);
    out
}

/// One member's requested SFU video layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupVideoQualityRequest {
    pub uid: u32,
    pub quality_id: u8,
}

/// A video layer announced to the SFU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupVideoLayer {
    pub id: u8,
    pub bitrate_kbps: u32,
    pub width: u16,
    pub height: u16,
}

/// The local encoder profile and SFU metadata derived from zrtcConfig.groupcall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupVideoEncode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u32,
    pub keyframe_interval_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupVideoPlan {
    pub layers: Vec<GroupVideoLayer>,
    /// The one spatial/temporal layer the local encoder actually sends.
    pub send: Vec<GroupVideoLayer>,
    pub encode: GroupVideoEncode,
    pub sfu_id: Option<u8>,
    pub sfu_long: bool,
    pub start: usize,
}

fn number(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.parse::<f64>().ok()))
    })
}

/// Port of the legacy groupVideoPlan() calculation. Invalid/empty spatial
/// layer tables are not advertised to the SFU.
pub fn group_video_plan(config: &serde_json::Value) -> Option<GroupVideoPlan> {
    let group = config.get("groupcall")?;
    let spatial_layers = group.get("spatialLayers")?.as_array()?;
    let temporal = number(group.get("temporalNum"))? as i64;
    if !(1..=8).contains(&temporal) {
        return None;
    }
    let max_width = number(config.get("maxWidth"))
        .filter(|value| *value > 0.0)
        .unwrap_or(1280.0)
        .trunc();
    let max_height = number(config.get("maxHeight"))
        .filter(|value| *value > 0.0)
        .unwrap_or(720.0)
        .trunc();
    let keyframe_interval_ms = number(config.get("keyFrameIntervalMs"))
        .filter(|value| *value > 0.0)
        .unwrap_or(1000.0)
        .trunc() as u64;

    let mut layers = Vec::new();
    let mut rows = Vec::new();
    let mut spatial_index = 0u8;
    for raw in spatial_layers {
        if !raw.is_object() {
            continue;
        }
        let Some(factor_num) = number(raw.get("factorNum")) else {
            continue;
        };
        let Some(factor_den) = number(raw.get("factorDen")) else {
            continue;
        };
        let denominator = (factor_den.trunc() as i64 & 0xff) as u32;
        let numerator = (factor_num.trunc() as i64 & 0xff) as u32;
        let bitrate = number(raw.get("bitrateKbps"));
        let fps = number(raw.get("fps"));
        let (Some(bitrate), Some(fps)) = (bitrate, fps) else {
            continue;
        };
        if denominator == 0 || !bitrate.is_finite() || !fps.is_finite() || fps <= 0.0 {
            continue;
        }
        let width =
            ((max_width * f64::from(numerator) / f64::from(denominator)).trunc() as u32) & !1;
        let height =
            ((max_height * f64::from(numerator) / f64::from(denominator)).trunc() as u32) & !1;
        if width == 0 || height == 0 || width > u16::MAX as u32 || height > u16::MAX as u32 {
            continue;
        }
        let base_kbps = bitrate.trunc().max(0.0) as u32;
        for temporal_index in 0..temporal as u8 {
            let shift = (temporal as u8 - 1 - temporal_index) as u32;
            let wire_kbps = base_kbps >> shift;
            let id = spatial_index
                .saturating_mul(3)
                .saturating_add(temporal_index);
            let layer = GroupVideoLayer {
                id,
                bitrate_kbps: wire_kbps,
                width: width as u16,
                height: height as u16,
            };
            layers.push(layer);
            if temporal_index == 0 {
                rows.push((layer, fps.trunc().max(1.0) as u32));
            }
        }
        spatial_index = spatial_index.saturating_add(1);
    }
    if layers.is_empty() || rows.is_empty() {
        return None;
    }

    let requested = std::env::var("ZCALL_GROUP_CAM_LAYER")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(1);
    let start = requested.clamp(0, rows.len() as i64 - 1) as usize;
    let (row, fps) = rows[start];
    let codec = number(group.get("codec")).unwrap_or(0.0) as i64;
    let (sfu_id, sfu_long) = if codec == 60 || codec == 61 {
        (Some(12), false)
    } else if codec == 4 {
        (None, false)
    } else {
        (Some(14), true)
    };

    Some(GroupVideoPlan {
        layers,
        send: vec![GroupVideoLayer {
            id: 0,
            bitrate_kbps: row.bitrate_kbps,
            width: row.width,
            height: row.height,
        }],
        encode: GroupVideoEncode {
            width: u32::from(row.width),
            height: u32::from(row.height),
            fps,
            bitrate: row.bitrate_kbps.saturating_mul(1000),
            keyframe_interval_ms,
        },
        sfu_id,
        sfu_long,
        start,
    })
}

/// Choose the SFU quality id for a tile using the legacy client sizing rule.
pub fn select_group_video_quality(layers: &[GroupVideoLayer], tile_width: u32) -> u8 {
    let mut spatial = Vec::new();
    let mut seen = HashSet::new();
    for layer in layers {
        let index = layer.id / 3;
        if seen.insert(index) {
            spatial.push(*layer);
        }
    }
    if spatial.is_empty() {
        return 0xff;
    }
    if tile_width == 0 {
        return spatial[0].id;
    }
    let mut top = spatial.len();
    if spatial.len() >= 3 {
        let high = spatial[spatial.len() - 1];
        if tile_width < u32::from(high.width.max(high.height)) {
            top = spatial.len() - 1;
        }
    }
    for layer in spatial[..top].iter().rev() {
        if u32::from(layer.width.min(layer.height)) <= tile_width {
            return layer.id;
        }
    }
    spatial[0].id
}

fn build_group_video_control(uid: u32, token: u32, seq: u32, sub_cmd: u8, body: &[u8]) -> Vec<u8> {
    let header = build_control_header(
        msg_types::CONTROL,
        0x01,
        seq,
        uid,
        token,
        commands::REQ_FORWARD,
        sub_cmd,
    );
    let mut out = Vec::with_capacity(CONTROL_HEADER_LEN + body.len() + 2);
    out.extend_from_slice(&header);
    push_u16_blob(&mut out, body);
    out
}

/// Ask the SFU for up to 200 members' selected video quality IDs (subCmd 13).
pub fn build_group_video_quality_request(
    uid: u32,
    token: u32,
    seq: u32,
    entries: &[GroupVideoQualityRequest],
) -> Vec<u8> {
    let entries = &entries[..entries.len().min(MAX_GROUP_VIDEO_QUALITY_ENTRIES)];
    let mut body = Vec::with_capacity(1 + entries.len() * 5);
    body.push(entries.len() as u8);
    for entry in entries {
        body.extend_from_slice(&entry.uid.to_le_bytes());
        body.push(entry.quality_id);
    }
    build_group_video_control(uid, token, seq, 0x0d, &body)
}

/// Announce the local sender's available video layers (subCmd 12).
pub fn build_group_video_quality_config(
    uid: u32,
    token: u32,
    seq: u32,
    layers: &[GroupVideoLayer],
) -> Vec<u8> {
    let layers = &layers[..layers.len().min(u8::MAX as usize)];
    let mut body = Vec::with_capacity(1 + layers.len() * 9);
    body.push(layers.len() as u8);
    for layer in layers {
        body.push(layer.id);
        body.extend_from_slice(&layer.bitrate_kbps.to_le_bytes());
        body.extend_from_slice(&layer.width.to_le_bytes());
        body.extend_from_slice(&layer.height.to_le_bytes());
    }
    build_group_video_control(uid, token, seq, 0x0c, &body)
}

/// cmd 33: SFU group synchronization control with no payload.
pub fn build_group_sync_packet(
    uid: u32,
    token: u32,
    seq: u32,
    sub_cmd: u8,
) -> [u8; CONTROL_HEADER_LEN] {
    build_control_header(
        msg_types::CONTROL,
        0x01,
        seq,
        uid,
        token,
        commands::GROUP_MESSAGE,
        sub_cmd,
    )
}

/// Parse the u32 LE UID list in cmd 50 ROOM_UPDATED.
pub fn parse_room_updated(data: &[u8]) -> Option<Vec<u32>> {
    if !data.len().is_multiple_of(4) {
        return None;
    }
    let (chunks, remainder) = data.as_chunks::<4>();
    if !remainder.is_empty() {
        return None;
    }
    Some(
        chunks
            .iter()
            .map(|item| u32::from_le_bytes(*item))
            .collect(),
    )
}

/// Parse a server-relayed cmd 32/subCmd 13 request from another group member.
pub fn parse_group_video_quality_request(
    packet: &[u8],
) -> Option<(u32, Vec<GroupVideoQualityRequest>)> {
    if packet.len() < CONTROL_HEADER_LEN + 3
        || packet[0] != msg_types::SERVER_REPLY
        || packet[1] != MAGIC_BYTE
        || u16::from_le_bytes([packet[18], packet[19]]) != commands::REQ_FORWARD
        || packet[20] != 0x0d
    {
        return None;
    }
    let body_len = u16::from_le_bytes([packet[21], packet[22]]) as usize;
    if body_len == 0 || body_len != packet.len() - CONTROL_HEADER_LEN - 2 {
        return None;
    }
    let count = packet[23] as usize;
    if body_len != 1 + count * 5 {
        return None;
    }
    let sender_uid = u32::from_le_bytes(packet[10..14].try_into().ok()?);
    let mut entries = Vec::with_capacity(count);
    let (entries_data, remainder) = packet[24..].as_chunks::<5>();
    if !remainder.is_empty() {
        return None;
    }
    for entry in entries_data {
        entries.push(GroupVideoQualityRequest {
            uid: u32::from_le_bytes(entry[..4].try_into().ok()?),
            quality_id: entry[4],
        });
    }
    Some((sender_uid, entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u16_blob(packet: &[u8], offset: &mut usize) -> Vec<u8> {
        let len = u16::from_le_bytes([packet[*offset], packet[*offset + 1]]) as usize;
        *offset += 2;
        let value = packet[*offset..*offset + len].to_vec();
        *offset += len;
        value
    }

    #[test]
    fn group_init_layouts_encode_role_fields_and_extra() {
        for layout in GroupInitLayout::ALL {
            let packet = build_group_init_zrtp_packet(
                true,
                0x1122_3344,
                b"session",
                0x5566_7788,
                0x99aa_bbcc,
                true,
                layout,
            );
            assert_eq!(packet[0], msg_types::CONTROL);
            assert_eq!(packet[1], MAGIC_BYTE);
            assert_eq!(
                u32::from_le_bytes(packet[10..14].try_into().unwrap()),
                0x1122_3344
            );
            assert_eq!(
                u16::from_le_bytes(packet[18..20].try_into().unwrap()),
                commands::INIT_CALL_CALLER
            );
            assert_eq!(
                packet[20],
                if layout == GroupInitLayout::Sub6 {
                    6
                } else {
                    4
                }
            );

            let mut offset = CONTROL_HEADER_LEN;
            match layout {
                GroupInitLayout::Zero => {
                    assert_eq!(&packet[offset..offset + 8], &[0; 8]);
                    offset += 8;
                }
                GroupInitLayout::Sub6 => {
                    assert_eq!(
                        &packet[offset..offset + 7],
                        &[0x88, 0x77, 0x66, 0x55, 1, 5, 0]
                    );
                    offset += 7;
                }
                GroupInitLayout::CallId => {
                    assert_eq!(
                        &packet[offset..offset + 8],
                        &[0x88, 0x77, 0x66, 0x55, 0, 0, 0, 0]
                    );
                    offset += 8;
                }
                GroupInitLayout::CallIdHost => {
                    assert_eq!(
                        &packet[offset..offset + 8],
                        &[0x88, 0x77, 0x66, 0x55, 0xcc, 0xbb, 0xaa, 0x99]
                    );
                    offset += 8;
                }
            }
            assert_eq!(read_u16_blob(&packet, &mut offset), b"session");
            assert_eq!(read_u16_blob(&packet, &mut offset), [0, 5, 1, 1]);
            assert_eq!(offset, packet.len());
        }
    }

    #[test]
    fn room_updated_requires_a_complete_u32_list() {
        assert_eq!(
            parse_room_updated(&[1, 0, 0, 0, 2, 0, 0, 0]),
            Some(vec![1, 2])
        );
        assert_eq!(parse_room_updated(&[]), Some(Vec::new()));
        assert_eq!(parse_room_updated(&[1, 0, 0]), None);
    }

    #[test]
    fn video_quality_packets_follow_the_sfu_control_layout() {
        let request = build_group_video_quality_request(
            0x1122_3344,
            0xaabb_ccdd,
            7,
            &[GroupVideoQualityRequest {
                uid: 0x0102_0304,
                quality_id: 3,
            }],
        );
        assert_eq!(request[1], 1);
        assert_eq!(u32::from_le_bytes(request[6..10].try_into().unwrap()), 7);
        assert_eq!(
            u32::from_le_bytes(request[10..14].try_into().unwrap()),
            0x1122_3344
        );
        assert_eq!(
            u32::from_le_bytes(request[14..18].try_into().unwrap()),
            0xaabb_ccdd
        );
        assert_eq!(
            u16::from_le_bytes(request[18..20].try_into().unwrap()),
            commands::REQ_FORWARD
        );
        assert_eq!(request[20], 0x0d);
        assert_eq!(u16::from_le_bytes(request[21..23].try_into().unwrap()), 6);
        assert_eq!(&request[23..], &[1, 4, 3, 2, 1, 3]);

        let config = build_group_video_quality_config(
            9,
            10,
            11,
            &[GroupVideoLayer {
                id: 3,
                bitrate_kbps: 300,
                width: 720,
                height: 360,
            }],
        );
        assert_eq!(config[20], 0x0c);
        assert_eq!(u16::from_le_bytes(config[21..23].try_into().unwrap()), 10);
        assert_eq!(&config[23..], &[1, 3, 44, 1, 0, 0, 208, 2, 104, 1]);
    }

    #[test]
    fn parses_relayed_member_video_quality_request() {
        let mut packet = build_group_video_quality_request(
            77,
            0,
            2,
            &[GroupVideoQualityRequest {
                uid: 88,
                quality_id: 0xff,
            }],
        );
        packet[0] = msg_types::SERVER_REPLY;
        packet[1] = MAGIC_BYTE;
        assert_eq!(
            parse_group_video_quality_request(&packet),
            Some((
                77,
                vec![GroupVideoQualityRequest {
                    uid: 88,
                    quality_id: 0xff
                }]
            ))
        );
        packet[22] = 8;
        assert!(parse_group_video_quality_request(&packet).is_none());
    }

    #[test]
    fn group_video_plan_builds_temporal_layers_and_selected_encoder_profile() {
        let config = serde_json::json!({
            "maxWidth":1280,
            "maxHeight":720,
            "keyFrameIntervalMs":800,
            "groupcall":{
                "codec":60,
                "temporalNum":2,
                "spatialLayers":[
                    {"factorNum":1,"factorDen":3,"bitrateKbps":300,"fps":15},
                    {"factorNum":1,"factorDen":2,"bitrateKbps":700,"fps":24},
                    {"factorNum":3,"factorDen":4,"bitrateKbps":1200,"fps":30}
                ]
            }
        });
        let plan = group_video_plan(&config).expect("valid group video plan");
        assert_eq!(plan.layers.len(), 6);
        assert_eq!(plan.layers[0].id, 0);
        assert_eq!(plan.layers[0].bitrate_kbps, 150);
        assert_eq!((plan.layers[0].width, plan.layers[0].height), (426, 240));
        assert_eq!(plan.encode.keyframe_interval_ms, 800);
        assert_eq!(plan.sfu_id, Some(12));
        assert!(!plan.sfu_long);
        assert_eq!(plan.send.len(), 1);
        assert_eq!(plan.send[0].id, 0);
        assert_eq!(plan.send[0].width, plan.encode.width as u16);
    }

    #[test]
    fn group_video_quality_selection_uses_tile_width_and_empty_table_fallback() {
        let layers = vec![
            GroupVideoLayer {
                id: 0,
                bitrate_kbps: 100,
                width: 426,
                height: 240,
            },
            GroupVideoLayer {
                id: 3,
                bitrate_kbps: 300,
                width: 640,
                height: 360,
            },
            GroupVideoLayer {
                id: 6,
                bitrate_kbps: 700,
                width: 960,
                height: 540,
            },
        ];
        assert_eq!(select_group_video_quality(&layers, 0), 0);
        assert_eq!(select_group_video_quality(&layers, 560), 3);
        assert_eq!(select_group_video_quality(&layers, 720), 3);
        assert_eq!(select_group_video_quality(&layers, 1000), 6);
        assert_eq!(select_group_video_quality(&[], 720), 0xff);
    }

    #[test]
    fn group_share_init_and_ping_use_their_dedicated_wire_layouts() {
        let init = build_share_init_zrtp_packet(0x1234, b"session");
        assert_eq!(init[0], msg_types::CONTROL);
        assert_eq!(u32::from_le_bytes(init[10..14].try_into().unwrap()), 0x1234);
        assert_eq!(
            u16::from_le_bytes(init[18..20].try_into().unwrap()),
            commands::INIT_CALL_CALLEE
        );
        assert_eq!(init[20], 0x04);
        assert_eq!(u16::from_le_bytes(init[29..31].try_into().unwrap()), 7);
        assert_eq!(&init[31..38], b"session");
        assert_eq!(u16::from_le_bytes(init[38..40].try_into().unwrap()), 4);
        assert_eq!(&init[40..44], &[0, 0xff, 1, 0]);

        let ping = build_share_zavi_ping_packet(0x1234, 0xaabb_ccdd, 9, b"session");
        assert_eq!(u32::from_le_bytes(ping[10..14].try_into().unwrap()), 0x1234);
        assert_eq!(
            u32::from_le_bytes(ping[14..18].try_into().unwrap()),
            0xaabb_ccdd
        );
        assert_eq!(
            u16::from_le_bytes(ping[18..20].try_into().unwrap()),
            commands::INIT_ZAVI_PING
        );
        assert_eq!(ping[20], 0x01);
        assert_eq!(u16::from_le_bytes(ping[29..31].try_into().unwrap()), 7);
        assert_eq!(&ping[31..38], b"session");
    }

    #[test]
    fn accepts_only_known_group_init_layout_names() {
        assert_eq!(GroupInitLayout::parse("zero"), Some(GroupInitLayout::Zero));
        assert_eq!(GroupInitLayout::parse("sub6"), Some(GroupInitLayout::Sub6));
        assert_eq!(GroupInitLayout::parse("invalid"), None);
    }
}
