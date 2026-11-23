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
