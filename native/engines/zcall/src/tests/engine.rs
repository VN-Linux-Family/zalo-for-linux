use super::*;

fn engine() -> EngineCore {
    EngineCore::new(false, false, None)
}

fn group_engine() -> EngineCore {
    EngineCore::new(false, true, None)
}

fn test_call(phase: CallPhase) -> Call {
    Call {
        role: CallRole::Caller,
        phase,
        id: 1,
        peer_id: "peer".into(),
        peer_uid: 2,
        local_uid: 1,
        peer_name: "Peer".into(),
        avatar: String::new(),
        kind: 3,
        session_id: String::new(),
        server: None,
        servers: Vec::new(),
        p2p_candidates: Vec::new(),
        media_config: Value::Null,
        deadline: None,
        started_at: Some(Instant::now().checked_sub(Duration::from_secs(3)).unwrap()),
        audio: CallAudioFlags::default(),
        camera: CallCameraFlags {
            on: true,
            was_on: true,
            ..CallCameraFlags::default()
        },
        camera_state_reassert_at: None,
        peer_media: PeerMediaFlags::default(),
    }
}

#[test]
fn connected_call_ui_state_uses_epoch_milliseconds_for_the_timer() {
    let mut engine = engine();
    engine.call = Some(test_call(CallPhase::InCall));

    let call = engine.call.as_ref().unwrap();
    let state = engine.ui_state_value("connected", "connected", call);
    let since = state["since"]
        .as_u64()
        .expect("connected call has timer origin");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    assert!(since > 1_700_000_000_000);
    assert!(now.saturating_sub(since) >= 2_500);
    assert!(now.saturating_sub(since) < 4_000);
}

#[test]
fn outgoing_setup_and_ringing_timeouts_finish_as_zero_duration_outgoing_calls() {
    for phase in [CallPhase::Outgoing, CallPhase::Ringing] {
        let mut engine = engine();
        let mut call = test_call(phase);
        call.deadline = Some(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
        );
        engine.call = Some(call);
        engine.phase = phase;

        engine.tick();
        let frames = engine.poll_outgoing_frames();
        let cancel = command(&frames, "sendSignal", 405);
        assert_eq!(cancel["data"]["toId"], "peer");
        assert_eq!(cancel["data"]["callType"], 1);
        assert_eq!(cancel["data"]["status"], 0);
        assert!(!frames
            .iter()
            .any(|frame| frame["type"] == "sendSignal" && frame["command"] == 409));
        let ended = command(&frames, "sendSignal", 406);
        assert_eq!(ended["data"]["status"], 103);
        assert_eq!(ended["data"]["role"], true);
        assert_eq!(ended["data"]["duration"], 0);
        let bubble = frames
            .iter()
            .find(|frame| frame["type"] == "update" && frame["command"] == "bubble")
            .expect("timeout should create a missed-call bubble");
        assert_eq!(bubble["data"]["missed"], true);
        assert_eq!(
            bubble["data"]["reason"],
            if phase == CallPhase::Ringing { 2 } else { 5 }
        );
        assert_eq!(bubble["data"]["callType"], 1);
        assert!(engine.call.is_none());
        assert_eq!(engine.phase, CallPhase::Idle);
    }
}

#[test]
fn call_duration_matches_legacy_nearest_second_rounding() {
    let started_at = Some(
        Instant::now()
            .checked_sub(Duration::from_millis(1_600))
            .unwrap(),
    );
    assert_eq!(elapsed_seconds_rounded(started_at), 2);
}

#[test]
fn ignores_peer_camera_state_for_a_stale_call_id() {
    let mut engine = engine();
    engine.call = Some(test_call(CallPhase::InCall));

    engine.on_zalo_frame(&json!({
        "type":"control",
        "data":{
            "act":"onoff_camera",
            "data":{"callId":2,"params":{"type":0}}
        }
    }));

    assert!(!engine.call.as_ref().unwrap().peer_media.camera_off);
}

#[test]
fn stale_answer_with_string_call_id_does_not_connect_current_call() {
    let mut engine = engine();
    engine.call = Some(test_call(CallPhase::Outgoing));
    engine.phase = CallPhase::Outgoing;

    engine.on_zalo_frame(&json!({
        "type":"control",
        "data":{
            "act":"answer",
            "data":{"callId":"2","status":0,"params":{"rtpSerIp":"127.0.0.1"}}
        }
    }));

    assert_eq!(engine.call.as_ref().unwrap().phase, CallPhase::Outgoing);
    assert_eq!(engine.poll_outgoing_frames(), [] as [serde_json::Value; 0]);
}

fn command<'a>(frames: &'a [Value], kind: &str, command: u64) -> &'a Value {
    frames
        .iter()
        .find(|frame| {
            frame.get("type").and_then(Value::as_str) == Some(kind)
                && frame.get("command").and_then(Value::as_u64) == Some(command)
        })
        .unwrap_or_else(|| panic!("missing {kind} command {command}: {frames:#?}"))
}

#[test]
fn sustained_early_audio_connects_a_ringing_caller_before_answer_signal() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.on_zalo_frame(&json!({
        "type":"request","command":"makeCall",
        "data":{"type":1,"partner":[{"id":"peer-123","name":"Peer","avatar":"avatar"}]}
    }));
    let initial = engine.poll_outgoing_frames();
    let call_id = command(&initial, "sendSignal", 401)["data"]["callId"]
        .as_u64()
        .unwrap();

    engine.on_zalo_frame(&json!({
        "type":"recvSignal","command":401,"data":{
            "status":0,"sessId":"012345678901234567890123456789",
            "fromId":111,"toId":222,
            "servers":[{"rtpaddr":"127.0.0.1:9000","rtcpaddr":"127.0.0.1:9001"}]
        }
    }));
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"ring_ring","data":{"callId":call_id}}
    }));
    engine.on_early_audio("first");
    assert_eq!(engine.phase, CallPhase::Connecting);
    assert_eq!(engine.call.as_ref().unwrap().phase, CallPhase::Connecting);

    engine.on_early_audio("connect");
    assert_eq!(engine.phase, CallPhase::InCall);
    let connected = engine.poll_outgoing_frames();
    assert_eq!(
        command(&connected, "sendSignal", 408)["data"]["callId"],
        call_id
    );
}

#[test]
fn caller_accepts_init_answer_after_ring_notification() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.on_zalo_frame(&json!({
        "type":"request","command":"makeCall",
        "data":{"type":1,"partner":[{"id":"peer-123","name":"Peer"}]}
    }));
    let initial = engine.poll_outgoing_frames();
    let call_id = command(&initial, "sendSignal", 401)["data"]["callId"]
        .as_u64()
        .unwrap();

    // Ring notifications can precede the 401 relay/session response.
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"ring_ring","data":{"callId":call_id}}
    }));
    assert_eq!(engine.call.as_ref().unwrap().phase, CallPhase::Ringing);

    engine.on_zalo_frame(&json!({
        "type":"recvSignal","command":401,"data":{
            "status":0,"sessId":"012345678901234567890123456789",
            "fromId":111,"toId":222,
            "servers":[{"rtpaddr":"127.0.0.1:9000","rtcpaddr":"127.0.0.1:9001"}]
        }
    }));
    let negotiated = engine.poll_outgoing_frames();
    assert!(negotiated
        .iter()
        .any(|frame| { frame["type"] == "sendSignal" && frame["command"] == 416 }));
}

#[test]
fn outgoing_call_negotiates_relay_and_finishes_over_call_v2() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.on_zalo_frame(&json!({
        "type":"request","command":"makeCall",
        "data":{"type":1,"partner":[{"id":"peer-123","name":"Peer","avatar":"avatar"}]}
    }));
    let initial = engine.poll_outgoing_frames();
    let init = command(&initial, "sendSignal", 401);
    let call_id = init["data"]["callId"].as_u64().unwrap();
    assert_eq!(init["data"]["codec"], EMPTY_CODEC);
    assert!(initial
        .iter()
        .any(|frame| frame["type"] == "update" && frame["command"] == "callState"));
    assert!(initial
        .iter()
        .any(|frame| frame["type"] == "request" && frame["command"] == "getAliasName"));

    engine.on_zalo_frame(&json!({
        "type":"recvSignal","command":401,"data":{
            "status":0,"sessId":"012345678901234567890123456789",
            "fromId":111,"toId":222,
            "servers":[{"rtpaddr":"127.0.0.1:9000","rtcpaddr":"127.0.0.1:9001"}]
        }
    }));
    let negotiated = engine.poll_outgoing_frames();
    let response = command(&negotiated, "sendSignal", 416);
    assert_eq!(response["data"]["callId"], call_id);
    assert_eq!(response["data"]["codec"], CODEC);
    let extend: Value =
        serde_json::from_str(response["data"]["extendData"].as_str().unwrap()).unwrap();
    assert_eq!(extend["serverAddr"][0]["rtp"], "127.0.0.1:9000");
    assert_eq!(extend["callType"], 0);
    assert_eq!(extend["sP2P"], 0);
    assert_eq!(extend["select2side"], 0);

    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"ring_ring","data":{"callId":call_id}}
    }));
    engine.on_zalo_frame(&json!({
        "type":"control","data":{
            "act":"answer",
            "data":{"callId":call_id,"params":{"status":0,"rtpSerIp":"127.0.0.1:9000"}}
        }
    }));
    let connected = engine.poll_outgoing_frames();
    assert_eq!(
        command(&connected, "sendSignal", 408)["data"]["callId"],
        call_id
    );
    assert_eq!(engine.phase, CallPhase::InCall);

    engine.on_zalo_frame(&json!({
        "type":"recvSignal","command":409,"data":{"callId":call_id + 1}
    }));
    assert_eq!(
        engine.phase,
        CallPhase::InCall,
        "a late end signal for an old call must be ignored"
    );
    assert_eq!(
        engine.call.as_ref().map(|call| call.id),
        Some(call_id as u32)
    );

    engine.on_zalo_frame(&json!({"type":"request","command":"endCall","data":{}}));
    let ended = engine.poll_outgoing_frames();
    assert_eq!(
        command(&ended, "sendSignal", 409)["data"]["callId"],
        call_id
    );
    assert_eq!(command(&ended, "sendSignal", 406)["data"]["status"], 50);
    assert_eq!(engine.phase, CallPhase::Idle);
}

#[test]
fn outgoing_remote_timeout_status_six_sends_legacy_unanswered_cancel_signal() {
    for phase in [CallPhase::Outgoing, CallPhase::Ringing] {
        for status in [6, 3] {
            let mut engine = engine();
            let call = test_call(phase);
            let call_id = call.id;
            engine.call = Some(call);
            engine.phase = phase;

            engine.on_zalo_frame(&json!({
                "type":"control","data":{"act":"answer","data":{
                    "callId":call_id,"params":{"status":status}
                }}
            }));
            let frames = engine.poll_outgoing_frames();
            let ended = command(&frames, "sendSignal", 406);
            if status == 6 {
                let cancel = command(&frames, "sendSignal", 405);
                assert_eq!(cancel["data"]["toId"], "peer");
                assert_eq!(cancel["data"]["callType"], 1);
                assert_eq!(cancel["data"]["status"], 0);
                assert!(!frames
                    .iter()
                    .any(|frame| frame["type"] == "sendSignal" && frame["command"] == 409));
            }
            assert_eq!(ended["data"]["duration"], 0);
            assert_eq!(ended["data"]["role"], true);
            assert_eq!(ended["data"]["status"], if status == 6 { 103 } else { 160 });
            let bubble = frames
                .iter()
                .find(|frame| frame["type"] == "update" && frame["command"] == "bubble")
                .expect("call completion should update the call bubble");
            assert_eq!(bubble["data"]["missed"], status == 6);
            if status == 6 {
                assert_eq!(
                    bubble["data"]["reason"],
                    if phase == CallPhase::Ringing { 2 } else { 5 }
                );
            }
            assert_eq!(engine.phase, CallPhase::Idle);
        }
    }
}

#[test]
fn video_call_reasserts_camera_state_after_peer_connects() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.on_zalo_frame(&json!({
        "type":"request","command":"makeCall",
        "data":{"type":3,"partner":[{"id":"peer-video","name":"Peer"}]}
    }));
    let initial = engine.poll_outgoing_frames();
    assert_eq!(command(&initial, "sendSignal", 12006)["data"]["isOnCam"], 0);
    let call_id = command(&initial, "sendSignal", 401)["data"]["callId"]
        .as_u64()
        .unwrap();

    engine.on_zalo_frame(&json!({
        "type":"recvSignal","command":401,"data":{
            "status":0,"sessId":"012345678901234567890123456789",
            "fromId":111,"toId":222,
            "servers":[{"rtpaddr":"127.0.0.1:9000","rtcpaddr":"127.0.0.1:9001"}]
        }
    }));
    engine.poll_outgoing_frames();
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"answer","data":{
            "callId":call_id,"params":{"status":0,"rtpSerIp":"127.0.0.1:9000"}
        }}
    }));
    let connected = engine.poll_outgoing_frames();
    assert_eq!(
        command(&connected, "sendSignal", 408)["data"]["callId"],
        call_id
    );
    assert_eq!(
        command(&connected, "sendSignal", 12006)["data"]["isOnCam"],
        1
    );
    assert!(engine
        .call
        .as_ref()
        .unwrap()
        .camera_state_reassert_at
        .is_some());

    {
        let call = engine.call.as_mut().unwrap();
        call.camera.on = true;
        call.camera_state_reassert_at = Some(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
        );
    }
    engine.tick();
    let reasserted = engine.poll_outgoing_frames();
    let camera = command(&reasserted, "sendSignal", 12006);
    assert_eq!(camera["data"]["callId"], call_id);
    assert_eq!(camera["data"]["isOnCam"], 1);
}

#[test]
fn incoming_video_answer_reports_camera_on_and_schedules_reassertion() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"request","data":{
            "callId":654_321,"uidN":"caller-video","uidFrom":222,"uidTo":111,
            "session":"012345678901234567890123456789",
            "rtpAddress":"127.0.0.1:9100","rtcpAddress":"127.0.0.1:9101",
            "params":{"Dname":"Caller","extendData":"{\"callType\":1,\"serverAddr\":[{\"rtpIPv6\":\"\",\"rtcpIPv6\":\"\"}]}"}
        }}
    }));
    engine.poll_outgoing_frames();
    engine.answer_call(false);
    let answered = engine.poll_outgoing_frames();
    assert_eq!(
        command(&answered, "sendSignal", 12006)["data"]["isOnCam"],
        1
    );
    assert!(engine.call.as_ref().unwrap().camera.on);
    assert!(engine
        .call
        .as_ref()
        .unwrap()
        .camera_state_reassert_at
        .is_some());
}

#[test]
fn incoming_call_answers_with_callers_relay_and_session() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"control","data":{
            "act":"request",
            "data":{
                "callId":123_456,"uidN":"caller-zalo","uidFrom":222,"uidTo":111,
                "session":"012345678901234567890123456789",
                "rtpAddress":"127.0.0.1:9100","rtcpAddress":"127.0.0.1:9101",
                "params":{
                    "Dname":"Caller","avatar":"caller-avatar",
                    "extendData":"{\"callType\":1,\"serverAddr\":[{\"rtpIPv6\":\"\",\"rtcpIPv6\":\"\"}]}",
                    "zrtc_config":{}
                }
            }
        }
    }));
    let incoming = engine.poll_outgoing_frames();
    assert_eq!(
        command(&incoming, "sendSignal", 407)["data"]["callId"],
        123_456
    );
    assert_eq!(engine.phase, CallPhase::Incoming);

    engine.answer_call(false);
    let answered = engine.poll_outgoing_frames();
    assert!(answered.iter().any(|frame| frame["type"] == "request"
        && frame["command"] == "action"
        && frame["data"]["id"] == 5));
    let response = command(&answered, "sendSignal", 402);
    assert_eq!(response["data"]["status"], 0);
    assert_eq!(response["data"]["rtpAddress"], "127.0.0.1:9100");
    assert_eq!(
        response["data"]["session"],
        "012345678901234567890123456789"
    );
    let extend: Value =
        serde_json::from_str(response["data"]["extendData"].as_str().unwrap()).unwrap();
    assert!(extend["p2p"].as_array().is_some_and(|candidates| {
        !candidates.is_empty()
            && candidates.iter().all(|candidate| {
                candidate["ip"].as_str().is_some()
                    && candidate["port"]
                        .as_u64()
                        .is_some_and(|port| port > 0 && port <= 65535)
            })
    }));
    assert_eq!(extend["sP2P"], 1);
    assert_eq!(extend["select2side"], 1);
    assert_eq!(engine.phase, CallPhase::InCall);

    engine.hangup("local");
    let ended = engine.poll_outgoing_frames();
    assert_eq!(
        command(&ended, "sendSignal", 409)["data"]["callId"],
        123_456
    );
    assert_eq!(command(&ended, "sendSignal", 406)["data"]["status"], 50);
    assert_eq!(engine.phase, CallPhase::Idle);
}

#[test]
fn screen_share_is_signalled_only_after_native_capture_is_ready() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"control","data":{
            "act":"request","data":{
                "callId":123_456,"uidN":"caller-zalo","uidFrom":222,"uidTo":111,
                "session":"012345678901234567890123456789",
                "rtpAddress":"127.0.0.1:9100","rtcpAddress":"127.0.0.1:9101",
                "params":{"Dname":"Caller","extendData":"{}", "zrtc_config":{}}
            }
        }
    }));
    engine.poll_outgoing_frames();
    engine.answer_call(false);
    engine.poll_outgoing_frames();
    {
        let call = engine.call.as_mut().unwrap();
        call.phase = CallPhase::InCall;
        call.kind = 3;
        call.camera.on = true;
        engine.phase = CallPhase::InCall;
    }

    assert!(!engine
        .poll_outgoing_frames()
        .iter()
        .any(|frame| frame["command"] == 12064));
    // The portal/PipeWire worker calls this only after the first encoded
    // H.264 access unit is ready, not merely after a raw capture buffer.
    engine.activate_screen_share();
    let frames = engine.poll_outgoing_frames();
    let share = command(&frames, "sendSignal", 12064);
    assert_eq!(share["data"]["callId"], 123_456);
    assert_eq!(share["data"]["uidTo"], "caller-zalo");
    assert_eq!(share["data"]["status"], 1);
    let camera_states = frames
        .iter()
        .filter(|frame| frame["type"] == "sendSignal" && frame["command"] == 12006)
        .map(|frame| frame["data"]["isOnCam"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(camera_states, vec![0, 1]);
    assert!(engine.call.as_ref().unwrap().camera.sharing_screen);
    assert!(engine.call.as_ref().unwrap().camera.was_on);
    assert!(!engine.call.as_ref().unwrap().camera.on);
}

#[test]
fn screen_share_keeps_peer_video_enabled_when_camera_was_off() {
    let mut engine = engine();
    engine.on_zalo_frame(&json!({
        "type":"control","data":{
            "act":"request","data":{
                "callId":123_456,"uidN":"caller-zalo","uidFrom":222,"uidTo":111,
                "session":"012345678901234567890123456789",
                "rtpAddress":"127.0.0.1:9100","rtcpAddress":"127.0.0.1:9101",
                "params":{"Dname":"Caller","extendData":"{}", "zrtc_config":{}}
            }
        }
    }));
    engine.poll_outgoing_frames();
    engine.answer_call(false);
    engine.poll_outgoing_frames();
    {
        let call = engine.call.as_mut().unwrap();
        call.phase = CallPhase::InCall;
        call.kind = 3;
        call.camera.on = false;
        engine.phase = CallPhase::InCall;
    }

    engine.activate_screen_share();
    let frames = engine.poll_outgoing_frames();
    assert_eq!(command(&frames, "sendSignal", 12006)["data"]["isOnCam"], 1);
    let call = engine.call.as_ref().unwrap();
    assert!(call.camera.sharing_screen);
    assert!(!call.camera.was_on);
    assert!(!call.camera.on);

    // The delayed connect reassertion must not overwrite the screen's
    // peer-visible video bit with the internal (camera capture) state.
    engine.call.as_mut().unwrap().camera_state_reassert_at = Some(
        Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap(),
    );
    engine.tick();
    let reasserted = engine.poll_outgoing_frames();
    assert_eq!(
        command(&reasserted, "sendSignal", 12006)["data"]["isOnCam"],
        1
    );
}

#[test]
fn incoming_call_rejects_missing_negative_and_overflow_call_ids() {
    for data in [
        json!({"uidN":"peer","params":{}}),
        json!({"callId":-1,"uidN":"peer","params":{}}),
        json!({"callId":4_294_967_296_u64,"uidN":"peer","params":{}}),
    ] {
        let mut engine = engine();
        engine.on_zalo_frame(&json!({"type":"control","data":{"act":"request","data":data}}));
        assert!(engine.call.is_none());
        assert_eq!(engine.phase, CallPhase::Idle);
    }
}

#[test]
fn incoming_one_to_one_call_does_not_replace_an_active_group_call() {
    let mut engine = group_engine();
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"id":"g-1","name":"Busy group"},
        "partner":[{"id":"222","name":"Member"}]
    }));
    let group_call_id = engine.group.as_ref().unwrap().call_id;
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"request","data":{
            "callId":987_654,"uidN":"caller","uidFrom":333,"uidTo":111,
            "session":"012345678901234567890123456789",
            "rtpAddress":"127.0.0.1:9100","rtcpAddress":"127.0.0.1:9101",
            "params":{"Dname":"Caller","extendData":"{}"}
        }}
    }));
    assert!(engine.call.is_none());
    assert_eq!(
        engine.group.as_ref().map(|group| group.call_id),
        Some(group_call_id)
    );
}

#[test]
fn group_single_invitee_is_rekeyed_to_sfu_uid_without_duplicate_tile() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Identity Test"},
        "partner":[{"id":"123456789012345678","name":"Alice","avatar":"alice.png"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }

    {
        let invitee = engine
            .group
            .as_mut()
            .unwrap()
            .members
            .get_mut("123456789012345678")
            .unwrap();
        invitee.state = "incall".into();
        invitee.muted = true;
        invitee.cam_off = true;
    }
    engine.group_note_zalo_member_join("123456789012345678");
    engine.group_roster(vec![111, 222]);
    engine.on_group_media_event(GroupMediaEvent::MemberVideo(222));

    let group = engine.group.as_ref().unwrap();
    assert!(!group.members.contains_key("123456789012345678"));
    assert_eq!(group.members.len(), 1);
    let member = &group.members["222"];
    assert_eq!(member.name, "Alice");
    assert_eq!(member.avatar, "alice.png");
    assert_eq!(member.state, "incall");
    assert!(member.muted);
    assert!(!member.cam_off);
    let ui = engine.group_members_ui();
    assert_eq!(ui.len(), 1);
    assert_eq!(ui[0]["src"], "222");
    assert_eq!(ui[0]["name"], "Alice");
}

#[test]
fn group_control_uid_rekeys_the_only_invitee_without_a_ghost_member() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Live Control Identity"},
        "partner":[{"id":"123456789012345678","name":"Alice","avatar":"alice.png"}]
    }));
    let call_id = engine.group.as_ref().unwrap().call_id;
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }

    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_ring_ring","data":{
            "id":call_id,"fromId":222
        }}
    }));
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_answer","data":{
            "id":call_id,"fromId":222
        }}
    }));
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_broadcast","data":{
            "id":call_id,"userId":222,"callState":3,"audioState":1,"videoState":0
        }}
    }));

    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members.len(), 1);
    assert!(!group.members.contains_key("123456789012345678"));
    assert_eq!(group.member_id_aliases["123456789012345678"], "222");
    let member = &group.members["222"];
    assert_eq!(member.name, "Alice");
    assert_eq!(member.avatar, "alice.png");
    assert_eq!(member.state, "incall");
    assert!(member.muted);
    assert!(!member.cam_off);
    let ui = engine.group_members_ui();
    assert_eq!(ui.len(), 1);
    assert_eq!(ui[0]["src"], "222");
}

#[test]
fn group_two_invitees_do_not_duplicate_uid_members_with_named_placeholders() {
    let mut engine = group_engine();
    engine.media = None;
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Two invitee identity"},
        "partner":[
            {"id":"111111111111111111","name":"Alice","avatar":"alice.png"},
            {"id":"222222222222222222","name":"Bob","avatar":"bob.png"}
        ]
    }));
    let call_id = engine.group.as_ref().unwrap().call_id;
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 999;
        group.host_call = 999;
    }

    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_ring_ring","data":{
            "id":call_id,"fromId":463_951_353
        }}
    }));
    let one_joined = engine.group_members_ui();
    assert_eq!(one_joined.len(), 2);
    assert!(one_joined.iter().any(|member| {
        member["src"] == "111111111111111111"
            && member["name"] == "Alice"
            && member["avatar"] == "alice.png"
    }));
    assert!(one_joined.iter().any(|member| {
        member["src"] == "222222222222222222"
            && member["name"] == "Bob"
            && member["avatar"] == "bob.png"
    }));
    assert!(one_joined.iter().all(|member| member["src"] != "463951353"));

    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_ring_ring","data":{
            "id":call_id,"fromId":319_623_368
        }}
    }));
    let two_joined = engine.group_members_ui();
    assert_eq!(two_joined.len(), 2);
    assert!(two_joined.iter().any(|member| {
        member["src"] == "111111111111111111"
            && member["name"] == "Alice"
            && member["avatar"] == "alice.png"
    }));
    assert!(two_joined.iter().any(|member| {
        member["src"] == "222222222222222222"
            && member["name"] == "Bob"
            && member["avatar"] == "bob.png"
    }));
    assert!(two_joined
        .iter()
        .all(|member| member["src"] != "463951353" && member["src"] != "319623368"));

    // Without an identity pair, keep the original invite metadata visible
    // even after one anonymous SFU UID departs.
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_end_call","data":{
            "id":call_id,"fromId":463_951_353
        }}
    }));
    let after_leave = engine.group_members_ui();
    assert_eq!(after_leave.len(), 2);
    assert!(after_leave.iter().any(|member| member["name"] == "Alice"));
    assert!(after_leave.iter().any(|member| member["name"] == "Bob"));
}

#[test]
fn group_video_ssrc_rekeys_invitee_after_early_control_broadcast() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Video Identity Test"},
        "partner":[{"id":"123456789012345678","name":"Alice","avatar":"alice.png"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_broadcast","data":{
            "id":engine.group.as_ref().unwrap().call_id,
            "userId":0,"fromId":"123456789012345678",
            "callState":3,"audioState":1,"videoState":0
        }}
    }));
    assert_eq!(
        engine.group.as_ref().unwrap().members["123456789012345678"].state,
        "incall"
    );
    assert!(!engine
        .group
        .as_ref()
        .unwrap()
        .members
        .contains_key("2788225870"));

    // A roster-only update has no camera/mic fields and must not erase the
    // state received under the full Zalo ID before the media UID appeared.
    engine.group_roster(vec![111, 222]);
    let mapped = &engine.group.as_ref().unwrap().members["222"];
    assert!(mapped.muted);
    assert!(!mapped.cam_off);

    engine.on_group_media_event(GroupMediaEvent::MemberVideo(222));

    let group = engine.group.as_ref().unwrap();
    assert!(!group.members.contains_key("123456789012345678"));
    assert_eq!(group.members.len(), 1);
    let member = &group.members["222"];
    assert_eq!(member.name, "Alice");
    assert_eq!(member.avatar, "alice.png");
    assert_eq!(member.state, "incall");
    assert!(member.muted);
    assert!(!member.cam_off);
    assert_eq!(engine.group_members_ui().len(), 1);
}

#[test]
fn group_sequential_invitees_pair_with_unique_media_sources_without_roster() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Sequential Media Identity Test"},
        "partner":[
            {"id":"zalo-a-long-id","name":"Alice","avatar":"alice.png"},
            {"id":"zalo-b-long-id","name":"Bob","avatar":"bob.png"}
        ]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }

    // First participant's audio arrives before their Zalo broadcast.
    engine.on_group_media_event(GroupMediaEvent::MemberAudio(222));
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_broadcast","data":{
            "id":engine.group.as_ref().unwrap().call_id,
            "userId":111,"fromId":"zalo-a-long-id",
            "callState":3,"audioState":1,"videoState":1
        }}
    }));
    assert_eq!(
        engine.group.as_ref().unwrap().member_id_aliases["zalo-a-long-id"],
        "222"
    );

    // The second participant's broadcast arrives first. The userId field
    // points at the local endpoint; fromId is the actual invited member.
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_broadcast","data":{
            "id":engine.group.as_ref().unwrap().call_id,
            "userId":111,"fromId":"zalo-b-long-id",
            "callState":3,"audioState":0,"videoState":1
        }}
    }));
    engine.on_group_media_event(GroupMediaEvent::MemberVideo(333));
    // Audio and video for the same participant share one identity key.
    engine.on_group_media_event(GroupMediaEvent::MemberAudio(333));

    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    assert_eq!(group.member_id_aliases["zalo-a-long-id"], "222");
    assert_eq!(group.member_id_aliases["zalo-b-long-id"], "333");
    assert_eq!(group.members["222"].name, "Alice");
    assert_eq!(group.members["333"].name, "Bob");
    assert!(!group.members.contains_key("zalo-a-long-id"));
    assert!(!group.members.contains_key("zalo-b-long-id"));
    assert_eq!(engine.group_members_ui().len(), 2);
}

#[test]
fn group_broadcast_explicitly_maps_each_long_zalo_id_to_its_sfu_user_id() {
    let mut engine = group_engine();
    engine.media = None;
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Explicit Identity Test"},
        "partner":[
            {"id":"zalo-a-long-id","name":"Alice"},
            {"id":"zalo-b-long-id","name":"Bob"}
        ]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }

    for (id, uid) in [("zalo-a-long-id", 222), ("zalo-b-long-id", 333)] {
        engine.on_zalo_frame(&json!({
            "type":"control","data":{"act":"group_broadcast","data":{
                "id":engine.group.as_ref().unwrap().call_id,
                "userId":uid,"fromId":id,
                "callState":3,"audioState":0,"videoState":1
            }}
        }));
    }

    // Media arrival after control must update the existing tiles, not add
    // anonymous duplicate participants.
    engine.on_group_media_event(GroupMediaEvent::MemberAudio(222));
    engine.on_group_media_event(GroupMediaEvent::MemberVideo(222));
    engine.on_group_media_event(GroupMediaEvent::MemberVideo(333));

    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.member_id_aliases["zalo-a-long-id"], "222");
    assert_eq!(group.member_id_aliases["zalo-b-long-id"], "333");
    assert_eq!(group.members.len(), 2);
    assert_eq!(group.members["222"].name, "Alice");
    assert_eq!(group.members["333"].name, "Bob");
    assert!(!group.members.contains_key("zalo-a-long-id"));
    assert!(!group.members.contains_key("zalo-b-long-id"));
    assert_eq!(engine.group_members_ui().len(), 2);
}

#[test]
fn group_multiple_invitees_are_rekeyed_from_explicit_broadcast_uids() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Multi Identity Test"},
        "partner":[
            {"id":"zalo-a-long-id","name":"Alice","avatar":"alice.png"},
            {"id":"zalo-b-long-id","name":"Bob","avatar":"bob.png"}
        ]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }
    for (id, audio_state, video_state) in [("zalo-a-long-id", 1, 0), ("zalo-b-long-id", 0, 1)] {
        engine.on_zalo_frame(&json!({
            "type":"control","data":{"act":"group_broadcast","data":{
                "id":engine.group.as_ref().unwrap().call_id,
                "userId":if id == "zalo-a-long-id" {222} else {333},
                "fromId":id,"callState":3,"audioState":audio_state,"videoState":video_state
            }}
        }));
    }

    engine.group_roster(vec![111, 222, 333]);

    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    assert!(!group.members.contains_key("zalo-a-long-id"));
    assert!(!group.members.contains_key("zalo-b-long-id"));
    assert_eq!(group.member_id_aliases["zalo-a-long-id"], "222");
    assert_eq!(group.member_id_aliases["zalo-b-long-id"], "333");
    assert_eq!(group.members["222"].name, "Alice");
    assert_eq!(group.members["222"].avatar, "alice.png");
    assert!(group.members["222"].muted);
    assert!(!group.members["222"].cam_off);
    assert_eq!(group.members["333"].name, "Bob");
    assert!(!group.members["333"].muted);
    assert!(group.members["333"].cam_off);
    assert_eq!(engine.group_members_ui().len(), 2);

    // Subsequent Zalo broadcasts keep targeting the SFU-backed member.
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_broadcast","data":{
            "id":group.call_id,"fromId":"zalo-b-long-id",
            "callState":3,"audioState":1,"videoState":0
        }}
    }));
    assert!(engine.group.as_ref().unwrap().members["333"].muted);
    assert!(!engine.group.as_ref().unwrap().members["333"].cam_off);
    assert!(!engine
        .group
        .as_ref()
        .unwrap()
        .members
        .contains_key("zalo-b-long-id"));
}

#[test]
fn group_roster_does_not_guess_multiple_invitee_identity_from_order() {
    let mut engine = group_engine();
    engine.media = None;
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Full Roster Identity Test"},
        "partner":[
            {"id":"zalo-a-long-id","name":"Alice"},
            {"id":"zalo-b-long-id","name":"Bob"},
            {"id":"zalo-c-long-id","name":"Carol"}
        ]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }

    engine.group_roster(vec![111, 222, 333, 444]);

    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members.len(), 6);
    assert!(group.member_id_aliases.is_empty());
    for id in ["zalo-a-long-id", "zalo-b-long-id", "zalo-c-long-id"] {
        assert!(group.members.contains_key(id));
    }
    for uid in ["222", "333", "444"] {
        assert_eq!(group.members[uid].state, "incall");
    }
    assert_eq!(engine.group_members_ui().len(), 3);
}

#[test]
fn multiple_sfu_media_sources_are_paired_when_zalo_broadcasts_arrive_later() {
    let mut engine = group_engine();
    engine.media = None;
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Reverse Multi Identity Test"},
        "partner":[
            {"id":"zalo-a-long-id","name":"Alice"},
            {"id":"zalo-b-long-id","name":"Bob"}
        ]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }
    for uid in [222, 333] {
        engine.group_member_update_from_media(
            &uid.to_string(),
            GroupMember {
                name: String::new(),
                avatar: String::new(),
                state: "incall".into(),
                muted: false,
                cam_off: true,
            },
        );
    }
    engine.group_roster(vec![111, 222, 333]);
    for (id, audio_state, video_state) in [("zalo-a-long-id", 1, 0), ("zalo-b-long-id", 0, 1)] {
        engine.on_zalo_frame(&json!({
            "type":"control","data":{"act":"group_broadcast","data":{
                "id":engine.group.as_ref().unwrap().call_id,
                "userId":if id == "zalo-a-long-id" {222} else {333},
                "fromId":id,"callState":3,"audioState":audio_state,"videoState":video_state
            }}
        }));
    }

    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    assert_eq!(group.member_id_aliases["zalo-a-long-id"], "222");
    assert_eq!(group.member_id_aliases["zalo-b-long-id"], "333");
    assert_eq!(group.members["222"].name, "Alice");
    assert_eq!(group.members["333"].name, "Bob");
    assert!(group.members["222"].muted);
    assert!(!group.members["222"].cam_off);
    assert!(!group.members["333"].muted);
    assert!(group.members["333"].cam_off);
    assert_eq!(engine.group_members_ui().len(), 2);
}

#[test]
fn group_roster_pairs_invitees_after_anonymous_media_tiles_already_exist() {
    let mut engine = group_engine();
    engine.media = None;
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Pre-Roster Media Test"},
        "partner":[
            {"id":"zalo-a-long-id","name":"Alice"},
            {"id":"zalo-b-long-id","name":"Bob"}
        ]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }

    // Media receives two SSRCs before ROOM_UPDATED and creates anonymous
    // member entries/tiles; the later complete roster must still re-key them.
    for uid in [222, 333] {
        engine.group_member_update_from_media(
            &uid.to_string(),
            GroupMember {
                name: String::new(),
                avatar: String::new(),
                state: "incall".into(),
                muted: false,
                cam_off: true,
            },
        );
    }
    assert_eq!(engine.group_members_ui().len(), 2); // one visible tile per observed participant

    for (id, uid) in [("zalo-a-long-id", 222), ("zalo-b-long-id", 333)] {
        engine.on_zalo_frame(&json!({
            "type":"control","data":{"act":"group_broadcast","data":{
                "id":engine.group.as_ref().unwrap().call_id,
                "userId":uid,"fromId":id,"callState":3,"audioState":0,"videoState":1
            }}
        }));
    }
    engine.group_roster(vec![111, 222, 333]);

    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members.len(), 2);
    assert_eq!(group.member_id_aliases["zalo-a-long-id"], "222");
    assert_eq!(group.member_id_aliases["zalo-b-long-id"], "333");
    assert_eq!(group.members["222"].name, "Alice");
    assert_eq!(group.members["333"].name, "Bob");
    assert_eq!(engine.group_members_ui().len(), 2);
}

#[test]
fn complete_roster_order_alone_does_not_assign_multiple_invitees() {
    let mut engine = group_engine();
    engine.media = None;
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Media Before Roster Test"},
        "partner":[
            {"id":"zalo-a-long-id","name":"Alice"},
            {"id":"zalo-b-long-id","name":"Bob"}
        ]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }
    // Media discovery order differs from the complete room roster order.
    for uid in [333, 222] {
        engine.group_member_update_from_media(
            &uid.to_string(),
            GroupMember {
                name: String::new(),
                avatar: String::new(),
                state: "incall".into(),
                muted: false,
                cam_off: false,
            },
        );
    }

    engine.group_roster(vec![111, 222, 333]);

    let group = engine.group.as_ref().unwrap();
    assert!(group.member_id_aliases.is_empty());
    assert_eq!(group.members.len(), 4);
    assert_eq!(engine.group_members_ui().len(), 2);
}

#[test]
fn group_sfu_uid_arriving_before_zalo_broadcast_is_paired_later() {
    let mut engine = group_engine();
    engine.media = None;
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Reverse Arrival Test"},
        "partner":[{"id":"zalo-a-long-id","name":"Alice"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
    }
    engine.group_member_update_from_media(
        "222",
        GroupMember {
            name: String::new(),
            avatar: String::new(),
            state: "incall".into(),
            muted: false,
            cam_off: true,
        },
    );
    engine.group_roster(vec![111, 222]);
    engine.on_zalo_frame(&json!({
        "type":"control","data":{"act":"group_broadcast","data":{
            "id":engine.group.as_ref().unwrap().call_id,
            "fromId":"zalo-a-long-id","callState":3,"audioState":1,"videoState":1
        }}
    }));
    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members.len(), 1);
    assert_eq!(group.member_id_aliases["zalo-a-long-id"], "222");
    assert_eq!(group.members["222"].name, "Alice");
    assert!(group.members["222"].muted);
    assert!(group.members["222"].cam_off);
}

#[test]
fn group_sfu_roster_adds_members_and_removes_departed_members() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Roster Test"},
        "partner":[{"id":"222","name":"Member 222"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
        group.started_at = Some(Instant::now());
        group.deadline = Some(Instant::now() + Duration::from_secs(30));
    }

    engine.group_roster(vec![111, 222, 333]);
    let group = engine.group.as_ref().unwrap();
    assert!(group.flags.answered);
    assert!(group.deadline.is_none());
    assert_eq!(group.members["222"].state, "incall");
    assert_eq!(group.members["333"].state, "incall");

    engine.group_roster(vec![111, 333]);
    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.members["222"].state, "left");
    assert_eq!(group.members["333"].state, "incall");

    engine.on_group_media_event(GroupMediaEvent::PeerEnd(2));
    assert!(engine.group.is_none());
    assert_eq!(engine.phase, CallPhase::Idle);
}

#[test]
fn group_member_join_reasserts_local_camera_state_after_delay() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "type":6,
        "groupInfo":{"name":"Camera Reassert Test"},
        "partner":[{"id":"222","name":"Member 222"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
        group.camera.on = true;
        group.deadline = None;
        group.next_ping_at = Some(Instant::now() + Duration::from_secs(60));
    }

    engine.group_member_update(
        "222",
        GroupMember {
            name: "Member 222".into(),
            avatar: String::new(),
            state: "incall".into(),
            muted: false,
            cam_off: false,
        },
    );
    let immediate = engine.poll_outgoing_frames();
    assert!(immediate
        .iter()
        .any(|event| { event["type"] == "sendSignal" && event["command"] == 12098 }));
    assert!(engine
        .group
        .as_ref()
        .unwrap()
        .camera_state_reassert_at
        .is_some());

    engine.group.as_mut().unwrap().camera_state_reassert_at = Some(
        Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap(),
    );
    engine.tick();
    let delayed = engine.poll_outgoing_frames();
    assert!(delayed.iter().any(|event| {
        event["type"] == "sendSignal"
            && event["command"] == 12098
            && event["data"]["videoState"] == 0
    }));
    assert!(engine
        .group
        .as_ref()
        .unwrap()
        .camera_state_reassert_at
        .is_none());
}

#[test]
fn first_group_audio_from_member_marks_them_in_call() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Audio Member Test"},
        "partner":[{"id":"222","name":"Member 222"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
        group.members.get_mut("222").unwrap().state = "ringing".into();
    }

    engine.on_group_media_event(GroupMediaEvent::MemberAudio(222));
    let member = &engine.group.as_ref().unwrap().members["222"];
    assert_eq!(member.state, "incall");
    assert_eq!(member.name, "Member 222");
}

#[test]
fn zalo_device_config_maps_auto_and_explicit_audio_devices() {
    let preferences = crate::devices::DevicePrefs {
        mic: "stored-mic".into(),
        speaker: "stored-speaker".into(),
        ..Default::default()
    };
    assert_eq!(
        zalo_audio_device_selection(
            &json!({
                "autoAudioInput":false,
                "autoAudioOutput":false,
                "defaultAudInputDevice":{"id":"usb-mic"},
                "defaultAudOutputDevice":{"id":"usb-speaker"}
            }),
            &preferences
        ),
        ("usb-mic".into(), "usb-speaker".into())
    );
    assert_eq!(
        zalo_audio_device_selection(
            &json!({"autoAudioInput":true,"autoAudioOutput":true}),
            &preferences
        ),
        (String::new(), String::new())
    );
    assert_eq!(
        zalo_audio_device_selection(&json!({}), &preferences),
        ("stored-mic".into(), "stored-speaker".into())
    );
}

#[test]
fn group_screen_share_uses_12044_to_allocate_and_release_share_uid() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Share Test"},
        "partner":[{"id":"222","name":"Member 222"},{"id":"777","name":"Synthetic Share"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
        group.call_id = 1234;
        group.camera.on = true;
        group.members.get_mut("777").unwrap().state = "incall".into();
    }

    engine.activate_screen_share();
    assert!(engine.group.as_ref().unwrap().flags.sharing_screen);
    assert!(engine.group.as_ref().unwrap().camera.was_on);
    assert!(engine.group.as_ref().unwrap().camera.on);
    let initial = engine.poll_outgoing_frames();
    assert!(initial.iter().any(|event| {
        event["type"] == "sendSignal"
            && event["command"] == 12044
            && event["data"]["status"] == 1
            && event["data"]["data"]
                .as_str()
                .is_some_and(|value| value.contains("\"shareScreenId\":0"))
    }));

    engine.group_share_response(&json!({
        "data":json_cpp(&json!({"shareScreenId":777}))
    }));
    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.share_screen_id, 777);
    assert!(!group.members.contains_key("777"));
    assert!(!engine
        .group_members_ui()
        .iter()
        .any(|member| member["src"] == "777"));
    let allocated = engine.poll_outgoing_frames();
    assert!(allocated.iter().any(|event| {
        event["type"] == "sendSignal"
            && event["command"] == 12044
            && event["data"]["status"] == 1
            && event["data"]["data"]
                .as_str()
                .is_some_and(|value| value.contains("777"))
    }));

    engine.stop_group_screen_share();
    assert!(!engine.group.as_ref().unwrap().flags.sharing_screen);
    assert!(engine.group.as_ref().unwrap().camera.on);
    assert!(!engine.group.as_ref().unwrap().camera.was_on);
    assert!(!engine.group.as_ref().unwrap().members.contains_key("777"));
    let stopped = engine.poll_outgoing_frames();
    assert!(stopped.iter().any(|event| {
        event["type"] == "sendSignal" && event["command"] == 12044 && event["data"]["status"] == 0
    }));
}

#[test]
fn group_screen_share_restores_an_initially_off_camera_bit() {
    let mut engine = group_engine();
    engine.media = None;
    engine.on_zalo_frame(&json!({
        "type":"update","command":"init","data":{"local":{"id":111}}
    }));
    engine.group_outgoing(&json!({
        "groupInfo":{"name":"Share Test"},
        "partner":[{"id":"222","name":"Member 222"}]
    }));
    {
        let group = engine.group.as_mut().unwrap();
        group.phase = GroupPhase::Connected;
        group.local_uid = 111;
        group.host_call = 111;
        group.call_id = 1234;
        group.camera.on = false;
    }

    engine.activate_screen_share();
    assert!(engine.group.as_ref().unwrap().camera.on);
    assert!(!engine.group.as_ref().unwrap().camera.was_on);
    engine.stop_group_screen_share();
    assert!(!engine.group.as_ref().unwrap().camera.on);
    assert!(!engine.group.as_ref().unwrap().camera.was_on);
}

#[test]
fn outgoing_group_video_call_preserves_camera_intent() {
    let mut engine = group_engine();
    engine.group_outgoing(&json!({
        "type":6,
        "groupInfo":{"name":"Video Group"},
        "partner":[{"id":"222","name":"Member"}]
    }));
    let group = engine.group.as_ref().unwrap();
    assert_eq!(group.call_type, 1);
    assert!(group.camera.on);
}

#[test]
fn outgoing_group_call_negotiates_sfu_and_sends_group_join_signal() {
    let mut engine = group_engine();
    engine.on_zalo_frame(&json!({
        "type":"request","command":"makeCall",
        "data":{
            "type":5,
            "groupInfo":{"id":"group-9","name":"Group Nine","avatar":"group.png","maxUsers":8},
            "partner":[{"id":"zalo-2","name":"Member Two","avatar":"member.png"}]
        }
    }));
    let initial = engine.poll_outgoing_frames();
    let request = command(&initial, "sendSignal", 12433);
    assert_eq!(request["data"]["groupId"], "group-9");
    assert_eq!(request["data"]["typeRequest"], 1);
    let request_payload: Value =
        serde_json::from_str(request["data"]["data"].as_str().unwrap()).unwrap();
    assert_eq!(request_payload["groupName"], "Group Nine");
    assert_eq!(request_payload["noiseId"][0], "zalo-2");
    assert_eq!(engine.group.as_ref().unwrap().phase, GroupPhase::Calling);

    engine.on_zalo_frame(&json!({
        "type":"recvSignal","command":12433,
        "data":{
            "callId":123_456,
            "hostCall":111,
            "data":{
                "status":0,
                "callId":123_456,
                "groupId":789,
                "interval":10,
                "callSetting":{
                    "session":"012345678901234567890123456789",
                    "servers":[{
                        "rtpaddr":"127.0.0.1:9100",
                        "rtcpaddr":"127.0.0.1:9101"
                    }],
                    "zrtcConfig":{}
                }
            }
        }
    }));
    let joined = engine.poll_outgoing_frames();
    let join = command(&joined, "sendSignal", 12434);
    assert_eq!(join["data"]["callId"], 123_456);
    assert_eq!(join["data"]["groupId"], "789");
    assert_eq!(join["data"]["session"], "012345678901234567890123456789");
    let join_payload: Value = serde_json::from_str(join["data"]["data"].as_str().unwrap()).unwrap();
    assert_eq!(join_payload["rtpAddress"], "127.0.0.1:9100");
    let nested: Value = serde_json::from_str(join_payload["data"].as_str().unwrap()).unwrap();
    assert_eq!(nested["hostCall"], 111);
    assert_eq!(nested["noiseId"][0], "zalo-2");
    assert!(joined
        .iter()
        .any(|frame| frame["type"] == "sendSignal" && frame["command"] == 12098));
    assert_eq!(engine.group.as_ref().unwrap().phase, GroupPhase::Connected);
    assert!(!engine.group.as_ref().unwrap().camera.on);

    engine.group.as_mut().unwrap().next_ping_at =
        Some(Instant::now().checked_sub(Duration::from_secs(1)).unwrap());
    engine.tick();
    let ping = engine.poll_outgoing_frames();
    assert_eq!(
        command(&ping, "sendSignal", 12097)["data"]["callId"],
        123_456
    );

    engine.on_zalo_frame(&json!({
        "type":"control","data":{
            "act":"group_broadcast",
            "data":{"id":123_456,"fromId":222,"callState":3,"audioState":1,"videoState":1}
        }
    }));
    let group = engine.group.as_ref().unwrap();
    assert!(group.flags.answered);
    assert!(group.deadline.is_none());
    assert!(group.members["222"].muted);

    engine.group_hangup("local");
    let ended = engine.poll_outgoing_frames();
    assert_eq!(
        command(&ended, "sendSignal", 12438)["data"]["callId"],
        123_456
    );
    assert_eq!(command(&ended, "sendSignal", 12446)["data"]["status"], 50);
    assert_eq!(engine.phase, CallPhase::Idle);
    assert!(engine.group.is_none());
}

#[test]
fn incoming_group_call_can_be_accepted_or_rejected_through_ui_actions() {
    let mut engine = group_engine();
    engine.on_zalo_frame(&json!({
        "type":"control","data":{
            "act":"group_request",
            "data":{
                "id":654_321,
                "hostCall":222,
                "receiverId":111,
                "groupId":987,
                "callType":1,
                "groupName":"Incoming Group",
                "groupAvatar":"incoming.png",
                "interval":10,
                "callSetting":{
                    "session":"012345678901234567890123456789",
                    "servers":[{
                        "rtpaddr":"127.0.0.1:9200",
                        "rtcpaddr":"127.0.0.1:9201"
                    }],
                    "zrtcConfig":{}
                },
                "partnerInfo":[{"userId":222,"name":"Host","avatar":"host.png","callState":3}]
            }
        }
    }));
    let incoming = engine.poll_outgoing_frames();
    assert_eq!(command(&incoming, "sendSignal", 12439)["data"]["status"], 1);
    assert_eq!(engine.group.as_ref().unwrap().phase, GroupPhase::Incoming);
    assert_eq!(engine.group.as_ref().unwrap().members["222"].name, "Host");

    let accept: UiAction = serde_json::from_value(json!({
        "action":"accept","camStartOff":true
    }))
    .unwrap();
    engine.on_ui_action(&accept);
    let accepted = engine.poll_outgoing_frames();
    assert_eq!(command(&accepted, "sendSignal", 12436)["data"]["status"], 0);
    assert!(accepted
        .iter()
        .any(|frame| frame["type"] == "sendSignal" && frame["command"] == 12098));
    assert_eq!(engine.group.as_ref().unwrap().phase, GroupPhase::Connected);
    assert!(!engine.group.as_ref().unwrap().camera.on);

    engine.group_hangup("local");
    let ended = engine.poll_outgoing_frames();
    assert_eq!(
        command(&ended, "sendSignal", 12438)["data"]["callId"],
        654_321
    );

    let mut rejected = group_engine();
    rejected.on_zalo_frame(&json!({
        "type":"control","data":{
            "act":"group_request",
            "data":{
                "id":654_322,"hostCall":222,"receiverId":111,"groupId":987,
                "callSetting":{
                    "session":"012345678901234567890123456789",
                    "servers":[{"rtpaddr":"127.0.0.1:9200","rtcpaddr":"127.0.0.1:9201"}],
                    "zrtcConfig":{}
                }
            }
        }
    }));
    rejected.poll_outgoing_frames();
    let reject: UiAction = serde_json::from_value(json!({"action":"reject"})).unwrap();
    rejected.on_ui_action(&reject);
    let declined = rejected.poll_outgoing_frames();
    assert_eq!(command(&declined, "sendSignal", 12436)["data"]["status"], 3);
    assert!(rejected.group.is_none());
}

#[test]
fn incoming_group_call_rejects_missing_negative_and_overflow_ids() {
    for id in [None, Some(-1_i64), Some(4_294_967_296_i64)] {
        let mut engine = group_engine();
        let mut data = json!({
            "type":"control",
            "data":{"act":"group_request","data":{
                "hostCall":222,"receiverId":111,"groupId":987,
                "callSetting":{"session":"session","servers":[],"zrtcConfig":{}}
            }}
        });
        if let Some(id) = id {
            data["data"]["data"]["id"] = json!(id);
        }
        engine.on_zalo_frame(&data);
        assert!(engine.group.is_none());
        assert_eq!(engine.poll_outgoing_frames(), Vec::<Value>::new());
    }
}
