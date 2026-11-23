use super::*;
use crate::crypto::srtp::{session_keys, unprotect_rtp};
use crate::protocol::rtp::build_group_audio_rtp_header;

#[test]
fn group_prepare_tries_next_init_layout_only_after_server_refusal() {
    use crate::protocol::zrtp::{
        build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
    };
    use std::thread;

    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    relay
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let relay_thread = thread::spawn(move || {
        let mut buf = [0u8; 1500];
        let (len, client) = relay
            .recv_from(&mut buf)
            .expect("receive first group InitZRTP");
        assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        assert_eq!(buf[0], zrtp_types::CONTROL);
        assert_eq!(buf[20], 0x04); // first candidate: zero layout
        let mut refusal = build_control_header(
            zrtp_types::SERVER_REPLY,
            MAGIC_BYTE,
            1,
            111,
            0,
            commands::INIT_CALL_CALLER,
            2,
        )
        .to_vec();
        refusal.extend_from_slice(&3u32.to_le_bytes());
        relay.send_to(&refusal, client).unwrap();

        let (len, client) = relay
            .recv_from(&mut buf)
            .expect("receive fallback group InitZRTP");
        assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        assert_eq!(buf[20], 0x06); // second candidate: sub6
        let mut accepted = build_control_header(
            zrtp_types::SERVER_REPLY,
            MAGIC_BYTE,
            2,
            111,
            0,
            commands::INIT_CALL_CALLER,
            2,
        )
        .to_vec();
        accepted.extend_from_slice(&0u32.to_le_bytes());
        accepted.extend_from_slice(&77u32.to_le_bytes());
        accepted.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
        accepted.extend_from_slice(&0u16.to_le_bytes());
        relay.send_to(&accepted, client).unwrap();
    });

    let backend = ZrtcMediaBackend::new().unwrap();
    let selected = backend
        .prepare(
            MediaSessionParams {
                call_id: 77,
                host_call: 0x1234,
                uid: 111,
                peer_uid: 222,
                sess_id: b"this-session-is-not-used-for-group-srtp".to_vec(),
                is_caller: true,
                is_group: true,
                send_udp: true,
                video_enabled: true,
                video_config: serde_json::json!({ "srtpKey": "" }),
                p2p_candidates: Vec::new(),
            },
            &[relay_addr],
        )
        .unwrap();
    relay_thread.join().unwrap();
    assert_eq!(selected, Some(relay_addr));
    assert_eq!(
        *backend.selected_server.lock().unwrap(),
        Some((relay_addr, 0xaabb_ccdd))
    );
    assert!(backend.keys.lock().unwrap().is_none());
    assert!(backend.params.lock().unwrap().as_ref().unwrap().is_group);
    backend.stop();
}

#[test]
fn group_camera_uses_sfu_rtp_header_and_announces_quality_layers() {
    use crate::protocol::zrtp::{
        build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
    };
    use std::thread;

    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    relay
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let init_socket = relay.try_clone().unwrap();
    let relay_thread = thread::spawn(move || {
        let mut buf = [0u8; 1500];
        let (len, client) = init_socket
            .recv_from(&mut buf)
            .expect("receive group InitZRTP");
        assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        assert_eq!(buf[0], zrtp_types::CONTROL);
        let mut accepted = build_control_header(
            zrtp_types::SERVER_REPLY,
            MAGIC_BYTE,
            1,
            111,
            0,
            commands::INIT_CALL_CALLER,
            2,
        )
        .to_vec();
        accepted.extend_from_slice(&0u32.to_le_bytes());
        accepted.extend_from_slice(&77u32.to_le_bytes());
        accepted.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
        accepted.extend_from_slice(&0u16.to_le_bytes());
        init_socket.send_to(&accepted, client).unwrap();
    });

    let backend = ZrtcMediaBackend::new().unwrap();
    let config = serde_json::json!({
        "maxWidth":1280,
        "maxHeight":720,
        "groupcall":{
            "codec":60,
            "temporalNum":1,
            "spatialLayers":[
                {"factorNum":1,"factorDen":3,"bitrateKbps":300,"fps":15},
                {"factorNum":1,"factorDen":2,"bitrateKbps":700,"fps":24},
                {"factorNum":3,"factorDen":4,"bitrateKbps":1200,"fps":30}
            ]
        }
    });
    backend
        .prepare(
            MediaSessionParams {
                call_id: 77,
                host_call: 0x1234,
                uid: 111,
                peer_uid: 222,
                sess_id: b"group-session-not-used-with-empty-srtp-key".to_vec(),
                is_caller: true,
                is_group: true,
                send_udp: true,
                video_enabled: true,
                video_config: config,
                p2p_candidates: Vec::new(),
            },
            &[relay_addr],
        )
        .unwrap();
    relay_thread.join().unwrap();
    assert!(backend.keys.lock().unwrap().is_none());
    assert_eq!(
        backend
            .video_profile()
            .map(|profile| (profile.width, profile.height)),
        Some((640, 360))
    );

    backend.start_with_audio(false);
    assert!(backend.send_group_video_quality_config().unwrap());
    assert!(backend.request_group_video(222).unwrap());
    assert!(backend.set_group_member_render_width(222, 700).unwrap());
    assert_eq!(
        backend
            .send_video_frame(true, &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xa0])
            .unwrap(),
        1
    );

    let mut saw_config = false;
    let mut requested_quality = Vec::new();
    let mut video_header = None;
    for _ in 0..8 {
        let mut packet = [0u8; 1500];
        let Ok((len, _)) = relay.recv_from(&mut packet) else {
            break;
        };
        let packet = &packet[..len];
        if packet.first() == Some(&zrtp_types::CONTROL) && packet.len() >= 24 {
            match packet[20] {
                0x0c => saw_config = true,
                0x0d if packet.len() >= 29 => requested_quality.push(packet[28]),
                _ => {}
            }
        } else if packet.first() == Some(&zrtp_types::VIDEO_RTP) {
            video_header = Some(packet[5..].to_vec());
        }
    }
    assert!(saw_config, "SFU layer configuration should be announced");
    assert!(requested_quality.contains(&0));
    assert!(requested_quality.contains(&3));
    let rtp = video_header.expect("group H.264 RTP should reach the SFU");
    assert_eq!(rtp[0], 0x90);
    assert_eq!(rtp[23] >> 4, 12);
    assert_eq!(rtp[24], 0x03);
    assert_eq!(u16::from_be_bytes([rtp[25], rtp[26]]), 1);
    backend.stop();
}

#[test]
fn group_camera_media_is_bidirectional_and_reports_remote_ssrc() {
    let relay = UdpSocket::bind("127.0.0.1:0").expect("bind group relay");
    relay
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    // SFU downlink media may use another UDP source port than the
    // InitZRTP endpoint; the legacy group receiver accepted that path.
    let alternate_relay = UdpSocket::bind("127.0.0.1:0").expect("bind alternate SFU media source");
    let backend = ZrtcMediaBackend::new().expect("create group media backend");
    let config = serde_json::json!({
        "maxWidth":640,"maxHeight":360,
        "groupcall":{"codec":60,"temporalNum":1,"spatialLayers":[
            {"factorNum":1,"factorDen":1,"bitrateKbps":400,"fps":15}
        ]}
    });
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 123,
        host_call: 111,
        uid: 111,
        peer_uid: 111,
        sess_id: vec![0x42; 30],
        is_caller: false,
        is_group: true,
        send_udp: true,
        video_enabled: true,
        video_config: config.clone(),
        p2p_candidates: Vec::new(),
    });
    *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
    // Model an SFU that uses a distinct receive session key: the configured
    // group key must fail authentication, then the receive-only session key
    // must authenticate the remote RTP before it reaches H.264 assembly.
    *backend.keys.lock().unwrap() = Some(session_keys(&[0x11; 30]).unwrap());
    *backend.rx_fallback_keys.lock().unwrap() = Some(session_keys(&[0x42; 30]).unwrap());
    *backend.group_video_plan.lock().unwrap() =
        Some(parse_group_video_plan(&config).expect("build group camera layers"));
    let bound = backend.socket.local_addr().unwrap();
    let local = SocketAddr::from(([127, 0, 0, 1], bound.port()));
    backend.start_with_audio(false);

    let local_frame = [
        0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
        0x88, 0x84, 0x21, 0x01,
    ];
    backend.set_group_camera(false).unwrap();
    assert_eq!(backend.send_video_frame(true, &local_frame).unwrap(), 0);
    backend.set_group_camera(true).unwrap();
    assert!(backend.send_group_video_quality_config().unwrap());
    // A tile resize before the first valid remote video frame must only
    // remember its width; the legacy client did not subscribe yet.
    assert!(!backend.set_group_member_render_width(222, 500).unwrap());
    assert!(backend.request_group_video(222).unwrap());
    assert!(!backend.set_group_member_render_width(222, 700).unwrap());
    assert!(backend.send_video_frame(true, &local_frame).unwrap() > 0);

    let mut tx_camera_rtp = None;
    for _ in 0..16 {
        let mut packet = [0u8; 1500];
        let Ok((len, _)) = relay.recv_from(&mut packet) else {
            break;
        };
        if len > 25 && packet[0] == msg_types::VIDEO_RTP {
            let rtp = &packet[5..len];
            if u32::from_be_bytes(rtp[8..12].try_into().unwrap()) == 111 && rtp[0] & 0x10 != 0 {
                tx_camera_rtp = Some(rtp.to_vec());
                break;
            }
        }
    }
    let tx_camera_rtp = tx_camera_rtp.expect("local camera RTP should reach the SFU");
    let primary_keys = session_keys(&[0x11; 30]).unwrap();
    let tx_clear = unprotect_rtp(&tx_camera_rtp, &primary_keys, 0)
        .expect("SFU uplink camera RTP should authenticate with the configured group key");
    assert_eq!(tx_clear.opus, local_frame);

    let remote_frame = [
        0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
        0x88, 0x84, 0x21, 0x02,
    ];
    let now = unix_now_ms();
    let rtp =
        build_group_video_rtp_header(7, 9000, 222, 77, true, VIDEO_PT_KEY, false, 12, 1, now, now);
    let keys = session_keys(&[0x42; 30]).unwrap();
    // Real UDP downlink: first a packet authenticated by the configured
    // group key, then one authenticated by the receive-only session fallback.
    let primary_protected = protect_rtp(&rtp, &remote_frame, &primary_keys, 7, 222, 0);
    let mut packet = vec![msg_types::VIDEO_DOWN];
    packet.extend_from_slice(&primary_protected);
    alternate_relay.send_to(&packet, local).unwrap();
    let rtp_fallback =
        build_group_video_rtp_header(8, 9900, 223, 78, true, VIDEO_PT_KEY, false, 12, 2, now, now);
    let fallback_frame = [
        0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
        0x88, 0x84, 0x21, 0x03,
    ];
    let fallback_protected = protect_rtp(&rtp_fallback, &fallback_frame, &keys, 8, 223, 0);
    let mut fallback_packet = vec![msg_types::VIDEO_DOWN];
    fallback_packet.extend_from_slice(&fallback_protected);
    alternate_relay.send_to(&fallback_packet, local).unwrap();

    let mut received = Vec::new();
    let mut events = Vec::new();
    for _ in 0..100 {
        received.extend(backend.drain_video_frames());
        events.extend(backend.drain_group_events());
        let received_222 = received.iter().any(|frame| frame.src == Some(222));
        let received_223 = received.iter().any(|frame| frame.src == Some(223));
        if received_222
            && received_223
            && events.contains(&GroupMediaEvent::MemberVideo(222))
            && events.contains(&GroupMediaEvent::MemberVideo(223))
        {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(received.len(), 2);
    assert!(received
        .iter()
        .any(|frame| frame.src == Some(222) && frame.data == remote_frame));
    assert!(received
        .iter()
        .any(|frame| frame.src == Some(223) && frame.data == fallback_frame));
    assert!(events.contains(&GroupMediaEvent::MemberVideo(222)));
    assert!(events.contains(&GroupMediaEvent::MemberVideo(223)));
    backend.stop();
}

#[test]
fn group_sfu_room_and_end_call_replies_become_engine_events() {
    use crate::protocol::zrtp::commands;

    let roster = crate::protocol::zrtp::ServerReply {
        msg_type: msg_types::SERVER_REPLY,
        seq: 1,
        uid: 111,
        token: 0,
        cmd: commands::ROOM_UPDATED,
        sub_cmd: 0,
        payload: [222u32.to_le_bytes(), 333u32.to_le_bytes()].concat(),
        result: None,
        call_id: None,
        server_token: None,
        public_addr: None,
    };
    assert_eq!(
        group_event_from_reply(&roster, 111),
        Some(GroupMediaEvent::Roster(vec![222, 333]))
    );

    let member_left = crate::protocol::zrtp::ServerReply {
        uid: 222,
        cmd: commands::END_CALL,
        sub_cmd: 3,
        payload: Vec::new(),
        ..roster
    };
    assert_eq!(
        group_event_from_reply(&member_left, 111),
        Some(GroupMediaEvent::MemberLeft(222, 3))
    );

    let kicked = crate::protocol::zrtp::ServerReply {
        uid: 111,
        sub_cmd: 2,
        ..member_left
    };
    assert_eq!(
        group_event_from_reply(&kicked, 111),
        Some(GroupMediaEvent::PeerEnd(2))
    );

    let ignored = crate::protocol::zrtp::ServerReply {
        uid: 111,
        sub_cmd: 0,
        ..kicked
    };
    assert_eq!(group_event_from_reply(&ignored, 111), None);
}

#[test]
fn p2p_handover_notifies_peer_and_requests_relay_fallback() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    for target in [&peer, &relay] {
        target
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
    }
    let params = MediaSessionParams {
        call_id: 123,
        host_call: 111,
        uid: 111,
        peer_uid: 222,
        sess_id: b"p2p-fallback-session".to_vec(),
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    };
    let path = Mutex::new(Some(peer.local_addr().unwrap()));
    let enabled = AtomicBool::new(true);
    assert!(stop_p2p_path(
        &socket,
        Some((relay.local_addr().unwrap(), 0xaabb_ccdd)),
        &path,
        &enabled,
        &params,
        true,
    ));
    assert!(!enabled.load(Ordering::Acquire));
    assert_eq!(*path.lock().unwrap(), None);

    for expected_seq in 0..3u32 {
        let mut packet = [0u8; 128];
        let (len, _) = peer.recv_from(&mut packet).unwrap();
        let (header, body) = p2p::parse(&packet[..len]).unwrap();
        assert_eq!(header.kind, p2p::SIGNAL);
        assert_eq!(header.call_id, 123);
        assert!(!header.is_callee);
        assert_eq!(body[0], 2);
        assert_eq!(
            u32::from_le_bytes(body[1..5].try_into().unwrap()),
            expected_seq
        );

        let (_len, _) = relay.recv_from(&mut packet).unwrap();
        assert_eq!(packet[0], msg_types::CONTROL);
        assert_eq!(
            u32::from_le_bytes(packet[14..18].try_into().unwrap()),
            0xaabb_ccdd
        );
        assert_eq!(
            u16::from_le_bytes(packet[18..20].try_into().unwrap()),
            crate::protocol::zrtp::commands::REQ_FORWARD
        );
        assert_eq!(packet[20], 2);
    }
    assert!(!stop_p2p_path(
        &socket, None, &path, &enabled, &params, true
    ));
}

#[test]
fn network_change_sends_change_address_ping_and_relay_request() {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    relay
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let params = MediaSessionParams {
        call_id: 123,
        host_call: 111,
        uid: 111,
        peer_uid: 222,
        sess_id: b"network-change-session".to_vec(),
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    };
    send_change_address(
        &socket,
        Some((relay.local_addr().unwrap(), 0x1122_3344)),
        &params,
        9,
    );
    let mut commands_seen = Vec::new();
    for _ in 0..3 {
        let mut packet = [0u8; 256];
        let (len, _) = relay.recv_from(&mut packet).unwrap();
        assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        commands_seen.push(u16::from_le_bytes(packet[18..20].try_into().unwrap()));
    }
    assert_eq!(
        commands_seen,
        vec![
            crate::protocol::zrtp::commands::CHANGE_ADDRESS,
            crate::protocol::zrtp::commands::PING,
            crate::protocol::zrtp::commands::REQ_FORWARD,
        ]
    );
}

#[test]
fn ring_ping_interval_honors_zcall_ping_ms_and_keeps_a_safe_default() {
    assert_eq!(
        ring_ping_interval_from(Some("100")),
        Duration::from_millis(100)
    );
    assert_eq!(ring_ping_interval_from(None), Duration::from_secs(6));
    assert_eq!(ring_ping_interval_from(Some("0")), Duration::from_secs(6));
    assert_eq!(
        ring_ping_interval_from(Some("70000")),
        Duration::from_secs(6)
    );
}

#[test]
fn ring_ping_reaches_every_candidate_at_the_configured_interval_and_stops() {
    let relay_a = UdpSocket::bind("127.0.0.1:0").unwrap();
    let relay_b = UdpSocket::bind("127.0.0.1:0").unwrap();
    for relay in [&relay_a, &relay_b] {
        relay
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
    }
    let backend = ZrtcMediaBackend::new().unwrap();
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 77,
        host_call: 111,
        uid: 111,
        peer_uid: 222,
        sess_id: b"ring-ping-session".to_vec(),
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    });
    *backend.ring_servers.lock().unwrap() = vec![
        (relay_a.local_addr().unwrap(), 0x1111),
        (relay_b.local_addr().unwrap(), 0x2222),
    ];
    backend.start_ring_ping_with_interval(Duration::from_millis(50));

    for (relay, expected_token) in [(&relay_a, 0x1111u32), (&relay_b, 0x2222u32)] {
        for _ in 0..2 {
            let mut packet = [0u8; 256];
            let (len, _) = relay.recv_from(&mut packet).unwrap();
            assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
            assert_eq!(packet[0], msg_types::CONTROL);
            assert_eq!(
                u32::from_le_bytes(packet[14..18].try_into().unwrap()),
                expected_token
            );
            assert_eq!(
                u16::from_le_bytes(packet[18..20].try_into().unwrap()),
                crate::protocol::zrtp::commands::PING
            );
        }
    }

    backend.ring_ping_running.store(false, Ordering::Release);
    thread::sleep(Duration::from_millis(120));
    for relay in [&relay_a, &relay_b] {
        relay
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut packet = [0u8; 256];
        assert!(relay.recv_from(&mut packet).is_err());
    }
}

#[test]
fn selecting_a_relay_pings_it_and_ends_other_ring_sessions() {
    let selected = UdpSocket::bind("127.0.0.1:0").unwrap();
    let other = UdpSocket::bind("127.0.0.1:0").unwrap();
    selected
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();
    other
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();
    let backend = ZrtcMediaBackend::new().unwrap();
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 77,
        host_call: 111,
        uid: 111,
        peer_uid: 222,
        sess_id: b"ring-select-session".to_vec(),
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    });
    let selected_addr = selected.local_addr().unwrap();
    let other_addr = other.local_addr().unwrap();
    *backend.selected_server.lock().unwrap() = Some((selected_addr, 0xaaaa));
    *backend.ring_servers.lock().unwrap() = vec![(selected_addr, 0xaaaa), (other_addr, 0xbbbb)];

    backend.start_with_audio(false);
    let mut selected_packet = [0u8; 256];
    selected.recv_from(&mut selected_packet).unwrap();
    assert_eq!(
        u16::from_le_bytes(selected_packet[18..20].try_into().unwrap()),
        crate::protocol::zrtp::commands::PING
    );
    let mut other_packet = [0u8; 256];
    other.recv_from(&mut other_packet).unwrap();
    assert_eq!(
        u16::from_le_bytes(other_packet[18..20].try_into().unwrap()),
        crate::protocol::zrtp::commands::END_CALL
    );
    backend.stop();
}

#[test]
fn group_camera_advertisement_is_not_disabled_by_srtp_mode() {
    assert!(group_camera_advertised(true, true));
    assert!(!group_camera_advertised(false, true));
    assert!(!group_camera_advertised(true, false));
}

#[test]
fn clear_group_h264_probe_only_accepts_annex_b_or_group_fragments() {
    assert!(looks_like_group_h264_payload(&[0, 0, 0, 1, 0x65, 1]));
    assert!(looks_like_group_h264_payload(&[0, 0, 1, 0x41, 1]));
    assert!(looks_like_group_h264_payload(&[0x7c, 0x85, 0x11, 0x22]));
    assert!(looks_like_group_h264_payload(&[0x5c, 0x85, 0x11, 0x22]));
    assert!(!looks_like_group_h264_payload(&[0x7c, 0x80]));
    assert!(!looks_like_group_h264_payload(&[0x61, 0x85, 0x11]));
    assert!(looks_like_group_h264_payload(&[0x7c, 0x85, 1, 2]));
    assert!(looks_like_group_h264_payload(&[0x7c, 0x45, 1, 2]));
    assert!(!looks_like_group_h264_payload(&[0x9a, 0x7f, 0x21, 0x44]));
    assert!(!looks_like_group_h264_payload(&[0x7c, 0x9c, 1, 2]));
}

#[test]
fn group_periodic_ping_sequence_shares_counter_with_video_controls() {
    let control_seq = AtomicU32::new(0);
    let ping_seq = next_group_control_sequence(&control_seq);
    let config_seq = next_group_control_sequence(&control_seq);
    let request_seq = next_group_control_sequence(&control_seq);
    assert_eq!((ping_seq, config_seq, request_seq), (0, 1, 2));

    let ping = build_zavi_ping_packet(111, 0x1234, ping_seq, b"group-session", true);
    let config = build_group_video_quality_config(
        111,
        0x1234,
        config_seq,
        &[GroupVideoLayer {
            id: 0,
            bitrate_kbps: 300,
            width: 640,
            height: 360,
        }],
    );
    let request = build_group_video_quality_request(
        111,
        0x1234,
        request_seq,
        &[GroupVideoQualityRequest {
            uid: 222,
            quality_id: 0,
        }],
    );
    for (packet, expected) in [(&ping, 0), (&config, 1), (&request, 2)] {
        assert_eq!(
            u32::from_le_bytes(packet[6..10].try_into().unwrap()),
            expected
        );
    }
}

#[test]
fn selecting_a_group_relay_sends_zavi_ping_not_one_to_one_ping() {
    use crate::protocol::zrtp::commands;

    let selected = UdpSocket::bind("127.0.0.1:0").unwrap();
    let other = UdpSocket::bind("127.0.0.1:0").unwrap();
    selected
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();
    other
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();

    let backend = ZrtcMediaBackend::new().unwrap();
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 77,
        host_call: 111,
        uid: 111,
        peer_uid: 111,
        sess_id: b"group-select-session".to_vec(),
        is_caller: true,
        is_group: true,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    });
    let selected_addr = selected.local_addr().unwrap();
    let other_addr = other.local_addr().unwrap();
    *backend.selected_server.lock().unwrap() = Some((selected_addr, 0xaaaa));
    *backend.ring_servers.lock().unwrap() = vec![(selected_addr, 0xaaaa), (other_addr, 0xbbbb)];

    backend.finish_ring_selection();

    let mut selected_packet = [0u8; 256];
    let (selected_len, _) = selected.recv_from(&mut selected_packet).unwrap();
    assert_eq!(
        u16::from_le_bytes(selected_packet[18..20].try_into().unwrap()),
        commands::INIT_ZAVI_PING,
        "group relay must receive ZaviPing immediately after selection"
    );
    assert_eq!(selected_packet[20], 1);
    assert_eq!(
        u32::from_le_bytes(selected_packet[6..10].try_into().unwrap()),
        0,
        "first group control packet consumes sequence zero"
    );
    assert_eq!(backend.group_control_seq.load(Ordering::Relaxed), 1);
    assert!(selected_len >= 38);

    let mut other_packet = [0u8; 256];
    other.recv_from(&mut other_packet).unwrap();
    assert_eq!(
        u16::from_le_bytes(other_packet[18..20].try_into().unwrap()),
        commands::END_CALL
    );
    assert!(backend.last_ping.lock().unwrap().elapsed() < Duration::from_secs(1));
    backend.stop();
}

#[test]
fn group_screen_share_uses_a_second_socket_uid_token_and_rtp_stream() {
    use crate::protocol::zrtp::{
        build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
    };
    use std::thread;

    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    relay
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let init_socket = relay.try_clone().unwrap();
    let relay_thread = thread::spawn(move || {
        let mut buf = [0u8; 1500];
        let (len, client) = init_socket
            .recv_from(&mut buf)
            .expect("receive group InitZRTP");
        assert_eq!(buf[0], zrtp_types::CONTROL);
        let mut accepted = build_control_header(
            zrtp_types::SERVER_REPLY,
            MAGIC_BYTE,
            1,
            111,
            0,
            commands::INIT_CALL_CALLER,
            2,
        )
        .to_vec();
        accepted.extend_from_slice(&0u32.to_le_bytes());
        accepted.extend_from_slice(&77u32.to_le_bytes());
        accepted.extend_from_slice(&0xaabb_ccddu32.to_le_bytes());
        accepted.extend_from_slice(&0u16.to_le_bytes());
        init_socket.send_to(&accepted, client).unwrap();
        assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
    });

    let backend = ZrtcMediaBackend::new().unwrap();
    backend
        .prepare(
            MediaSessionParams {
                call_id: 77,
                host_call: 0x1234,
                uid: 111,
                peer_uid: 222,
                sess_id: b"group-share-session".to_vec(),
                is_caller: true,
                is_group: true,
                send_udp: true,
                video_enabled: false,
                video_config: serde_json::json!({
                    "maxShrScreenBr":1400,
                    "groupcall":{"codec":60,"temporalNum":1,"spatialLayers":[
                        {"factorNum":1,"factorDen":2,"bitrateKbps":700,"fps":24}
                    ]}
                }),
                p2p_candidates: Vec::new(),
            },
            &[relay_addr],
        )
        .unwrap();
    relay_thread.join().unwrap();
    let main_port = backend.socket.local_addr().unwrap().port();
    backend.start_with_audio(false);
    let mut selected_ping = [0u8; 1500];
    let (_ping_len, _) = relay.recv_from(&mut selected_ping).unwrap();
    assert_eq!(
        u16::from_le_bytes(selected_ping[18..20].try_into().unwrap()),
        commands::INIT_ZAVI_PING
    );
    backend.start_group_share(777).unwrap();

    let mut init = [0u8; 1500];
    let (_init_len, share_client) = relay.recv_from(&mut init).unwrap();
    assert_eq!(init[0], zrtp_types::CONTROL);
    assert_eq!(u32::from_le_bytes(init[10..14].try_into().unwrap()), 777);
    assert_ne!(
        share_client.port(),
        main_port,
        "share must own a second UDP socket"
    );
    assert_eq!(
        u16::from_le_bytes(init[18..20].try_into().unwrap()),
        commands::INIT_CALL_CALLEE
    );

    let mut accepted = build_control_header(
        zrtp_types::SERVER_REPLY,
        MAGIC_BYTE,
        1,
        777,
        0,
        commands::INIT_CALL_CALLEE,
        4,
    )
    .to_vec();
    accepted.extend_from_slice(&0u32.to_le_bytes());
    accepted.extend_from_slice(&77u32.to_le_bytes());
    accepted.extend_from_slice(&0x1122_3344u32.to_le_bytes());
    accepted.extend_from_slice(&0u16.to_le_bytes());
    relay.send_to(&accepted, share_client).unwrap();

    let ready_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let ready = backend
            .share_peer
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|peer| peer.token == 0x1122_3344);
        if ready || Instant::now() >= ready_deadline {
            assert!(ready, "share InitZRTP response should establish a token");
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    assert!(backend
        .drain_group_events()
        .contains(&GroupMediaEvent::ShareReady));
    assert_eq!(
        backend
            .send_group_share_frame(true, &[0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0xa0], 1280, 720)
            .unwrap(),
        1
    );

    let mut video = None;
    for _ in 0..8 {
        let mut packet = [0u8; 1500];
        let Ok((len, _)) = relay.recv_from(&mut packet) else {
            break;
        };
        if packet[0] == zrtp_types::VIDEO_RTP {
            video = Some(packet[..len].to_vec());
            break;
        }
    }
    let video = video.expect("screen-share RTP should reach the SFU");
    assert_eq!(
        u32::from_le_bytes(video[1..5].try_into().unwrap()),
        0x1122_3344
    );
    let rtp = &video[5..];
    assert_eq!(u32::from_be_bytes(rtp[8..12].try_into().unwrap()), 777);
    assert_eq!(rtp[23] >> 4, 12);

    backend.stop_group_share();
    let mut end = [0u8; 1500];
    let (end_len, _) = relay.recv_from(&mut end).unwrap();
    assert_eq!(end[0], zrtp_types::CONTROL);
    assert_eq!(u32::from_le_bytes(end[10..14].try_into().unwrap()), 777);
    assert_eq!(
        u16::from_le_bytes(end[18..20].try_into().unwrap()),
        commands::END_CALL
    );
    assert!(end_len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
    backend.stop();
}

#[test]
fn group_share_worker_recognizes_targeted_pli_and_fir() {
    let pli = [0x81, 206, 0, 2, 0, 0, 0, 1, 0, 0, 3, 9];
    assert!(rtcp_requests_keyframe(&pli, 0x0309));
    assert!(!rtcp_requests_keyframe(&pli, 0x0310));

    let fir = [0x84, 206, 0, 3, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 3, 9];
    assert!(rtcp_requests_keyframe(&fir, 0x0309));
    assert!(!rtcp_requests_keyframe(&fir, 0x0310));
}

#[test]
fn legacy_group_downlink_accepts_alternate_relay_endpoint_only_for_group() {
    let selected: SocketAddr = "192.0.2.10:4000".parse().unwrap();
    let same_host_other_port: SocketAddr = "192.0.2.10:5000".parse().unwrap();
    let other_host: SocketAddr = "192.0.2.11:4000".parse().unwrap();

    assert!(accepts_relay_source(Some(selected), selected, true));
    assert!(accepts_relay_source(
        Some(selected),
        same_host_other_port,
        true
    ));
    assert!(accepts_relay_source(Some(selected), other_host, true));
    assert!(!accepts_relay_source(
        Some(selected),
        same_host_other_port,
        false
    ));
    assert!(!accepts_relay_source(Some(selected), other_host, false));
}

#[test]
fn one_to_one_media_watchdog_expires_after_twenty_seconds_without_peer_media() {
    assert!(!media_receive_timed_out(0, 50_000));
    assert!(!media_receive_timed_out(10_000, 29_999));
    assert!(media_receive_timed_out(10_000, 30_000));
    assert!(!media_receive_timed_out(30_000, 29_000));
}

#[test]
fn member_speaking_uses_four_of_ten_audio_levels_and_expires_after_silence() {
    let now = Instant::now();
    let mut tracker = MemberSpeakingTracker::default();
    for index in 0..3 {
        assert_eq!(
            tracker.observe(45, now + Duration::from_millis(index * 20)),
            None
        );
    }
    assert_eq!(
        tracker.observe(45, now + Duration::from_millis(60)),
        Some(true)
    );
    assert_eq!(tracker.observe(80, now + Duration::from_millis(80)), None);
    assert!(!tracker.expire(now + Duration::from_millis(659)));
    assert!(tracker.expire(now + Duration::from_millis(660)));
    assert_eq!(tracker.observe(80, now + Duration::from_millis(680)), None);
}

#[test]
fn plain_group_audio_parser_handles_header_extensions_and_padding() {
    let header =
        build_group_audio_rtp_header(42, 320, 0x1234, 7, false, 112, 1_700_000_000_000, 35);
    let mut packet = header;
    packet.extend_from_slice(&[0xf8, 0xff, 0xfe, 0x01]);
    packet.extend_from_slice(&[0, 2]); // RTP padding count = 2
    packet[0] |= 0x20;
    let parsed = read_plain_rtp_payload(&packet).expect("parse group RTP payload");
    assert_eq!(parsed.0, 42);
    assert_eq!(parsed.1, 0x1234);
    assert_eq!(parsed.2, [0xf8, 0xff, 0xfe, 0x01]);
    assert!(read_plain_rtp_payload(&packet[..20]).is_none());
}

#[test]
fn caller_installs_candidates_from_answer_before_media_binder_starts() {
    let backend = ZrtcMediaBackend::new().unwrap();
    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    let params = MediaSessionParams {
        call_id: 7,
        host_call: 0,
        uid: 10,
        peer_uid: 20,
        sess_id: vec![1; 30],
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    };
    backend
        .prepare(params, &[relay.local_addr().unwrap()])
        .unwrap();
    backend
        .set_p2p_candidates(vec!["192.0.2.5:42000".parse().unwrap()])
        .unwrap();
    let stored = backend.params.lock().unwrap().clone().unwrap();
    assert_eq!(
        stored.p2p_candidates,
        vec!["192.0.2.5:42000".parse().unwrap()]
    );
}

#[test]
fn active_audio_device_switch_updates_generation_without_restarting_media() {
    let backend = ZrtcMediaBackend::new().unwrap();
    let generation = backend.device_generation.load(Ordering::Acquire);
    backend.set_device("mic", "alsa_input.usb-headset");
    let after_mic = backend.device_generation.load(Ordering::Acquire);
    assert_eq!(after_mic, generation + 1);
    assert_eq!(
        backend.selected_devices().0.as_deref(),
        Some("alsa_input.usb-headset")
    );

    backend.set_device("mic", "alsa_input.usb-headset");
    assert_eq!(backend.device_generation.load(Ordering::Acquire), after_mic);

    backend.set_device("speaker", "alsa_output.usb-headset");
    assert_eq!(
        backend.device_generation.load(Ordering::Acquire),
        after_mic + 1
    );
    assert_eq!(
        backend.selected_devices().1.as_deref(),
        Some("alsa_output.usb-headset")
    );

    backend.set_device("mic", "");
    assert_eq!(
        backend.device_generation.load(Ordering::Acquire),
        after_mic + 2
    );
    assert_eq!(backend.selected_devices().0, None);
}

#[test]
fn prepare_collects_all_accepted_relays_and_selects_first_successful_reply() {
    use crate::protocol::zrtp::{
        build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
    };
    use std::thread;

    fn relay(delay: Duration, public: &'static [u8]) -> (SocketAddr, thread::JoinHandle<()>) {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let addr = socket.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let mut buf = [0u8; 1500];
            let (len, client) = socket.recv_from(&mut buf).expect("receive InitZRTP");
            assert_eq!(buf[0], zrtp_types::CONTROL);
            let _ = len;
            thread::sleep(delay);
            let header = build_control_header(
                zrtp_types::SERVER_REPLY,
                MAGIC_BYTE,
                1,
                111,
                0,
                commands::INIT_CALL_CALLER,
                2,
            );
            let mut reply = header.to_vec();
            reply.extend_from_slice(&0u32.to_le_bytes());
            reply.extend_from_slice(&77u32.to_le_bytes());
            reply.extend_from_slice(&0xabcdu32.to_le_bytes());
            reply.extend_from_slice(&(public.len() as u16).to_le_bytes());
            reply.extend_from_slice(public);
            socket.send_to(&reply, client).expect("send InitZRTP reply");
        });
        (addr, handle)
    }

    let media = ZrtcMediaBackend::new().unwrap();
    let (slow_addr, slow_thread) = relay(Duration::from_millis(120), b"198.51.100.10|4000");
    let (fast_addr, fast_thread) = relay(Duration::from_millis(10), b"198.51.100.20|5000");
    let servers = [slow_addr, fast_addr];
    let selected = media
        .prepare(
            MediaSessionParams {
                call_id: 77,
                host_call: 0,
                uid: 111,
                peer_uid: 222,
                sess_id: vec![0x42; 30],
                is_caller: true,
                is_group: false,
                send_udp: true,
                video_enabled: false,
                video_config: Value::Null,
                p2p_candidates: Vec::new(),
            },
            &servers,
        )
        .unwrap();
    slow_thread.join().unwrap();
    fast_thread.join().unwrap();
    assert_eq!(selected, Some(servers[1]));
    assert_eq!(media.selected_server.lock().unwrap().unwrap().0, servers[1]);
    assert_eq!(media.ring_servers.lock().unwrap().len(), 2);
    assert_eq!(
        media.public_address().as_deref(),
        Some("198.51.100.20|5000")
    );
    media.stop();
}

#[test]
fn stop_during_ringing_sends_end_call_before_media_workers_start() {
    use crate::protocol::zrtp::{
        build_control_header, commands, msg_types as zrtp_types, MAGIC_BYTE,
    };
    use std::thread;

    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    relay
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 1500];
        let (len, client) = relay.recv_from(&mut buf).expect("receive InitZRTP");
        assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
        let mut reply = build_control_header(
            zrtp_types::SERVER_REPLY,
            MAGIC_BYTE,
            1,
            111,
            0,
            commands::INIT_CALL_CALLER,
            2,
        )
        .to_vec();
        reply.extend_from_slice(&0u32.to_le_bytes());
        reply.extend_from_slice(&77u32.to_le_bytes());
        reply.extend_from_slice(&0xabcdu32.to_le_bytes());
        reply.extend_from_slice(&0u16.to_le_bytes());
        relay.send_to(&reply, client).unwrap();
        let (len, _) = relay
            .recv_from(&mut buf)
            .expect("receive EndCall while ringing");
        assert_eq!(buf[0], zrtp_types::CONTROL);
        let cmd = u16::from_le_bytes([buf[18], buf[19]]);
        assert_eq!(cmd, commands::END_CALL);
        assert!(len >= crate::protocol::zrtp::CONTROL_HEADER_LEN);
    });

    let media = ZrtcMediaBackend::new().unwrap();
    media
        .prepare(
            MediaSessionParams {
                call_id: 77,
                host_call: 0,
                uid: 111,
                peer_uid: 222,
                sess_id: vec![0x42; 30],
                is_caller: true,
                is_group: false,
                send_udp: true,
                video_enabled: false,
                video_config: Value::Null,
                p2p_candidates: Vec::new(),
            },
            &[relay_addr],
        )
        .unwrap();
    assert!(!media.running.load(Ordering::Relaxed));
    media.stop();
    handle.join().unwrap();
}

#[test]
fn incoming_h264_frame_is_srtp_verified_and_foreign_sources_are_dropped() {
    let relay = UdpSocket::bind("127.0.0.1:0").expect("bind selected relay");
    let relay_addr = relay.local_addr().unwrap();
    let rogue = UdpSocket::bind("127.0.0.1:0").expect("bind unrelated source");

    let backend = ZrtcMediaBackend::new().expect("create media backend");
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 123,
        host_call: 0,
        uid: 0x1234,
        peer_uid: 0x5678,
        sess_id: vec![1; 30],
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    });
    *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
    let master = [0x42; 30];
    let keys = session_keys(&master).unwrap();
    *backend.keys.lock().unwrap() = Some(session_keys(&master).unwrap());
    let bound = backend.socket.local_addr().unwrap();
    let local = SocketAddr::from(([127, 0, 0, 1], bound.port()));
    backend.start();

    let annex_b = [
        0, 0, 0, 1, 0x67, 0x64, 0, 0x1e, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65,
        0x88, 0x84, 0x21, 0x01,
    ];
    let chunks = packetize_frame(&annex_b, 9);
    assert_eq!(chunks.len(), 3);
    let mut packets = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        let seq = 7 + index as u16;
        let header = build_audio_rtp_header(
            seq,
            9000,
            0x1234,
            55 + index as u16,
            index + 1 == chunks.len(),
            VIDEO_PT_KEY,
        );
        let protected = protect_rtp(&header, chunk, &keys, seq, 0x1234, 0);
        let mut packet = vec![msg_types::VIDEO_DOWN];
        packet.extend_from_slice(&protected);
        packets.push(packet);
    }

    // A correctly authenticated RTP datagram from an unselected source
    // must not be allowed to inject a video frame.
    rogue.send_to(&packets[0], local).unwrap();
    thread::sleep(Duration::from_millis(60));
    assert!(backend.drain_video_frames().is_empty());

    // Deliver the last fragment before the middle fragment, as happens
    // when relay and direct-path copies take different network routes.
    relay.send_to(&packets[0], local).unwrap();
    relay.send_to(&packets[2], local).unwrap();
    relay.send_to(&packets[1], local).unwrap();
    let mut received = Vec::new();
    for _ in 0..100 {
        received = backend.drain_video_frames();
        if !received.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(received.len(), 1);
    assert!(received[0].key);
    assert_eq!(received[0].data, annex_b);
    backend.stop();
}

#[test]
fn remote_group_share_is_plain_rtp_even_when_camera_peer_has_srtp_keys() {
    use std::thread;

    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let backend = ZrtcMediaBackend::new().unwrap();
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 123,
        host_call: 111,
        uid: 111,
        peer_uid: 222,
        sess_id: vec![1; 30],
        is_caller: true,
        is_group: true,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    });
    *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
    *backend.keys.lock().unwrap() = Some(session_keys(&[0x42; 30]).unwrap());
    backend.set_group_share_remote_uid(777);
    let bound = backend.socket.local_addr().unwrap();
    let local = SocketAddr::from(([127, 0, 0, 1], bound.port()));
    backend.start_with_audio(false);

    let annex_b = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21, 0x01];
    let mut rtp = build_group_video_rtp_header(
        7,
        9000,
        777,
        55,
        true,
        100,
        false,
        12,
        1,
        unix_now_ms(),
        unix_now_ms(),
    );
    rtp.extend_from_slice(&annex_b);
    let mut packet = vec![msg_types::VIDEO_DOWN];
    packet.extend_from_slice(&rtp);
    relay.send_to(&packet, local).unwrap();

    let mut received = Vec::new();
    for _ in 0..100 {
        received = backend.drain_video_frames();
        if !received.is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].src, Some(777));
    assert!(received[0].key);
    assert_eq!(received[0].data, annex_b);
    backend.stop();
}

#[test]
fn p2p_bind_keepalive_and_media_duplicate_use_authenticated_call_path() {
    use crate::protocol::zrtp::p2p;
    use std::thread;

    let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
    relay
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let peer_addr = peer.local_addr().unwrap();
    let backend = ZrtcMediaBackend::new().unwrap();
    let call_id = 0x1234_5678;
    let local_uid = 0x1111;
    let peer_uid = 0x2222;
    let master = [0x42; 30];
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id,
        host_call: 0,
        uid: local_uid,
        peer_uid,
        sess_id: vec![1; 30],
        is_caller: false,
        is_group: false,
        send_udp: true,
        video_enabled: true,
        video_config: Value::Null,
        p2p_candidates: vec![peer_addr],
    });
    *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
    *backend.keys.lock().unwrap() = Some(session_keys(&master).unwrap());
    *backend.rtcp.lock().unwrap() = Some(RtcpFeedback::new(local_uid));
    backend.start_with_audio(false);

    // The caller sends a BIND_PEER reply to the callee's bind attempt.
    let mut bind_peer = p2p::header(false, call_id, p2p::BIND_PEER).to_vec();
    bind_peer.extend_from_slice(&[0x34, 0x12, 0, 0, 0, 1]);
    bind_peer.extend_from_slice(&0u32.to_le_bytes());
    bind_peer.push(0xff);
    peer.send_to(&bind_peer, backend.socket.local_addr().unwrap())
        .unwrap();

    let mut buffer = [0u8; 1600];
    let mut bind_answer_seen = false;
    for _ in 0..40 {
        let Ok((length, _)) = peer.recv_from(&mut buffer) else {
            continue;
        };
        if let Some((header, body)) = p2p::parse(&buffer[..length]) {
            if header.kind == p2p::BIND_ANSWER {
                assert!(header.is_callee);
                assert_eq!(header.call_id, call_id);
                assert!(body.len() >= 6 + 4);
                bind_answer_seen = true;
                break;
            }
        }
    }
    assert!(
        bind_answer_seen,
        "native engine should answer the peer's BIND_PEER"
    );

    // A valid peer keepalive completes the NAT path and gets a pong.
    let ping = p2p::wrap(false, call_id, p2p::KEEPALIVE, &[1]);
    peer.send_to(&ping, backend.socket.local_addr().unwrap())
        .unwrap();
    let mut pong_seen = false;
    for _ in 0..40 {
        let Ok((length, _)) = peer.recv_from(&mut buffer) else {
            continue;
        };
        if let Some((header, body)) = p2p::parse(&buffer[..length]) {
            if header.kind == p2p::KEEPALIVE && body.first() == Some(&2) {
                assert!(header.is_callee);
                pong_seen = true;
                break;
            }
        }
    }
    assert!(pong_seen, "native engine should answer peer keepalive");
    assert_eq!(*backend.p2p_path.lock().unwrap(), Some(peer_addr));

    // Once the path is up the same protected H.264 packet is sent by both
    // relay and peer UDP, with each transport's own envelope.
    let frame = [0, 0, 0, 1, 0x65, 0x88, 0x84, 0x21];
    assert_eq!(backend.send_video_frame(true, &frame).unwrap(), 1);
    let (relay_len, _) = relay.recv_from(&mut buffer).unwrap();
    assert_eq!(buffer[0], msg_types::VIDEO_RTP);
    let mut direct_seen = false;
    for _ in 0..40 {
        let Ok((length, _)) = peer.recv_from(&mut buffer) else {
            continue;
        };
        if let Some((header, body)) = p2p::parse(&buffer[..length]) {
            if header.kind == p2p::VIDEO {
                assert!(header.is_callee);
                assert_eq!(body.len(), relay_len - 5);
                direct_seen = true;
                break;
            }
        }
    }
    assert!(
        direct_seen,
        "native video should also be sent over the direct path"
    );
    backend.stop();
    thread::sleep(Duration::from_millis(30));
}

#[test]
fn voice_only_session_never_sends_video_frames() {
    let backend = ZrtcMediaBackend::new().unwrap();
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 1,
        host_call: 0,
        uid: 111,
        peer_uid: 222,
        sess_id: b"voice-only-session".to_vec(),
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: false,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    });
    assert_eq!(
        backend
            .send_video_frame(true, &[0, 0, 0, 1, 0x65, 0x88])
            .unwrap(),
        0
    );
}

#[test]
fn outgoing_h264_frame_is_fragmented_srtp_protected_and_sent_to_relay() {
    let relay = UdpSocket::bind("127.0.0.1:0").expect("bind test relay");
    relay
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("set relay timeout");
    let relay_addr = relay.local_addr().unwrap();

    let backend = ZrtcMediaBackend::new().expect("create media backend");
    *backend.params.lock().unwrap() = Some(MediaSessionParams {
        call_id: 123,
        host_call: 0,
        uid: 0x1234,
        peer_uid: 0x5678,
        sess_id: vec![1; 30],
        is_caller: true,
        is_group: false,
        send_udp: true,
        video_enabled: true,
        video_config: Value::Null,
        p2p_candidates: Vec::new(),
    });
    *backend.selected_server.lock().unwrap() = Some((relay_addr, 0xaabb_ccdd));
    let master = [0x42; 30];
    let keys = session_keys(&master).unwrap();
    *backend.keys.lock().unwrap() = Some(session_keys(&master).unwrap());
    *backend.rtcp.lock().unwrap() = Some(RtcpFeedback::new(0x1234));
    backend.start();

    let annex_b: Vec<u8> = (0..2500).map(|n| (n % 251) as u8).collect();
    assert_eq!(backend.send_video_frame(false, &annex_b).unwrap(), 0);
    assert!(backend.take_local_keyframe_request());
    assert_eq!(backend.send_video_frame(true, &annex_b).unwrap(), 3);

    let mut recovered = Vec::new();
    let mut original_packets = Vec::new();
    for index in 0..3 {
        let mut packet = [0u8; 1600];
        let len = loop {
            let (len, from) = relay.recv_from(&mut packet).expect("receive H264 RTP");
            assert_eq!(from.port(), backend.socket.local_addr().unwrap().port());
            if packet[0] == msg_types::VIDEO_RTP {
                break len;
            }
        };
        assert_eq!(
            u32::from_le_bytes(packet[1..5].try_into().unwrap()),
            0xaabb_ccdd
        );

        let rtp = &packet[5..len];
        original_packets.push(rtp.to_vec());
        let header_len = parse_rtp_header_len(rtp).unwrap();
        assert_eq!(header_len, 20);
        assert_eq!(rtp[1] & 0x7f, VIDEO_PT_KEY);
        assert_eq!((rtp[1] & 0x80) != 0, index == 2);
        let clear = unprotect_rtp(rtp, &keys, 0).expect("verify SRTP authentication and decrypt");
        let expected_flag = [0x80, 0x00, 0x40][index];
        assert_eq!(&clear.opus[..2], &[0x1c, expected_flag]);
        recovered.extend_from_slice(&clear.opus[2..]);
    }
    assert_eq!(recovered, annex_b);

    // Peer NACKs the middle RTP sequence. The engine must resend the
    // original authenticated packet from its short retransmission cache.
    let nack = build_nack_many(0x5678, 0x1234, &[1]);
    let feedback = wrap_media_packet(msg_types::VIDEO_FEC, 0xaabb_ccdd, &nack);
    relay
        .send_to(&feedback, backend.socket.local_addr().unwrap())
        .unwrap();
    let mut retransmitted = [0u8; 1600];
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut recovered_nack = false;
    while Instant::now() < deadline {
        let Ok((len, from)) = relay.recv_from(&mut retransmitted) else {
            continue;
        };
        assert_eq!(from.port(), backend.socket.local_addr().unwrap().port());
        if retransmitted[0] == msg_types::VIDEO_RTP
            && &retransmitted[5..len] == original_packets[1].as_slice()
        {
            recovered_nack = true;
            break;
        }
    }
    assert!(
        recovered_nack,
        "peer NACK should resend the original protected RTP packet"
    );

    backend.stop();
}
