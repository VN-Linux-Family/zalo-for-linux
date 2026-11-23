use super::*;

fn init_reply(result: u32, call_id: u32, token: u32, public: &[u8]) -> Vec<u8> {
    let header = build_control_header(
        msg_types::SERVER_REPLY,
        MAGIC_BYTE,
        19,
        1234,
        0xdead_beef,
        commands::INIT_CALL_CALLER,
        2,
    );
    let mut packet = header.to_vec();
    packet.extend_from_slice(&result.to_le_bytes());
    packet.extend_from_slice(&call_id.to_le_bytes());
    packet.extend_from_slice(&token.to_le_bytes());
    packet.extend_from_slice(&(public.len() as u16).to_le_bytes());
    packet.extend_from_slice(public);
    packet
}

#[test]
fn wraps_media_packet_with_type_token_and_payload() {
    let payload = b"audio-payload-sample-bytes";
    let token = 0x1234_5678;
    let packet = wrap_media_packet(msg_types::AUDIO_RTP, token, payload);
    assert_eq!(packet.len(), MEDIA_HEADER_LEN + payload.len());
    assert_eq!(packet[0], msg_types::AUDIO_RTP);
    assert_eq!(u32::from_le_bytes(packet[1..5].try_into().unwrap()), token);
    assert_eq!(&packet[MEDIA_HEADER_LEN..], payload);
}

#[test]
fn control_header_uses_expected_little_endian_fields() {
    let header = build_control_header(
        msg_types::CONTROL,
        MAGIC_BYTE,
        42,
        10_001,
        9_999,
        commands::PING,
        0,
    );
    assert_eq!(header.len(), CONTROL_HEADER_LEN);
    assert_eq!(header[0], msg_types::CONTROL);
    assert_eq!(header[1], MAGIC_BYTE);
    assert_eq!(u32::from_le_bytes(header[6..10].try_into().unwrap()), 42);
    assert_eq!(
        u32::from_le_bytes(header[10..14].try_into().unwrap()),
        10_001
    );
    assert_eq!(
        u32::from_le_bytes(header[14..18].try_into().unwrap()),
        9_999
    );
    assert_eq!(
        u16::from_le_bytes(header[18..20].try_into().unwrap()),
        commands::PING
    );
    assert_eq!(header[20], 0);
}

#[test]
fn end_call_packet_carries_call_id() {
    let packet = build_end_call_packet(12345, 67890, 4, 999);
    assert_eq!(packet.len(), CONTROL_HEADER_LEN + 4);
    assert_eq!(
        u16::from_le_bytes(packet[18..20].try_into().unwrap()),
        commands::END_CALL
    );
    assert_eq!(
        u32::from_le_bytes(packet[CONTROL_HEADER_LEN..].try_into().unwrap()),
        999
    );
}

#[test]
fn control_header_encodes_magic_sequence_uid_token_and_command() {
    let header = build_control_header(
        msg_types::CONTROL,
        MAGIC_BYTE,
        42,
        10_001,
        9_999,
        commands::PING,
        0,
    );
    assert_eq!(header.len(), CONTROL_HEADER_LEN);
    assert_eq!(header[0], msg_types::CONTROL);
    assert_eq!(header[1], MAGIC_BYTE);
    assert_eq!(u32::from_le_bytes(header[6..10].try_into().unwrap()), 42);
    assert_eq!(
        u32::from_le_bytes(header[10..14].try_into().unwrap()),
        10_001
    );
    assert_eq!(
        u32::from_le_bytes(header[14..18].try_into().unwrap()),
        9_999
    );
    assert_eq!(
        u16::from_le_bytes(header[18..20].try_into().unwrap()),
        commands::PING
    );
    assert_eq!(header[20], 0);
}

#[test]
fn builds_init_zrtp_for_caller_and_callee() {
    let caller = build_init_zrtp_packet(true, 1001, 3003, 2002, b"my-session-token", &[], 2);
    assert_eq!(caller[0], msg_types::CONTROL);
    assert_eq!(
        u16::from_le_bytes(caller[18..20].try_into().unwrap()),
        commands::INIT_CALL_CALLER
    );
    let payload = CONTROL_HEADER_LEN;
    assert_eq!(
        u32::from_le_bytes(caller[payload..payload + 4].try_into().unwrap()),
        2002
    );
    assert_eq!(
        u32::from_le_bytes(caller[payload + 4..payload + 8].try_into().unwrap()),
        3003
    );
    assert_eq!(
        u16::from_le_bytes(caller[payload + 8..payload + 10].try_into().unwrap()),
        16
    );
    assert_eq!(&caller[payload + 10..], b"my-session-token");

    let callee_packet =
        build_init_zrtp_packet(false, 3003, 1001, 2002, b"my-session-token", &[], 2);
    assert_eq!(
        u16::from_le_bytes(callee_packet[18..20].try_into().unwrap()),
        commands::INIT_CALL_CALLEE
    );
}

#[test]
fn parses_init_result_and_media_token_from_payload() {
    let packet = init_reply(0, 0x1234_5678, 0xaabb_ccdd, b"203.0.113.5|40000");
    let parsed = parse_server_reply(&packet).expect("valid relay reply");
    assert_eq!(parsed.seq, 19);
    assert_eq!(parsed.uid, 1234);
    assert_eq!(parsed.token, 0xdead_beef); // header token is not the media token
    assert_eq!(parsed.result, Some(0));
    assert_eq!(parsed.call_id, Some(0x1234_5678));
    assert_eq!(parsed.server_token, Some(0xaabb_ccdd));
    assert_eq!(parsed.public_addr.as_deref(), Some("203.0.113.5|40000"));
}

#[test]
fn ping_change_address_and_req_forward_match_pr220_wire_layout() {
    let ping = build_ping_packet(0x1122_3344, 0xaabb_ccdd, 7, b"session");
    assert_eq!(ping[0], msg_types::CONTROL);
    assert_eq!(ping[1], 1);
    assert_eq!(u32::from_le_bytes(ping[6..10].try_into().unwrap()), 7);
    assert_eq!(
        u16::from_le_bytes(ping[18..20].try_into().unwrap()),
        commands::PING
    );
    assert_eq!(ping[20], 1);
    assert_eq!(
        &ping[21..],
        &[7, 0, b's', b'e', b's', b's', b'i', b'o', b'n']
    );

    let change = build_change_address_packet(0x1122_3344, 0xaabb_ccdd, 9, b"session");
    assert_eq!(change[1], 1);
    assert_eq!(u32::from_le_bytes(change[6..10].try_into().unwrap()), 9);
    assert_eq!(
        u16::from_le_bytes(change[18..20].try_into().unwrap()),
        commands::CHANGE_ADDRESS
    );
    assert_eq!(change[20], 1);
    assert_eq!(
        &change[21..],
        &[7, 0, b's', b'e', b's', b's', b'i', b'o', b'n']
    );

    let forward = build_req_forward_packet(5, 6, 7, 2, &[1, 2, 3]);
    assert_eq!(forward[1], 1);
    assert_eq!(
        u16::from_le_bytes(forward[18..20].try_into().unwrap()),
        commands::REQ_FORWARD
    );
    assert_eq!(forward[20], 2);
    assert_eq!(&forward[21..], &[1, 2, 3]);
}

#[test]
fn req_forward_status_encodes_six_little_endian_counters() {
    let payload = build_req_forward_status(37);
    let values = payload
        .as_chunks::<4>()
        .0
        .iter()
        .map(|part| u32::from_le_bytes(*part))
        .collect::<Vec<_>>();
    assert_eq!(values, [20, 0, 1, 1, 37, 0]);
}

#[test]
fn p2p_signal_uses_kind_five_and_five_byte_body() {
    let signal = p2p::signal(true, 0x1234_5678, 2, 0x1122_3344);
    assert_eq!(signal.len(), p2p::HEADER_LEN + 5);
    assert_eq!(signal[7], p2p::SIGNAL);
    assert_eq!(&signal[9..], &[2, 0x44, 0x33, 0x22, 0x11]);
}

#[test]
fn p2p_bind_and_reply_use_captured_wire_layout() {
    let bind = p2p::bind(true, 0x1234_5678, 0x9abc, 0x0102_0304);
    assert_eq!(&bind[..9], &[0x7f, 0, 1, 0x78, 0x56, 0x34, 0x12, 1, 0]);
    assert_eq!(&bind[9..11], &[0xbc, 0x9a]);
    assert_eq!(&bind[11..15], &[1, 2, 3, 4]);

    let mut request = p2p::header(false, 0x1234_5678, p2p::BIND_PEER).to_vec();
    request.extend_from_slice(&[0x34, 0x12, 0, 0, 0, 1]);
    request.extend_from_slice(&5u32.to_le_bytes());
    request.extend_from_slice(b"hello");
    request.push(0xff);
    let (_, body) = p2p::parse(&request).unwrap();
    let answer = p2p::bind_answer(true, 0x1234_5678, body, "203.0.113.2|45000").unwrap();
    assert_eq!(answer[7], p2p::BIND_ANSWER);
    assert_eq!(&answer[9..15], &request[9..15]);
    assert_eq!(
        u32::from_le_bytes(answer[15..19].try_into().unwrap()),
        "203.0.113.2|45000".len() as u32
    );
    assert_eq!(&answer[19..], b"203.0.113.2|45000");
}

#[test]
fn p2p_keepalive_pong_changes_role_and_kind_only() {
    let mut ping = p2p::header(false, 88, p2p::KEEPALIVE).to_vec();
    ping.extend_from_slice(&[1, 7, 8]);
    let pong_packet = p2p::pong(true, &ping).unwrap();
    assert_eq!(pong_packet[2], 1);
    assert_eq!(pong_packet[9], 2);
    assert_eq!(pong_packet[10..], ping[10..]);
    assert!(p2p::pong(true, &pong_packet).is_none());
}

#[test]
fn p2p_parser_rejects_wrong_message_role_and_call() {
    let packet = p2p::wrap(false, 42, p2p::AUDIO, &[1, 2, 3]);
    let (header, payload) = p2p::parse(&packet).unwrap();
    assert!(!header.is_callee);
    assert_eq!(header.call_id, 42);
    assert_eq!(header.kind, p2p::AUDIO);
    assert_eq!(payload, &[1, 2, 3]);
    assert!(p2p::parse(&packet[..8]).is_none());
    let mut invalid = packet;
    invalid[2] = 2;
    assert!(p2p::parse(&invalid).is_none());
}

#[test]
fn refuses_failed_or_truncated_init_replies() {
    let failed = init_reply(3, 7, 9, b"");
    let parsed = parse_server_reply(&failed).expect("valid failure reply");
    assert_eq!(parsed.result, Some(3));
    assert_eq!(parsed.server_token, None);

    let mut truncated = init_reply(0, 7, 9, b"");
    truncated.truncate(CONTROL_HEADER_LEN + 8);
    let parsed = parse_server_reply(&truncated).expect("truncated init header is still parseable");
    assert_eq!(parsed.result, Some(0));
    assert_eq!(parsed.call_id, None);
    assert_eq!(parsed.server_token, None);
}

#[test]
fn rejects_untrusted_or_short_datagrams() {
    assert!(parse_server_reply(&[]).is_none());
    assert!(parse_server_reply(&[0u8; CONTROL_HEADER_LEN - 1]).is_none());
    let mut invalid = init_reply(0, 7, 9, b"127.0.0.1|1234");
    invalid[1] ^= 1;
    assert!(parse_server_reply(&invalid).is_none());
}
