use super::*;
use std::io::Read;
use std::net::TcpListener;
use std::thread;

#[test]
fn authenticates_and_exchanges_the_existing_json_line_protocol() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut token = String::new();
        reader.read_line(&mut token).unwrap();
        assert_eq!(token.trim(), "test-token");
        stream
            .write_all(b"{\"action\":\"mute\",\"on\":true}\n")
            .unwrap();
        let mut action = String::new();
        reader.read_line(&mut action).unwrap();
        serde_json::from_str::<Value>(&action).unwrap()
    });

    let transport = TcpUiTransport::connect_with(port, "test-token").unwrap();
    let action = (0..100)
        .find_map(|_| {
            let action = transport.try_recv_action();
            if action.is_none() {
                thread::sleep(Duration::from_millis(5));
            }
            action
        })
        .expect("frontend action should arrive");
    assert_eq!(action.action, "mute");
    assert_eq!(action.on, Some(true));
    transport.send_message(&json!({"type":"state","phase":"connected"}));
    let sent = server.join().unwrap();
    assert_eq!(sent["type"], "state");
    assert_eq!(sent["phase"], "connected");
}

#[test]
fn reconnects_when_the_frontend_listener_starts_after_the_helper() {
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = reservation.local_addr().unwrap().port();
    drop(reservation);

    let transport = TcpUiTransport::connect_retrying(port, "retry-token").unwrap();
    transport.send_message(&json!({"type":"state","phase":"connected","video":true}));

    thread::sleep(Duration::from_millis(150));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut token = String::new();
        reader.read_line(&mut token).unwrap();
        assert_eq!(token.trim(), "retry-token");
        let mut state = String::new();
        reader.read_line(&mut state).unwrap();
        let state: Value = serde_json::from_str(&state).unwrap();
        stream
            .write_all(b"{\"action\":\"mute\",\"on\":true}\n")
            .unwrap();
        state
    });

    let state = server.join().unwrap();
    assert_eq!(state["phase"], "connected");
    let action = (0..100)
        .find_map(|_| {
            let action = transport.try_recv_action();
            if action.is_none() {
                thread::sleep(Duration::from_millis(5));
            }
            action
        })
        .expect("action from reconnected frontend should arrive");
    assert_eq!(action.action, "mute");
    assert_eq!(action.on, Some(true));
}

#[test]
fn replays_the_latest_call_state_after_the_ui_socket_reconnects() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (closed_tx, closed_rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (first, _) = listener.accept().unwrap();
        let mut first_reader = BufReader::new(first.try_clone().unwrap());
        let mut token = String::new();
        first_reader.read_line(&mut token).unwrap();
        let mut first_state = String::new();
        first_reader.read_line(&mut first_state).unwrap();
        drop(first_reader);
        drop(first);
        closed_tx.send(()).unwrap();

        let (second, _) = listener.accept().unwrap();
        let mut second_reader = BufReader::new(second.try_clone().unwrap());
        let mut second_token = String::new();
        second_reader.read_line(&mut second_token).unwrap();
        let mut replayed = String::new();
        second_reader.read_line(&mut replayed).unwrap();
        (
            serde_json::from_str::<Value>(&first_state).unwrap(),
            serde_json::from_str::<Value>(&replayed).unwrap(),
            token.trim().to_owned(),
            second_token.trim().to_owned(),
        )
    });

    let transport = TcpUiTransport::connect_retrying(port, "replay-token").unwrap();
    transport.send_message(&json!({"type":"state","phase":"outgoing"}));
    closed_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    transport.send_message(&json!({"type":"state","phase":"connected","sharing":true}));
    let (first, replayed, first_token, second_token) = server.join().unwrap();
    assert_eq!(first["phase"], "outgoing");
    assert_eq!(replayed["phase"], "connected");
    assert_eq!(replayed["sharing"], true);
    assert_eq!(first_token, "replay-token");
    assert_eq!(second_token, "replay-token");
}

#[test]
fn writes_binary_video_using_the_frontend_length_header() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut token = String::new();
        reader.read_line(&mut token).unwrap();
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header: Value = serde_json::from_str(&header).unwrap();
        let length = header["bin"].as_u64().unwrap() as usize;
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).unwrap();
        (header, payload)
    });

    let transport = TcpUiTransport::connect_with(port, "video-token").unwrap();
    transport.send_video_frame(true, "h264", &[0, 0, 1, 0x65, 0xaa], Some(7));
    let (header, payload) = server.join().unwrap();
    assert_eq!(header["type"], "video");
    assert_eq!(header["src"], 7);
    assert_eq!(header["bin"], payload.len());
    assert_eq!(payload, [0, 0, 1, 0x65, 0xaa]);
}

#[test]
fn writes_binary_local_preview_using_the_frontend_length_header() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut token = String::new();
        reader.read_line(&mut token).unwrap();
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header: Value = serde_json::from_str(&header).unwrap();
        let length = header["bin"].as_u64().unwrap() as usize;
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).unwrap();
        (header, payload)
    });

    let transport = TcpUiTransport::connect_with(port, "preview-token").unwrap();
    transport.send_local_video_frame(true, "avc1.42E01E", &[0, 0, 0, 1, 0x67, 0x42]);
    let (header, payload) = server.join().unwrap();
    assert_eq!(header["type"], "localVideo");
    assert_eq!(header["key"], true);
    assert_eq!(header["codec"], "avc1.42E01E");
    assert_eq!(header["bin"], payload.len());
    assert_eq!(payload, [0, 0, 0, 1, 0x67, 0x42]);
}
