//! Existing Zalo-Linux call-window TCP protocol adapter.
//!
//! The frontend opens a loopback listener and exports `ZCALL_UI_PORT` and
//! `ZCALL_UI_TOKEN`. The helper authenticates with the token, then exchanges
//! JSON-lines control messages and length-prefixed binary H.264 frames.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError,
};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::ui::{UiAction, UiTransport};
use serde_json::{json, Value};

const MAX_CONTROL_LINE: usize = 8 * 1024 * 1024;
const MAX_VIDEO_FRAME: usize = 8 * 1024 * 1024;
const OUTBOUND_QUEUE: usize = 16;

enum OutboundMessage {
    Json(Value),
    Binary(Value, Vec<u8>),
}

pub struct TcpUiTransport {
    outbound: SyncSender<OutboundMessage>,
    action_rx: Mutex<Receiver<UiAction>>,
    last_state: Arc<Mutex<Option<Value>>>,
}

impl TcpUiTransport {
    /// Connect using the environment exported by plugins/zcall/window.js.
    #[must_use]
    pub fn connect_from_env() -> Option<Self> {
        let port = match std::env::var("ZCALL_UI_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
        {
            Some(port) if port != 0 => port,
            _ => {
                crate::zcall_log!("[zcall-rust] ZCALL_UI_PORT is missing; the frontend listener was not ready before helper startup");
                return None;
            }
        };
        let token = match std::env::var("ZCALL_UI_TOKEN") {
            Ok(token) if !token.is_empty() => token,
            _ => {
                crate::zcall_log!(
                    "[zcall-rust] ZCALL_UI_TOKEN is missing; refusing unauthenticated UI transport"
                );
                return None;
            }
        };
        match Self::connect_retrying(port, &token) {
            Ok(transport) => Some(transport),
            Err(error) => {
                crate::zcall_log!("[zcall-rust] call UI reconnect worker failed: {error}");
                None
            }
        }
    }

    /// Keep the UI channel alive if Zalo starts the helper before the plugin's
    /// ephemeral loopback listener has finished binding, or if the renderer
    /// reconnects after a temporary socket failure.
    fn connect_retrying(port: u16, token: &str) -> io::Result<Self> {
        let (outbound, outbound_rx) = sync_channel(OUTBOUND_QUEUE);
        let (incoming_tx, action_rx) = sync_channel(OUTBOUND_QUEUE);
        let last_state = Arc::new(Mutex::new(None));
        let manager_state = last_state.clone();
        let token = token.to_owned();
        thread::Builder::new()
            .name("zcall-ui-connection".into())
            .spawn(move || {
                reconnect_loop(port, &token, &outbound_rx, &incoming_tx, &manager_state);
            })?;
        Ok(Self {
            outbound,
            action_rx: Mutex::new(action_rx),
            last_state,
        })
    }

    #[cfg(test)]
    fn connect_with(port: u16, token: &str) -> io::Result<Self> {
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let mut stream = TcpStream::connect_timeout(&address.into(), Duration::from_secs(3))?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        stream.write_all(token.as_bytes())?;
        stream.write_all(b"\n")?;

        let reader_stream = stream.try_clone()?;
        let (action_tx, action_rx) = sync_channel(OUTBOUND_QUEUE);
        thread::Builder::new()
            .name("zcall-ui-writer".into())
            .spawn(move || writer_loop(stream, &action_rx))?;

        let (incoming_tx, incoming_rx) = std::sync::mpsc::channel();
        thread::Builder::new()
            .name("zcall-ui-reader".into())
            .spawn(move || {
                let mut reader = BufReader::new(reader_stream);
                let mut line = Vec::new();
                loop {
                    line.clear();
                    match reader.read_until(b'\n', &mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(size) if size > MAX_CONTROL_LINE => {}
                        Ok(_) => {
                            if let Ok(action) = serde_json::from_slice::<UiAction>(&line) {
                                if incoming_tx.send(action).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                }
            })?;

        Ok(Self {
            outbound: action_tx,
            action_rx: Mutex::new(incoming_rx),
            last_state: Arc::new(Mutex::new(None)),
        })
    }
}

fn connect_ui_stream(
    address: SocketAddrV4,
    port: u16,
    token: &str,
    incoming: &SyncSender<UiAction>,
    logged_connect_failure: &mut bool,
) -> Option<(TcpStream, Arc<AtomicBool>)> {
    let mut stream = match TcpStream::connect_timeout(&address.into(), Duration::from_millis(500)) {
        Ok(stream) => stream,
        Err(error) => {
            if !*logged_connect_failure {
                crate::zcall_log!(
                    "[zcall-rust] waiting for call UI listener on 127.0.0.1:{port}: {error}"
                );
                *logged_connect_failure = true;
            }
            thread::sleep(Duration::from_millis(200));
            return None;
        }
    };

    if let Err(error) = stream
        .set_nodelay(true)
        .and_then(|()| stream.set_write_timeout(Some(Duration::from_millis(500))))
        .and_then(|()| stream.write_all(token.as_bytes()))
        .and_then(|()| stream.write_all(b"\n"))
    {
        crate::zcall_log!("[zcall-rust] call UI authentication write failed: {error}");
        let _ = stream.shutdown(std::net::Shutdown::Both);
        thread::sleep(Duration::from_millis(200));
        return None;
    }

    let reader_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(error) => {
            crate::zcall_log!("[zcall-rust] call UI reader clone failed: {error}");
            let _ = stream.shutdown(std::net::Shutdown::Both);
            thread::sleep(Duration::from_millis(200));
            return None;
        }
    };
    let disconnected = Arc::new(AtomicBool::new(false));
    let reader_disconnected = disconnected.clone();
    let reader_incoming = incoming.clone();
    let reader = thread::Builder::new()
        .name("zcall-ui-reconnect-reader".into())
        .spawn(move || {
            reader_loop(reader_stream, &reader_incoming);
            reader_disconnected.store(true, Ordering::Release);
        });
    if let Err(error) = reader {
        crate::zcall_log!("[zcall-rust] call UI reader start failed: {error}");
        let _ = stream.shutdown(std::net::Shutdown::Both);
        thread::sleep(Duration::from_millis(200));
        return None;
    }
    *logged_connect_failure = false;
    Some((stream, disconnected))
}

fn reconnect_loop(
    port: u16,
    token: &str,
    receiver: &Receiver<OutboundMessage>,
    incoming: &SyncSender<UiAction>,
    last_state: &Arc<Mutex<Option<Value>>>,
) {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    let mut pending = VecDeque::new();
    let mut logged_connect_failure = false;

    loop {
        match receiver.try_recv() {
            Ok(OutboundMessage::Json(value)) => {
                enqueue_pending(&mut pending, OutboundMessage::Json(value));
            }
            Ok(OutboundMessage::Binary(_, _)) | Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => return,
        }
        while let Ok(message) = receiver.try_recv() {
            if let OutboundMessage::Json(value) = message {
                enqueue_pending(&mut pending, OutboundMessage::Json(value));
            }
        }

        let Some((mut stream, disconnected)) =
            connect_ui_stream(address, port, token, incoming, &mut logged_connect_failure)
        else {
            continue;
        };

        if let Some(state) = last_state.lock().ok().and_then(|state| state.clone()) {
            enqueue_pending(&mut pending, OutboundMessage::Json(state));
        }
        crate::zcall_log!("[zcall-rust] call UI connected on 127.0.0.1:{port}");
        let mut write_failed = false;
        while !disconnected.load(Ordering::Acquire) {
            while let Ok(message) = receiver.try_recv() {
                enqueue_pending(&mut pending, message);
            }

            if let Some(message) = pending.pop_front() {
                let result = match &message {
                    OutboundMessage::Json(value) => write_json_line(&mut stream, value),
                    OutboundMessage::Binary(header, data) => {
                        write_json_line(&mut stream, header).and_then(|()| stream.write_all(data))
                    }
                };
                if let Err(error) = result {
                    crate::zcall_log!("[zcall-rust] call UI socket write failed: {error}");
                    if matches!(message, OutboundMessage::Json(_)) {
                        pending.push_front(message);
                    }
                    write_failed = true;
                    break;
                }
                continue;
            }

            match receiver.recv_timeout(Duration::from_millis(20)) {
                Ok(message) => enqueue_pending(&mut pending, message),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    return;
                }
            }
        }

        let _ = stream.shutdown(std::net::Shutdown::Both);
        // A dropped connection invalidates preview frames. Keep only the most
        // recent state/close event so a reconnected renderer cannot replay an
        // old call window state or an obsolete video frame.
        pending.retain(|message| matches!(message, OutboundMessage::Json(_)));
        if write_failed {
            thread::sleep(Duration::from_millis(200));
        }
    }
}

fn enqueue_pending(pending: &mut VecDeque<OutboundMessage>, message: OutboundMessage) {
    match message {
        OutboundMessage::Binary(header, data) => {
            if pending.len() < 4 {
                pending.push_back(OutboundMessage::Binary(header, data));
            }
        }
        OutboundMessage::Json(value) => {
            let kind = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if kind == "state" {
                pending.retain(|queued| {
                    !matches!(queued, OutboundMessage::Json(value)
                        if matches!(value.get("type").and_then(Value::as_str), Some("state" | "close")))
                });
            } else if kind == "close" {
                pending.retain(|queued| {
                    !matches!(queued, OutboundMessage::Json(value)
                        if matches!(value.get("type").and_then(Value::as_str), Some("state" | "close")))
                });
            }
            if pending.len() < 64 {
                pending.push_back(OutboundMessage::Json(value));
            }
        }
    }
}

fn reader_loop(stream: TcpStream, incoming: &SyncSender<UiAction>) {
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(size) if size > MAX_CONTROL_LINE => {}
            Ok(_) => {
                if let Ok(action) = serde_json::from_slice::<UiAction>(&line) {
                    if incoming.send(action).is_err() {
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
fn writer_loop(mut stream: TcpStream, receiver: &Receiver<OutboundMessage>) {
    while let Ok(message) = receiver.recv() {
        let result = match message {
            OutboundMessage::Json(value) => write_json_line(&mut stream, &value),
            OutboundMessage::Binary(header, data) => {
                write_json_line(&mut stream, &header).and_then(|()| stream.write_all(&data))
            }
        };
        if let Err(error) = result {
            crate::zcall_log!("[zcall-rust] call UI socket write failed: {error}");
            break;
        }
    }
}

fn write_json_line(stream: &mut TcpStream, message: &Value) -> io::Result<()> {
    serde_json::to_writer(&mut *stream, message).map_err(io::Error::other)?;
    stream.write_all(b"\n")
}

impl UiTransport for TcpUiTransport {
    fn try_recv_action(&self) -> Option<UiAction> {
        self.action_rx.lock().ok()?.try_recv().ok()
    }

    fn send_message(&self, message: &Value) {
        if let Ok(mut last_state) = self.last_state.lock() {
            match message.get("type").and_then(Value::as_str) {
                Some("state") => *last_state = Some(message.clone()),
                Some("close") => *last_state = None,
                _ => {}
            }
        }
        let _ = self.outbound.send(OutboundMessage::Json(message.clone()));
    }

    fn send_video_frame(&self, key: bool, codec: &str, data: &[u8], src: Option<u32>) {
        self.queue_video_frame(
            json!({
                "type":"video", "key":key, "codec":codec, "src":src, "bin":data.len()
            }),
            data,
        );
    }

    fn send_local_video_frame(&self, key: bool, codec: &str, data: &[u8]) {
        self.queue_video_frame(
            json!({
                "type":"localVideo", "key":key, "codec":codec, "bin":data.len()
            }),
            data,
        );
    }
}

impl TcpUiTransport {
    /// Preview is best-effort: never stall call signaling/media when the UI
    /// is temporarily behind; a full queue drops this preview frame only.
    fn queue_video_frame(&self, header: Value, data: &[u8]) {
        if data.is_empty() || data.len() > MAX_VIDEO_FRAME {
            return;
        }
        match self
            .outbound
            .try_send(OutboundMessage::Binary(header, data.to_vec()))
        {
            Ok(()) | Err(TrySendError::Disconnected(_) | TrySendError::Full(_)) => {}
        }
    }
}

#[cfg(test)]
#[path = "../tests/ui/tcp.rs"]
mod tests;
