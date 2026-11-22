//! ZRTP relay protocol wire framing for Zalo media transport.

pub mod msg_types {
    pub const CONTROL: u8 = 0x01;
    pub const SERVER_REPLY: u8 = 0x02;
    pub const AUDIO_RTP: u8 = 0x03;
    pub const AUDIO_RTP_DOWN: u8 = 0x04;
    pub const AUDIO_FEC: u8 = 0x05;
    pub const AUDIO_RTCP: u8 = 0x07;
    pub const VIDEO_RTP: u8 = 0x0d;
    pub const VIDEO_DOWN: u8 = 0x0e;
    pub const VIDEO_FEC: u8 = 0x0f;
    pub const P2P_EXT: u8 = 0x7f;
}

pub mod commands {
    pub const PING: u16 = 2;
    pub const END_CALL: u16 = 3;
    pub const INIT_CALL_CALLER: u16 = 11;
    pub const INIT_CALL_CALLEE: u16 = 12;
    pub const ECHO: u16 = 5;
    pub const CHANGE_ADDRESS: u16 = 14;
    pub const REQ_FORWARD: u16 = 32;
    pub const GROUP_MESSAGE: u16 = 33;
    pub const ROOM_UPDATED: u16 = 50;
    pub const INIT_ZAVI_PING: u16 = 0x33;
}

pub const MAGIC_BYTE: u8 = 0x7e;
pub const CONTROL_HEADER_LEN: usize = 21;
pub const MEDIA_HEADER_LEN: usize = 5;

/// Direct peer-to-peer transport envelope (legacy ZRTP MsgType 0x7f).
pub mod p2p {
    pub const HEADER_LEN: usize = 9;
    pub const BIND: u8 = 1;
    pub const BIND_PEER: u8 = 2;
    pub const BIND_ANSWER: u8 = 3;
    pub const KEEPALIVE: u8 = 4;
    pub const SIGNAL: u8 = 5;
    pub const AUDIO: u8 = 6;
    pub const AUDIO_RTCP: u8 = 7;
    pub const VIDEO: u8 = 8;
    pub const VIDEO_RTCP: u8 = 9;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Header {
        pub is_callee: bool,
        pub call_id: u32,
        pub kind: u8,
    }

    pub fn parse(packet: &[u8]) -> Option<(Header, &[u8])> {
        if packet.len() < HEADER_LEN
            || packet[0] != super::msg_types::P2P_EXT
            || packet[1] != 0
            || packet[8] != 0
        {
            return None;
        }
        let role = packet[2];
        if role > 1 {
            return None;
        }
        Some((
            Header {
                is_callee: role == 1,
                call_id: u32::from_le_bytes(packet[3..7].try_into().ok()?),
                kind: packet[7],
            },
            &packet[HEADER_LEN..],
        ))
    }

    pub fn header(is_callee: bool, call_id: u32, kind: u8) -> [u8; HEADER_LEN] {
        let mut output = [0u8; HEADER_LEN];
        output[0] = super::msg_types::P2P_EXT;
        output[2] = u8::from(is_callee);
        output[3..7].copy_from_slice(&call_id.to_le_bytes());
        output[7] = kind;
        output
    }

    pub fn bind(is_callee: bool, call_id: u32, seq: u16, index: u32) -> Vec<u8> {
        let mut output = header(is_callee, call_id, BIND).to_vec();
        output.extend_from_slice(&seq.to_le_bytes());
        output.extend_from_slice(&index.to_be_bytes());
        output
    }

    /// Respond to a peer's kind-2 BIND_PEER with the endpoint the OS observed.
    pub fn bind_answer(
        is_callee: bool,
        call_id: u32,
        request_body: &[u8],
        observed_addr: &str,
    ) -> Option<Vec<u8>> {
        if request_body.len() < 6 {
            return None;
        }
        let addr = observed_addr.as_bytes();
        let mut output = header(is_callee, call_id, BIND_ANSWER).to_vec();
        output.extend_from_slice(&request_body[..6]);
        output.extend_from_slice(&(addr.len() as u32).to_le_bytes());
        output.extend_from_slice(addr);
        Some(output)
    }

    /// Tell the peer to stop using P2P (subCmd 2) or start it again (subCmd 1).
    pub fn signal(is_callee: bool, call_id: u32, sub_cmd: u8, seq: u32) -> Vec<u8> {
        let mut output = header(is_callee, call_id, SIGNAL).to_vec();
        output.push(sub_cmd);
        output.extend_from_slice(&seq.to_le_bytes());
        output
    }

    pub fn pong(is_callee: bool, ping: &[u8]) -> Option<Vec<u8>> {
        let (parsed, _) = parse(ping)?;
        if parsed.kind != KEEPALIVE || ping.len() < HEADER_LEN + 1 || ping[9] != 1 {
            return None;
        }
        let mut output = ping.to_vec();
        output[2] = u8::from(is_callee);
        output[9] = 2;
        Some(output)
    }

    pub fn wrap(is_callee: bool, call_id: u32, kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut output = header(is_callee, call_id, kind).to_vec();
        output.extend_from_slice(payload);
        output
    }
}

pub fn wrap_media_packet(msg_type: u8, token: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(MEDIA_HEADER_LEN + payload.len());
    buf.push(msg_type);
    buf.extend_from_slice(&token.to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

pub fn build_control_header(
    msg_type: u8,
    magic: u8,
    seq: u32,
    uid: u32,
    token: u32,
    cmd: u16,
    sub_cmd: u8,
) -> [u8; CONTROL_HEADER_LEN] {
    let mut buf = [0u8; CONTROL_HEADER_LEN];
    buf[0] = msg_type;
    buf[1] = magic;
    buf[2..6].copy_from_slice(&0u32.to_le_bytes()); // field02
    buf[6..10].copy_from_slice(&seq.to_le_bytes());
    buf[10..14].copy_from_slice(&uid.to_le_bytes());
    buf[14..18].copy_from_slice(&token.to_le_bytes());
    buf[18..20].copy_from_slice(&cmd.to_le_bytes());
    buf[20] = sub_cmd;
    buf
}

fn write_u16_str(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u16).to_le_bytes());
    out.extend_from_slice(s);
}

pub fn build_init_zrtp_packet(
    is_caller: bool,
    uid: u32,
    peer_uid: u32,
    call_id: u32,
    sess_id: &[u8],
    extra: &[u8],
    sub_cmd: u8,
) -> Vec<u8> {
    let cmd = if is_caller {
        commands::INIT_CALL_CALLER
    } else {
        commands::INIT_CALL_CALLEE
    };
    let hdr = build_control_header(msg_types::CONTROL, MAGIC_BYTE, 0, uid, 0, cmd, sub_cmd);
    let mut out = Vec::with_capacity(64 + sess_id.len() + extra.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(&call_id.to_le_bytes());
    out.extend_from_slice(&peer_uid.to_le_bytes());
    write_u16_str(&mut out, sess_id);
    if !extra.is_empty() {
        write_u16_str(&mut out, extra);
    }
    out
}

pub fn build_ping_packet(uid: u32, token: u32, seq: u32, sess_id: &[u8]) -> Vec<u8> {
    // initZRTPPacketRequestPing overwrites magic with 1 and uses subCmd 1.
    let hdr = build_control_header(
        msg_types::CONTROL,
        0x01,
        seq,
        uid,
        token,
        commands::PING,
        0x01,
    );
    let mut out = Vec::with_capacity(32 + sess_id.len());
    out.extend_from_slice(&hdr);
    write_u16_str(&mut out, sess_id);
    out
}

/// initZRTPPacketRequestChangeAddress: magic 1, cmd 14/subCmd 1, then u16str sessId.
pub fn build_change_address_packet(uid: u32, token: u32, seq: u32, sess_id: &[u8]) -> Vec<u8> {
    let hdr = build_control_header(
        msg_types::CONTROL,
        0x01,
        seq,
        uid,
        token,
        commands::CHANGE_ADDRESS,
        0x01,
    );
    let mut out = Vec::with_capacity(CONTROL_HEADER_LEN + 2 + sess_id.len());
    out.extend_from_slice(&hdr);
    write_u16_str(&mut out, sess_id);
    out
}

/// ReqForward (cmd 32) is relayed by the server to the peer and acknowledged by
/// MsgType 2. Its payload is raw (not length-prefixed).
/// Status payload used by ReqForward cmd 32/subCmd 10 once per second:
/// six little-endian u32 values [20, 0, 1, 1, rx_packets, 0].
pub fn build_req_forward_status(rx_per_second: u32) -> [u8; 24] {
    let values = [20u32, 0, 1, 1, rx_per_second, 0];
    let mut payload = [0u8; 24];
    for (index, value) in values.into_iter().enumerate() {
        payload[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    payload
}

pub fn build_req_forward_packet(
    uid: u32,
    token: u32,
    seq: u32,
    sub_cmd: u8,
    payload: &[u8],
) -> Vec<u8> {
    let hdr = build_control_header(
        msg_types::CONTROL,
        0x01,
        seq,
        uid,
        token,
        commands::REQ_FORWARD,
        sub_cmd,
    );
    let mut out = Vec::with_capacity(CONTROL_HEADER_LEN + payload.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(payload);
    out
}

pub fn build_end_call_packet(uid: u32, token: u32, seq: u32, call_id: u32) -> Vec<u8> {
    let hdr = build_control_header(
        msg_types::CONTROL,
        MAGIC_BYTE,
        seq,
        uid,
        token,
        commands::END_CALL,
        0,
    );
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&hdr);
    out.extend_from_slice(&call_id.to_le_bytes());
    out
}

pub fn build_zavi_ping_packet(
    uid: u32,
    token: u32,
    seq: u32,
    sess_id: &[u8],
    video_on: bool,
) -> Vec<u8> {
    let hdr = build_control_header(
        msg_types::CONTROL,
        MAGIC_BYTE,
        seq,
        uid,
        token,
        commands::INIT_ZAVI_PING,
        1,
    );
    let mut out = Vec::with_capacity(32 + sess_id.len());
    out.extend_from_slice(&hdr);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    write_u16_str(&mut out, sess_id);
    let extra = [0u8, 5, if video_on { 1 } else { 0 }];
    write_u16_str(&mut out, &extra);
    out
}

#[derive(Debug, Clone)]
pub struct ServerReply {
    pub msg_type: u8,
    pub seq: u32,
    pub uid: u32,
    /// Generic header token (used by non-InitZRTP replies).
    pub token: u32,
    pub cmd: u16,
    pub sub_cmd: u8,
    pub payload: Vec<u8>,
    /// InitZRTP result from the first four payload bytes (0 = accepted).
    pub result: Option<u32>,
    pub call_id: Option<u32>,
    /// The media token is in the InitZRTP response payload at offset 8,
    /// not in the control header's token field.
    pub server_token: Option<u32>,
    /// Public endpoint observed by the relay, encoded as "ip|port".
    pub public_addr: Option<String>,
}

pub fn parse_server_reply(buf: &[u8]) -> Option<ServerReply> {
    if buf.len() < CONTROL_HEADER_LEN || buf[0] != msg_types::SERVER_REPLY || buf[1] != MAGIC_BYTE {
        return None;
    }
    let seq = u32::from_le_bytes(buf[6..10].try_into().ok()?);
    let uid = u32::from_le_bytes(buf[10..14].try_into().ok()?);
    let token = u32::from_le_bytes(buf[14..18].try_into().ok()?);
    let cmd = u16::from_le_bytes(buf[18..20].try_into().ok()?);
    let sub_cmd = buf[20];
    let payload = buf[CONTROL_HEADER_LEN..].to_vec();

    let is_init = cmd == commands::INIT_CALL_CALLER || cmd == commands::INIT_CALL_CALLEE;
    let result = if is_init && payload.len() >= 4 {
        Some(u32::from_le_bytes(payload[0..4].try_into().ok()?))
    } else {
        None
    };
    let accepted = result == Some(0) && payload.len() >= 12;
    let call_id = accepted.then(|| u32::from_le_bytes(payload[4..8].try_into().unwrap()));
    let server_token = accepted.then(|| u32::from_le_bytes(payload[8..12].try_into().unwrap()));
    let public_addr = if accepted && payload.len() >= 14 {
        let n = u16::from_le_bytes(payload[12..14].try_into().unwrap()) as usize;
        let end = 14usize.saturating_add(n).min(payload.len());
        if n > 0 && end == 14 + n {
            Some(String::from_utf8_lossy(&payload[14..end]).into_owned())
        } else {
            None
        }
    } else {
        None
    };

    Some(ServerReply {
        msg_type: buf[0],
        seq,
        uid,
        token,
        cmd,
        sub_cmd,
        payload,
        result,
        call_id,
        server_token,
        public_addr,
    })
}

#[cfg(test)]
mod tests {
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

        let callee = build_init_zrtp_packet(false, 3003, 1001, 2002, b"my-session-token", &[], 2);
        assert_eq!(
            u16::from_le_bytes(callee[18..20].try_into().unwrap()),
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
        let pong = p2p::pong(true, &ping).unwrap();
        assert_eq!(pong[2], 1);
        assert_eq!(pong[9], 2);
        assert_eq!(pong[10..], ping[10..]);
        assert!(p2p::pong(true, &pong).is_none());
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
        let parsed =
            parse_server_reply(&truncated).expect("truncated init header is still parseable");
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
}
