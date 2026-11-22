//! Zalo call-v2 signaling state machine.
//!
//! The patched Zalo main process owns the two call-v2 Unix sockets; this module
//! owns protocol state and media lifecycle. Host communication is injected
//! through UiTransport.

use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::camera::CameraCapture;
use crate::media::{GroupMediaEvent, MediaSessionParams, ZrtcMediaBackend};
use crate::screen::ScreenCapture;
use crate::ui::{UiAction, UiTransport};

const CALL_SETUP_TIMEOUT: Duration = Duration::from_secs(15);
const RING_TIMEOUT: Duration = Duration::from_secs(45);
const CODEC: &str =
    "[{\"dynamicFptime\":0,\"frmPtime\":20,\"name\":\"opus/16000/1\",\"payload\":112}]\n";
const EMPTY_CODEC: &str = "[]\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    Idle,
    Outgoing,
    Ringing,
    Incoming,
    Connecting,
    InCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallRole {
    Caller,
    Callee,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupRole {
    Caller,
    Callee,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupPhase {
    Calling,
    Incoming,
    Connecting,
    Connected,
    Failed,
    Ended,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
struct GroupMember {
    name: String,
    avatar: String,
    state: String,
    muted: bool,
    cam_off: bool,
}

#[allow(dead_code)]
struct GroupCall {
    role: GroupRole,
    phase: GroupPhase,
    call_id: u32,
    host_call: u32,
    group_id: u32,
    group_id_str: String,
    local_uid: u32,
    call_type: u8,
    sess_id: String,
    servers: Vec<Value>,
    config: Value,
    title: String,
    avatar: String,
    max_users: u32,
    invited: Vec<String>,
    members: HashMap<String, GroupMember>,
    member_id_aliases: HashMap<String, String>,
    pending_member_ids: VecDeque<String>,
    pending_sfu_uids: VecDeque<u32>,
    observed_sfu_uids: HashSet<u32>,
    deadline: Option<Instant>,
    started_at: Option<Instant>,
    next_ping_at: Option<Instant>,
    camera_state_reassert_at: Option<Instant>,
    ping_interval: Duration,
    muted: bool,
    speaker_off: bool,
    camera_on: bool,
    camera_was_on: bool,
    answered: bool,
    share_screen_id: u32,
    sharing_screen: bool,
}

#[allow(dead_code)]
impl GroupCall {
    fn connected(&self) -> bool {
        self.phase == GroupPhase::Connected
    }
}

#[derive(Debug)]
struct Call {
    role: CallRole,
    phase: CallPhase,
    call_id: u32,
    peer_id: String,
    peer_uid: u32,
    local_uid: u32,
    peer_name: String,
    avatar: String,
    call_type: u8,
    session_id: String,
    server: Option<Value>,
    servers: Vec<Value>,
    p2p_candidates: Vec<SocketAddr>,
    media_config: Value,
    deadline: Option<Instant>,
    started_at: Option<Instant>,
    muted: bool,
    speaker_off: bool,
    camera_on: bool,
    camera_was_on: bool,
    camera_state_reassert_at: Option<Instant>,
    peer_muted: bool,
    peer_camera_off: bool,
    peer_sharing: bool,
    sharing_screen: bool,
}

pub struct EngineCore {
    phase: CallPhase,
    send_udp: bool,
    group_enabled: bool,
    local: Value,
    call: Option<Call>,
    group: Option<GroupCall>,
    media: Option<ZrtcMediaBackend>,
    camera: Option<CameraCapture>,
    screen: Option<ScreenCapture>,
    screen_restart_pending: bool,
    screen_restart_source: Option<String>,
    screen_request_id: Option<String>,
    screen_restart_request_id: Option<String>,
    screen_stop_ack_pending: bool,
    ui: Option<Box<dyn UiTransport>>,
    outgoing_frames: Vec<Value>,
    last_ended_call: Option<u32>,
    group_notice_until: Option<Instant>,
}

impl EngineCore {
    pub fn new(send_udp: bool, group_enabled: bool, ui: Option<Box<dyn UiTransport>>) -> Self {
        let media = match ZrtcMediaBackend::new() {
            Ok(value) => {
                let prefs = crate::devices::load_prefs();
                if !prefs.mic.is_empty() {
                    value.set_device("mic", &prefs.mic);
                }
                if !prefs.speaker.is_empty() {
                    value.set_device("speaker", &prefs.speaker);
                }
                Some(value)
            }
            Err(err) => {
                crate::zcall_log!("[zcall-rust] media socket unavailable: {err}");
                None
            }
        };
        Self {
            phase: CallPhase::Idle,
            send_udp,
            group_enabled,
            local: json!({}),
            call: None,
            group: None,
            media,
            camera: None,
            screen: None,
            screen_restart_pending: false,
            screen_restart_source: None,
            screen_request_id: None,
            screen_restart_request_id: None,
            screen_stop_ack_pending: false,
            ui,
            outgoing_frames: Vec::new(),
            last_ended_call: None,
            group_notice_until: None,
        }
    }

    pub fn start(&mut self) {
        self.emit(json!({"type":"update","command":"native-ready","data":{}}));
        self.log("engine ready");
    }

    pub fn poll_outgoing_frames(&mut self) -> Vec<Value> {
        std::mem::take(&mut self.outgoing_frames)
    }

    pub fn poll_ui_actions(&mut self) {
        let mut actions = Vec::new();
        if let Some(ui) = self.ui.as_ref() {
            while let Some(action) = ui.try_recv_action() {
                actions.push(action);
            }
        }
        for action in actions {
            self.log(&format!(
                "UI action received: action={}, on={:?}, request_id={:?}, source={:?}, call_phase={:?}, call_type={:?}, group_phase={:?}, screen_worker_present={}, screen_worker_stopping={}, screen_worker_finished={}, restart_pending={}",
                action.action,
                action.on,
                action.request_id,
                action.source,
                self.call.as_ref().map(|call| call.phase),
                self.call.as_ref().map(|call| call.call_type),
                self.group.as_ref().map(|group| group.phase),
                self.screen.is_some(),
                self.screen.as_ref().is_some_and(ScreenCapture::is_stopping),
                self.screen.as_ref().is_some_and(ScreenCapture::is_finished),
                self.screen_restart_pending
            ));
            self.on_ui_action(action);
        }
        self.tick();
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let camera_reassert = self.call.as_ref().and_then(|call| {
            call.camera_state_reassert_at
                .filter(|deadline| now >= *deadline)
                .map(|_| call.camera_on || call.sharing_screen)
        });
        if let Some(camera_on) = camera_reassert {
            if let Some(call) = self.call.as_mut() {
                call.camera_state_reassert_at = None;
            }
            self.camera_state(camera_on);
        }

        let group_events = self
            .media
            .as_ref()
            .map(ZrtcMediaBackend::drain_group_events)
            .unwrap_or_default();
        for event in group_events {
            self.on_group_media_event(event);
        }

        let media_timed_out = self
            .call
            .as_ref()
            .is_some_and(|call| call.phase == CallPhase::InCall)
            && self
                .media
                .as_ref()
                .is_some_and(ZrtcMediaBackend::media_timed_out);
        if media_timed_out {
            self.log("peer media timed out after 20 seconds; ending call");
            self.hangup("media-timeout");
            return;
        }

        let screen_status = self.screen.as_ref().and_then(ScreenCapture::try_status);
        if let Some(status) = screen_status {
            match status {
                Ok(()) => {
                    let stopping = self.screen.as_ref().is_some_and(ScreenCapture::is_stopping);
                    let active_call = self
                        .call
                        .as_ref()
                        .is_some_and(|call| call.phase == CallPhase::InCall);
                    let active_group = self
                        .group
                        .as_ref()
                        .is_some_and(|group| group.phase == GroupPhase::Connected);
                    if stopping {
                        // A ready notification can race with the user's Stop
                        // action. Never signal a share after cancellation.
                    } else if active_call || active_group {
                        self.activate_screen_share();
                    } else {
                        self.stop_screen_capture();
                    }
                }
                Err(error) if crate::screen::is_cancelled_error(&error) => {
                    // Dismissing the system chooser is a normal stop. Preserve a
                    // newer start queued while this cancellation was in flight;
                    // stop_screen_capture normally clears retry state.
                    let restart_pending = self.screen_restart_pending;
                    let restart_source = self.screen_restart_source.take();
                    let restart_request_id = self.screen_restart_request_id.take();
                    self.stop_screen_capture();
                    if restart_pending {
                        self.screen_restart_pending = true;
                        self.screen_restart_source = restart_source;
                        self.screen_restart_request_id = restart_request_id;
                    }
                    let request_id = self.screen_request_id.clone();
                    let stop_was_requested = std::mem::take(&mut self.screen_stop_ack_pending);
                    if !stop_was_requested {
                        self.ui_message(json!({"type":"screenStopped","requestId":request_id}));
                    }
                    self.log("screen chooser cancelled; capture worker is closing");
                }
                Err(error) => {
                    // A terminal error may be queued by PipeWire while an old
                    // stream is being torn down. It must not erase a newer
                    // screen-start request waiting for this worker to exit.
                    let restart_pending = self.screen_restart_pending;
                    let restart_source = self.screen_restart_source.take();
                    let restart_request_id = self.screen_restart_request_id.take();
                    let request_id = self.screen_request_id.clone();
                    self.stop_screen_capture();
                    if restart_pending || self.screen_stop_ack_pending {
                        if restart_pending {
                            self.screen_restart_pending = true;
                            self.screen_restart_source = restart_source;
                            self.screen_restart_request_id = restart_request_id;
                        }
                        self.log(&format!(
                            "previous screen worker ended while stopping/retrying; preserving terminal state: {error}"
                        ));
                    } else {
                        let notify_stop = self.call.as_mut().and_then(|call| {
                            let was_sharing = call.sharing_screen;
                            call.sharing_screen = false;
                            was_sharing.then(|| {
                                json!({
                                    "callId":call.call_id,
                                    "status":0,
                                    "uidTo":call.peer_id
                                })
                            })
                        });
                        if let Some(data) = notify_stop {
                            self.emit(json!({"type":"sendSignal","command":12064,"data":data}));
                            self.restore_call_camera_after_screen_share();
                        }
                        self.ui_message(
                            json!({"type":"screenError","message":error,"requestId":request_id}),
                        );
                    }
                }
            }
        }
        // A stopped capture is retained until its worker has actually exited.
        // This prevents a rapid retry from opening a second portal while the
        // previous request is still being cancelled.
        if self.screen.as_ref().is_some_and(ScreenCapture::is_finished) {
            self.screen.take();
            self.screen_stop_ack_pending = false;
            if self.screen_restart_pending {
                let source = self.screen_restart_source.take();
                let request_id = self.screen_restart_request_id.take();
                self.screen_restart_pending = false;
                self.start_screen_capture(source, request_id);
            } else {
                self.screen_request_id = None;
            }
        }

        let rate_profile_changed = self
            .media
            .as_ref()
            .and_then(ZrtcMediaBackend::take_video_profile_change);
        if let Some(profile) = rate_profile_changed {
            if self.camera.is_some() {
                self.log(&format!(
                    "adaptive video profile -> rung {}: {}x{} {}fps {}kbps",
                    profile.rung,
                    profile.width,
                    profile.height,
                    profile.fps,
                    profile.bitrate / 1000
                ));
                self.stop_camera_capture();
                if !self.start_camera_capture() {
                    self.camera_state(false);
                }
            }
        }

        if let Some(media) = self.media.as_ref() {
            if let Some(level) = media.take_mic_level_update() {
                self.ui_message(json!({"type":"micLevel","level":level}));
            }
            if let Some(volume) = media.take_mic_volume_update() {
                self.ui_message(json!({"type":"micVolume","volume":volume}));
                if let Some(call) = self.call.as_ref() {
                    let mut state =
                        self.ui_state_value(self.ui_phase(), self.ui_status_code(), call);
                    state["micVolume"] = json!(volume);
                    self.ui_message(state);
                }
            }
        }

        let local_keyframe_requested = self
            .media
            .as_ref()
            .is_some_and(ZrtcMediaBackend::take_local_keyframe_request);
        if local_keyframe_requested {
            let sharing_screen = self.call.as_ref().is_some_and(|call| call.sharing_screen);
            if sharing_screen {
                if let Some(screen) = self.screen.as_ref() {
                    screen.request_keyframe();
                }
            } else if let Some(camera) = self.camera.as_ref() {
                camera.request_keyframe();
            }
        }

        let network_screen_fps = self
            .media
            .as_ref()
            .and_then(ZrtcMediaBackend::video_profile)
            .map(|profile| match profile.fps {
                0..=15 => 15,
                16..=20 => 24,
                _ => 30,
            })
            .unwrap_or(30);
        if let Some(screen) = self.screen.as_ref().filter(|screen| !screen.is_stopping()) {
            if screen.set_target_fps(network_screen_fps) {
                self.log(&format!(
                    "adaptive screen-share target -> {network_screen_fps} FPS (network profile)"
                ));
            }
        }

        let mut screen_frames = Vec::new();
        if let Some(screen) = self.screen.as_ref().filter(|screen| !screen.is_stopping()) {
            while let Some(frame) = screen.try_recv() {
                screen_frames.push(frame);
            }
        }
        for frame in screen_frames {
            if let Some(media) = self.media.as_ref() {
                let result = if self
                    .group
                    .as_ref()
                    .is_some_and(|group| group.sharing_screen)
                {
                    media.send_group_share_frame(frame.key, &frame.data, frame.width, frame.height)
                } else {
                    media.send_video_frame(frame.key, &frame.data)
                };
                if let Err(error) = result {
                    self.log(&format!("screen H.264 send failed: {error}"));
                }
            }
            if let Some(ui) = self.ui.as_ref() {
                let fallback = if frame.width > 640 || frame.height > 480 {
                    "avc1.42E028"
                } else {
                    "avc1.42E01E"
                };
                let codec = crate::video::avc_codec_string(&frame.data)
                    .unwrap_or_else(|| fallback.to_owned());
                ui.send_local_video_frame(frame.key, &codec, &frame.data);
            }
        }

        let mut camera_frames = Vec::new();
        if let Some(camera) = self.camera.as_ref() {
            while let Some(frame) = camera.try_recv() {
                camera_frames.push(frame);
            }
        }
        for frame in camera_frames {
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.send_video_frame(frame.key, &frame.data) {
                    self.log(&format!("camera H.264 send failed: {error}"));
                }
            }
            if let Some(ui) = self.ui.as_ref() {
                let fallback = if frame.width > 640 || frame.height > 480 {
                    "avc1.42E028"
                } else {
                    "avc1.42E01E"
                };
                let codec = crate::video::avc_codec_string(&frame.data)
                    .unwrap_or_else(|| fallback.to_owned());
                ui.send_local_video_frame(frame.key, &codec, &frame.data);
            }
        }

        if let Some(media) = self.media.as_ref() {
            for frame in media.drain_video_frames() {
                if let Some(ui) = self.ui.as_ref() {
                    ui.send_video_frame(frame.key, &frame.codec, &frame.data, frame.src);
                }
            }
        }
        if self.call.is_none() && self.group_notice_until.is_some_and(|t| Instant::now() >= t) {
            self.group_notice_until = None;
            self.ui_message(json!({"type":"close","statusCode":"groupUnsupported"}));
        }
        let expired = self.call.as_ref().and_then(|call| {
            call.deadline
                .filter(|deadline| Instant::now() >= *deadline)
                .map(|_| (call.phase, call.role))
        });
        if let Some((phase, role)) = expired {
            match (phase, role) {
                (CallPhase::Outgoing | CallPhase::Ringing, CallRole::Caller) => {
                    // A timed-out outgoing call is still an outgoing call that
                    // ended before connection: mirror the user's local hangup
                    // at 00:00 rather than presenting it as a missed/failed call.
                    self.log("outgoing call timed out before connection; ending at 00:00");
                    self.hangup("timeout");
                }
                (CallPhase::Incoming, CallRole::Callee) => self.reject_call("no-answer"),
                _ => {}
            }
        }

        let group_expired = self.group.as_ref().and_then(|group| {
            group
                .deadline
                .filter(|deadline| Instant::now() >= *deadline)
                .map(|_| group.phase)
        });
        if let Some(phase) = group_expired {
            match phase {
                GroupPhase::Calling => self.group_cancel("no-answer"),
                GroupPhase::Incoming => self.group_reject("no-answer"),
                GroupPhase::Connecting => {
                    self.group_send_answer(3);
                    self.group_finish(0, "groupJoinRejected", false);
                }
                GroupPhase::Connected => {
                    let waiting_for_members = self
                        .group
                        .as_ref()
                        .is_some_and(|group| group.role == GroupRole::Caller && !group.answered);
                    if waiting_for_members {
                        self.group_cancel("no-answer");
                    } else if let Some(group) = self.group.as_mut() {
                        group.deadline = None;
                    }
                }
                GroupPhase::Failed => self.group_finish(0, "groupCallFailed", false),
                GroupPhase::Ended => {}
            }
        }

        let group_ping_due = self.group.as_ref().is_some_and(|group| {
            group.phase == GroupPhase::Connected
                && group.next_ping_at.is_some_and(|at| Instant::now() >= at)
        });
        if group_ping_due {
            self.group_broadcast(12097);
            if let Some(group) = self.group.as_mut() {
                group.next_ping_at = Some(Instant::now() + group.ping_interval);
            }
        }
        let camera_reassert_due = self.group.as_ref().is_some_and(|group| {
            group.phase == GroupPhase::Connected
                && group
                    .camera_state_reassert_at
                    .is_some_and(|at| Instant::now() >= at)
        });
        if camera_reassert_due {
            if let Some(group) = self.group.as_mut() {
                group.camera_state_reassert_at = None;
            }
            self.log("group member joined; reasserting local camera state");
            self.group_broadcast(12098);
        }
    }

    pub fn on_zalo_frame(&mut self, frame: Value) {
        if !frame.is_object() {
            return;
        }
        let kind = string_at(&frame, "type").unwrap_or_default();
        let command = frame.get("command").cloned().unwrap_or(Value::Null);
        let data = frame
            .get("data")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        self.log(&format!(
            "Zalo event received: type={kind}, command={command}"
        ));
        match kind.as_str() {
            "update" => self.on_update(&command, &data),
            "request" => self.on_request(&command, &data),
            "control" => {
                let act = string_at(&data, "act").unwrap_or_default();
                self.on_control(&act, &data);
            }
            "recvSignal" => self.on_recv_signal(&command, &data),
            "response" => {
                if command == "getAliasName" {
                    self.on_alias_name(&data);
                }
            }
            _ => self.log(&format!("unhandled frame type={kind} command={command}")),
        }
    }

    fn on_update(&mut self, command: &Value, data: &Value) {
        if (command == "init" || command == "updateLocal") && data.get("local").is_some() {
            merge_object(&mut self.local, data.get("local").unwrap_or(&Value::Null));
        }
    }

    fn on_request(&mut self, command: &Value, data: &Value) {
        match command.as_str().unwrap_or_default() {
            "makeCall" => self.make_call(data),
            "endCall" => self.hangup("zalo"),
            "listDevice" | "ping" | "hide" => {}
            other => self.log(&format!("unhandled request {other}")),
        }
    }

    fn make_call(&mut self, data: &Value) {
        if self.call.is_some() || self.group.is_some() {
            self.log("makeCall while already in a call; ignored");
            return;
        }
        if data.get("groupInfo").is_some()
            || number_at(data, "type").is_some_and(|n| n == 5 || n == 6)
        {
            if self.group_enabled {
                self.group_outgoing(data);
            } else {
                self.group_not_supported(data);
            }
            return;
        }
        self.group_notice_until = None;
        let partner = data
            .get("partner")
            .and_then(Value::as_array)
            .and_then(|list| list.first())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let peer_id = value_string(partner.get("id"));
        let peer_name = string_at(&partner, "name").unwrap_or_default();
        let avatar = string_at(&partner, "avatar").unwrap_or_default();
        let raw_type = number_at(data, "type");
        let call_type = if raw_type == Some(3) { 3 } else { 1 };
        self.log(&format!("outgoing call type map: protocol.type={raw_type:?} -> internal.call_type={call_type} (3=video, 1=voice)"));
        let call_id = new_call_id();

        self.call = Some(Call {
            role: CallRole::Caller,
            phase: CallPhase::Outgoing,
            call_id,
            peer_id: peer_id.clone(),
            peer_uid: 0,
            local_uid: 0,
            peer_name: peer_name.clone(),
            avatar: avatar.clone(),
            call_type,
            session_id: String::new(),
            server: None,
            servers: Vec::new(),
            p2p_candidates: Vec::new(),
            media_config: Value::Null,
            deadline: Some(Instant::now() + CALL_SETUP_TIMEOUT),
            started_at: None,
            muted: false,
            speaker_off: false,
            camera_on: call_type == 3,
            camera_was_on: false,
            camera_state_reassert_at: None,
            peer_muted: false,
            peer_camera_off: false,
            peer_sharing: false,
            sharing_screen: false,
        });
        self.phase = CallPhase::Outgoing;
        self.emit(json!({"type":"update","command":"callState","data":{"state":"incall"}}));
        self.emit(
            json!({"type":"response","command":"show","data":{"error":0,"extendData":"success"}}),
        );
        self.emit(json!({"type":"sendSignal","command":401,"data":{
            "callId":call_id,
            "calleeId":peer_id,
            "codec":EMPTY_CODEC,
            "type":call_type
        }}));
        if call_type == 3 {
            self.emit(json!({"type":"sendSignal","command":12006,"data":{
                "callId":call_id,"calleeId":peer_id,"data":"","isOnCam":0,"isOnMic":-1
            }}));
        }
        self.ui_state("outgoing", "calling");
        if !peer_id.is_empty() {
            self.emit(
                json!({"type":"request","command":"getAliasName","data":{"noisedId":peer_id}}),
            );
        }
        self.log(&format!("outgoing call {call_id} to {peer_id}"));
    }

    fn group_not_supported(&mut self, data: &Value) {
        let group = data.get("groupInfo").unwrap_or(&Value::Null);
        let name = string_at(group, "name")
            .or_else(|| string_at(group, "groupName"))
            .unwrap_or_else(|| "Cuộc gọi nhóm".to_string());
        self.emit(
            json!({"type":"response","command":"show","data":{"error":0,"extendData":"success"}}),
        );
        self.emit(json!({"type":"update","command":"callState","data":{"state":"free"}}));
        self.group_notice_until = Some(Instant::now() + Duration::from_secs(3));
        self.ui_message(json!({
            "type":"state","phase":"outgoing","statusCode":"groupUnsupported",
            "name":name,"avatar":string_at(group,"avatar").unwrap_or_default(),
            "video":true,"group":true,"muted":false,"speakerOff":false
        }));
    }

    fn group_outgoing(&mut self, data: &Value) {
        let info = data.get("groupInfo").unwrap_or(&Value::Null);
        let partners = data
            .get("partner")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let invited = partners
            .iter()
            .map(|partner| value_string(partner.get("id")))
            .filter(|id| !id.is_empty())
            .collect::<Vec<_>>();
        let mut members = HashMap::new();
        for (index, partner) in partners.iter().enumerate() {
            let key = value_string(partner.get("id"));
            let key = if key.is_empty() {
                (index + 1).to_string()
            } else {
                key
            };
            members.insert(
                key,
                GroupMember {
                    name: string_at(partner, "name").unwrap_or_default(),
                    avatar: string_at(partner, "avatar").unwrap_or_default(),
                    state: "calling".into(),
                    muted: false,
                    cam_off: true,
                },
            );
        }
        let title = string_at(info, "name")
            .or_else(|| string_at(info, "groupName"))
            .unwrap_or_else(|| "Cuộc gọi nhóm".to_string());
        let avatar = string_at(info, "avatar").unwrap_or_default();
        let group_id_str = value_string(info.get("id"));
        let call_type = if number_at(data, "type") == Some(6) {
            1
        } else {
            0
        };
        let max_users = number_at(info, "maxUsers").filter(|n| *n > 0).unwrap_or(8) as u32;
        let call_id = new_call_id();
        self.group = Some(GroupCall {
            role: GroupRole::Caller,
            phase: GroupPhase::Calling,
            call_id,
            host_call: 0,
            group_id: 0,
            group_id_str: group_id_str.clone(),
            local_uid: 0,
            call_type,
            sess_id: String::new(),
            servers: Vec::new(),
            config: json!({}),
            title: title.clone(),
            avatar: avatar.clone(),
            max_users,
            invited: invited.clone(),
            members,
            member_id_aliases: HashMap::new(),
            pending_member_ids: VecDeque::new(),
            pending_sfu_uids: VecDeque::new(),
            observed_sfu_uids: HashSet::new(),
            deadline: Some(Instant::now() + Duration::from_secs(60)),
            started_at: None,
            next_ping_at: None,
            camera_state_reassert_at: None,
            ping_interval: Duration::from_secs(10),
            muted: false,
            speaker_off: false,
            // Group call type 1 is video; keep the caller's camera intent
            // through SFU setup just as the incoming group-answer path does.
            camera_on: call_type == 1,
            camera_was_on: false,
            answered: false,
            share_screen_id: 0,
            sharing_screen: false,
        });
        self.phase = CallPhase::Outgoing;
        self.emit(json!({"type":"update","command":"callState","data":{"state":"incall"}}));
        self.emit(
            json!({"type":"response","command":"show","data":{"error":0,"extendData":"success"}}),
        );
        self.emit(json!({
            "type":"sendSignal","command":12433,"data":{
                "callId":call_id,
                "data":json_cpp(&json!({
                    "extraData":"",
                    "groupAvatar":avatar,
                    "groupId":group_id_str,
                    "groupName":title,
                    "maxUsers":max_users,
                    "noiseId":invited
                })),
                "groupId":group_id_str,
                "partners":json_cpp(&json!(invited)),
                "typeRequest":1
            }
        }));
        self.log(&format!(
            "outgoing group call {call_id}: {} invitee(s)",
            invited.len()
        ));
        self.group_debug_reset();
        self.group_debug("outgoing-created");
        let (invite_total, named_invites, avatar_invites) = self
            .group
            .as_ref()
            .map(|group| {
                let total = group.invited.len();
                let named = group
                    .invited
                    .iter()
                    .filter(|id| {
                        group
                            .members
                            .get(*id)
                            .is_some_and(|member| !member.name.is_empty())
                    })
                    .count();
                let avatars = group
                    .invited
                    .iter()
                    .filter(|id| {
                        group
                            .members
                            .get(*id)
                            .is_some_and(|member| !member.avatar.is_empty())
                    })
                    .count();
                (total, named, avatars)
            })
            .unwrap_or_default();
        self.group_debug(&format!(
            "invite-metadata total={invite_total} named={named_invites} avatar={avatar_invites}"
        ));
        self.group_send_ui_state("outgoing", "groupCalling", None);
    }

    fn group_request_answered(&mut self, data: &Value) {
        if !self.group.as_ref().is_some_and(|group| {
            group.role == GroupRole::Caller && group.phase == GroupPhase::Calling
        }) {
            return;
        }

        let mut response = parse_json_value(data.get("data"));
        merge_object(&mut response, &parse_json_value(data.get("params")));
        let setting = parse_json_value(response.get("callSetting"));
        let status = number_at(&response, "status")
            .or_else(|| number_at(data, "status"))
            .unwrap_or(0);
        let host_call = number_at(data, "hostCall")
            .or_else(|| number_at(&response, "hostCall"))
            .unwrap_or(0)
            .max(0) as u32;
        let call_id = number_at(&response, "callId")
            .or_else(|| number_at(data, "callId"))
            .unwrap_or(0)
            .max(0) as u32;
        let group_id = number_at(&response, "groupId").unwrap_or(0).max(0) as u32;
        let interval = number_at(&response, "interval").unwrap_or(10_000).max(1) as u64;
        let ping_interval = if interval < 1000 {
            Duration::from_millis(interval * 1000)
        } else {
            Duration::from_millis(interval)
        };
        let sess_id = string_at(&setting, "session")
            .or_else(|| string_at(&response, "session"))
            .unwrap_or_default();
        let servers = setting
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let config = parse_json_value(setting.get("zrtcConfig"));
        let group_key = config
            .get("srtpKey")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let key_fp = crate::crypto::srtp::sha1(&[group_key.as_bytes()]);
        let key_fp = key_fp[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let config_fields = config
            .as_object()
            .map(|object| {
                let mut fields = object.keys().cloned().collect::<Vec<_>>();
                fields.sort_unstable();
                fields.join(",")
            })
            .unwrap_or_else(|| "not-object".into());
        self.log(&format!(
            "group-config-boundary status={status} config_kind={} config_fields={} key_type={} key_bytes={} key_fp={key_fp}",
            if setting.get("zrtcConfig").is_some_and(Value::is_string) { "json-string" } else if setting.get("zrtcConfig").is_some_and(Value::is_object) { "object" } else { "missing-or-other" },
            config_fields,
            config.get("srtpKey").map(|value| match value {
                Value::String(_) => "string",
                Value::Null => "null",
                Value::Bool(_) => "bool",
                Value::Number(_) => "number",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            }).unwrap_or("missing"),
            group_key.len()
        ));

        if status != 0 || host_call == 0 || sess_id.is_empty() || servers.is_empty() {
            let reason = string_at(&response, "msg").unwrap_or_else(|| {
                format!("group call request refused or incomplete (status={status})")
            });
            self.group_setup_failed("groupCallFailed", &reason);
            return;
        }

        let (call_id, call_type, title, avatar, max_users, invited) = {
            let Some(group) = self.group.as_mut() else {
                return;
            };
            if call_id != 0 {
                group.call_id = call_id;
            }
            group.host_call = host_call;
            group.local_uid = host_call;
            group.group_id = group_id;
            group.sess_id = sess_id.clone();
            group.servers = servers.clone();
            group.config = config.clone();
            group.ping_interval = ping_interval;
            group.deadline = Some(Instant::now() + Duration::from_secs(60));
            // Preserve the requested call mode. The SFU video transport is
            // initialized below and camera capture is started after media.start().
            (
                group.call_id,
                group.call_type,
                group.title.clone(),
                group.avatar.clone(),
                group.max_users,
                group.invited.clone(),
            )
        };

        self.group_debug("request-accepted");
        let camera_on = self.group.as_ref().is_some_and(|group| group.camera_on);
        let params = MediaSessionParams {
            call_id,
            host_call,
            uid: host_call,
            peer_uid: host_call,
            sess_id: sess_id.as_bytes().to_vec(),
            is_caller: true,
            is_group: true,
            send_udp: self.send_udp,
            video_enabled: camera_on,
            video_config: config,
            p2p_candidates: Vec::new(),
        };
        let addresses = parse_server_addresses(&servers);
        let selected = match self.media.as_ref() {
            Some(media) => match media.prepare(params, &addresses) {
                Ok(selected) => selected,
                Err(error) => {
                    self.group_setup_failed("groupJoinRejected", &error);
                    return;
                }
            },
            None => {
                self.group_setup_failed("groupJoinRejected", "media backend unavailable");
                return;
            }
        };
        if self.send_udp && selected.is_none() {
            self.group_setup_failed(
                "groupJoinRejected",
                "no group media relay accepted InitZRTP",
            );
            return;
        }
        let Some(server) = select_server(&servers, selected) else {
            self.group_setup_failed("groupJoinRejected", "no usable group media server");
            return;
        };

        if let Some(media) = self.media.as_ref() {
            media.start();
            if let Err(error) = media.send_group_video_quality_config() {
                self.log(&format!("group VidQualConfig failed: {error}"));
            }
        }
        if self.group.as_ref().is_some_and(|group| group.camera_on) {
            let camera_on = self.start_camera_capture();
            if let Some(group) = self.group.as_mut() {
                group.camera_on = camera_on;
            }
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.set_group_camera(camera_on) {
                    self.log(&format!("group camera state update failed: {error}"));
                }
            }
        }
        if let Some(group) = self.group.as_mut() {
            group.phase = GroupPhase::Connected;
            group.started_at = Some(Instant::now());
            group.next_ping_at = Some(Instant::now() + group.ping_interval);
        }

        let nested = json_cpp(&json!({
            "groupAvatar":avatar,
            "groupName":title,
            "hostCall":host_call,
            "maxUsers":max_users,
            "noiseId":invited
        }));
        self.emit(json!({
            "type":"sendSignal","command":12434,"data":{
                "callId":call_id,
                "callType":call_type,
                "data":json_cpp(&json!({
                    "codec":"",
                    "data":nested,
                    "extendData":"",
                    "rtcpAddress":string_at(&server,"rtcpaddr").or_else(||string_at(&server,"rtcp")).unwrap_or_default(),
                    "rtcpAddressIPv6":string_at(&server,"rtcpaddrIPv6").or_else(||string_at(&server,"rtcpIPv6")).unwrap_or_default(),
                    "rtpAddress":string_at(&server,"rtpaddr").or_else(||string_at(&server,"rtp")).unwrap_or_default(),
                    "rtpAddressIPv6":string_at(&server,"rtpaddrIPv6").or_else(||string_at(&server,"rtpIPv6")).unwrap_or_default()
                })),
                "groupId":group_id.to_string(),
                "partners":json_cpp(&json!(invited)),
                "session":sess_id
            }
        }));
        self.group_broadcast(12098);
        self.group_update_status();
        self.log(&format!(
            "outgoing group call {call_id} joined SFU relay {server:?}"
        ));
    }

    fn group_setup_failed(&mut self, status: &str, reason: &str) {
        if let Some(group) = self.group.as_mut() {
            group.phase = GroupPhase::Failed;
            group.deadline = Some(Instant::now() + Duration::from_secs(3));
        }
        self.group_send_ui_state("failed", status, None);
        self.log(&format!("group call setup failed: {reason}"));
    }

    fn group_members_ui(&self) -> Vec<Value> {
        let Some(group) = self.group.as_ref() else {
            return Vec::new();
        };

        // Keep each original invite visible with the name/avatar supplied by
        // Zalo. A ring control may expose only an SFU UID; until signalling
        // supplies an authoritative Zalo-ID <-> SFU-UID pair, hide that
        // anonymous UID rather than replacing a named invite with a blank tile.
        // This deliberately avoids guessing identity from ring/roster order.
        let mapped_sfu_uids = group
            .member_id_aliases
            .values()
            .cloned()
            .collect::<HashSet<_>>();
        let pending_invite_count = group
            .invited
            .iter()
            .filter(|invite| {
                !group.member_id_aliases.contains_key(*invite)
                    && group.members.get(*invite).is_some_and(|member| {
                        matches!(member.state.as_str(), "calling" | "ringing" | "incall")
                    })
            })
            .count();
        let hidden_anonymous_sfu = group
            .members
            .iter()
            .filter_map(|(key, member)| {
                let uid = key.parse::<u32>().ok()?;
                (uid != group.local_uid
                    && uid != group.share_screen_id
                    && !group.invited.iter().any(|invite| invite == key)
                    && !mapped_sfu_uids.contains(key)
                    && member.name.is_empty()
                    && member.avatar.is_empty())
                .then_some(key.clone())
            })
            .take(pending_invite_count)
            .collect::<HashSet<_>>();

        group
            .members
            .iter()
            .filter(|(uid, member)| {
                matches!(member.state.as_str(), "calling" | "ringing" | "incall")
                    && !hidden_anonymous_sfu.contains(*uid)
            })
            .map(|(uid, member)| {
                json!({
                    "src":uid,
                    "name":member.name,
                    "avatar":member.avatar,
                    "muted":member.muted,
                    "camOff":member.cam_off,
                    "state":member.state
                })
            })
            .collect()
    }

    fn group_send_ui_state(&self, phase: &str, status: &str, since: Option<u64>) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        let members = self.group_members_ui();
        let mut state = json!({
            "type":"state","phase":phase,"statusCode":status,
            "name":group.title,"avatar":group.avatar,
            "video":true,"group":true,"sharing":group.sharing_screen,
            "cameraOn":group.camera_on,"cameraWasOn":group.camera_was_on,"muted":group.muted,
            "speakerOff":group.speaker_off,"members":members,
            "selfAvatar":string_at(&self.local,"avatar").unwrap_or_default()
        });
        if let Some(since) = since {
            state["since"] = json!(since);
        }
        self.ui_message(state);
    }

    fn group_state_payload(&self) -> Value {
        let group = self.group.as_ref().expect("group state");
        json!({
            "audioState":if group.muted {1} else {0},
            "callId":group.call_id,
            "callState":3,
            "hostCall":group.host_call,
            "userId":group.local_uid,
            "videoState":if group.camera_on {0} else {1}
        })
    }

    fn group_broadcast(&mut self, command: u32) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        if group.phase != GroupPhase::Connected {
            return;
        }
        let state = self.group_state_payload();
        self.emit(json!({"type":"sendSignal","command":command,"data":state}));
    }

    fn group_update_status(&mut self) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        if !matches!(group.phase, GroupPhase::Calling | GroupPhase::Connected) {
            return;
        }
        let names = group
            .members
            .iter()
            .filter(|(uid, member)| {
                uid.parse::<u32>().ok() != Some(group.share_screen_id) && member.state == "incall"
            })
            .map(|(_, member)| {
                if member.name.is_empty() {
                    Value::Null
                } else {
                    json!(member.name)
                }
            })
            .collect::<Vec<_>>();
        let ringing = group.members.iter().any(|(uid, member)| {
            uid.parse::<u32>().ok() != Some(group.share_screen_id) && member.state == "ringing"
        });
        let status = if !names.is_empty() {
            "groupParticipants"
        } else if group.role == GroupRole::Caller && !group.answered {
            if ringing {
                "groupRinging"
            } else {
                "groupCalling"
            }
        } else {
            "groupWaitingOthers"
        };
        // The renderer's timer consumes an epoch-millisecond timestamp
        // (Date.now() - since), matching the legacy UI contract.
        let since = if group.answered && !names.is_empty() {
            group.started_at.map(instant_to_unix_ms)
        } else {
            None
        };
        let mut ui = json!({
            "type":"state","phase":if group.phase == GroupPhase::Connected {"connected"} else {"outgoing"},
            "statusCode":status,"name":group.title,"avatar":group.avatar,
            "video":true,"group":true,"muted":group.muted,"speakerOff":group.speaker_off,
            "members":self.group_members_ui(),
            "selfAvatar":string_at(&self.local,"avatar").unwrap_or_default(),
            "statusParams":{"count":names.len()+1,"names":names}
        });
        if let Some(seconds) = since {
            ui["since"] = json!(seconds);
        }
        self.group_debug("ui-state");
        self.ui_message(ui);
    }

    // Sanitized call-identity trace stored beside the executable so it can be
    // inspected without touching the user's existing native-engine log.
    // Account IDs, names and avatars are never written to this file.
    fn group_debug_reset(&self) {
        if let Some(path) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("zcall-group-debug.log")))
        {
            let _ = File::create(path);
        }
    }

    fn group_debug_uid(group: &GroupCall, uid: u32) -> String {
        if uid == 0 {
            return "zero".into();
        }
        if uid == group.local_uid {
            return "LOCAL".into();
        }
        if uid == group.share_screen_id {
            return "SHARE".into();
        }
        for (index, invite) in group.invited.iter().enumerate() {
            if group.member_id_aliases.get(invite) == Some(&uid.to_string()) {
                return format!("invite#{index}/uid:{uid}");
            }
        }
        format!("uid:{uid}")
    }

    fn group_debug_field(group: &GroupCall, value: &str) -> String {
        if value.is_empty() {
            return "missing".into();
        }
        if value == "0" {
            return "zero".into();
        }
        if let Some(index) = group.invited.iter().position(|id| id == value) {
            return format!("invite#{index}");
        }
        if let Ok(uid) = value.parse::<u32>() {
            return Self::group_debug_uid(group, uid);
        }
        if value.parse::<u64>().is_ok() {
            return "numeric-account-id".into();
        }
        "other-id".into()
    }

    fn group_debug(&self, event: &str) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        let members = group
            .members
            .iter()
            .map(|(key, member)| {
                let tag = if let Some(index) = group.invited.iter().position(|id| id == key) {
                    format!("invite#{index}")
                } else if let Ok(uid) = key.parse::<u32>() {
                    Self::group_debug_uid(group, uid)
                } else {
                    "other-id".into()
                };
                format!("{tag}:{}", member.state)
            })
            .collect::<Vec<_>>()
            .join(",");
        let aliases = group
            .member_id_aliases
            .iter()
            .filter_map(|(id, uid)| {
                group
                    .invited
                    .iter()
                    .position(|invite| invite == id)
                    .map(|index| format!("invite#{index}->{}", uid))
            })
            .collect::<Vec<_>>()
            .join(",");
        let pending_ids = group
            .pending_member_ids
            .iter()
            .filter_map(|id| group.invited.iter().position(|invite| invite == id))
            .map(|index| format!("invite#{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let pending_uids = group
            .pending_sfu_uids
            .iter()
            .map(|uid| Self::group_debug_uid(group, *uid))
            .collect::<Vec<_>>()
            .join(",");
        let observed = group
            .observed_sfu_uids
            .iter()
            .map(|uid| Self::group_debug_uid(group, *uid))
            .collect::<Vec<_>>()
            .join(",");
        let line = format!(
            "event={event} phase={:?} role={:?} local={} host={} invited={} members=[{}] aliases=[{}] pending_ids=[{}] pending_uids=[{}] observed=[{}]",
            group.phase, group.role, group.local_uid, group.host_call, group.invited.len(),
            members, aliases, pending_ids, pending_uids, observed
        );
        if let Some(path) = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("zcall-group-debug.log")))
        {
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    fn group_finish(&mut self, status: i32, close_code: &str, send_signals: bool) {
        self.stop_camera_capture();
        self.stop_screen_capture();
        let Some(mut group) = self.group.take() else {
            return;
        };
        let connected = group.phase == GroupPhase::Connected;
        group.phase = GroupPhase::Ended;
        let duration = if connected {
            elapsed_seconds_rounded(group.started_at)
        } else {
            0
        };
        let (rx_total, tx_total) = self
            .media
            .as_ref()
            .map(ZrtcMediaBackend::stats)
            .unwrap_or_default();
        if let Some(media) = self.media.as_ref() {
            media.stop();
        }
        if send_signals && connected {
            self.emit(json!({
                "type":"sendSignal","command":12438,"data":{
                    "callId":group.call_id,
                    "data":json_cpp(&json!({
                        "callType":group.call_type,"duration":duration,"extraData":"",
                        "groupId":group.group_id,"status":0
                    })),
                    "hostCall":group.host_call
                }
            }));
            self.emit(json!({
                "type":"sendSignal","command":12446,"data":{
                    "callId":group.call_id,"callType":group.call_type,
                    "data":json_cpp(&json!({
                        "Codec":["opus/16000/1","20"],"RxTotalPkt":rx_total,"TxTotalPkt":tx_total,
                        "sysInfo":"native-engine"
                    })),
                    "duration":duration,"hostCall":group.host_call,
                    // Legacy group finish reports the call's Unix-ms start time.
                    "joinTime":group.started_at.map(instant_to_unix_ms).unwrap_or(0),
                    "status":status
                }
            }));
        }
        self.emit(json!({"type":"update","command":"callState","data":{"state":"free"}}));
        self.phase = CallPhase::Idle;
        self.ui_message(json!({
            "type":"close","statusCode":close_code,
            "statusParams":{"duration":duration}
        }));
        self.log(&format!(
            "group call {} finished ({duration}s)",
            group.call_id
        ));
    }

    fn group_cancel(&mut self, why: &str) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        let call_id = group.call_id;
        let host_call = group.host_call;
        let call_type = group.call_type;
        let group_id = group.group_id;
        if host_call != 0 {
            self.emit(json!({
                "type":"sendSignal","command":12437,"data":{
                    "callId":call_id,
                    "data":json_cpp(&json!({"callType":call_type,"duration":0,"extraData":"","groupId":group_id})),
                    "hostCall":host_call
                }
            }));
        }
        self.group_finish(
            0,
            if why == "no-answer" {
                "groupNoAnswer"
            } else {
                "groupCancelled"
            },
            false,
        );
    }

    fn group_hangup(&mut self, why: &str) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        if group.phase == GroupPhase::Incoming {
            self.group_reject(why);
        } else if group.role == GroupRole::Caller && !group.answered {
            self.group_cancel(why);
        } else {
            let status = if group.phase == GroupPhase::Connected {
                50
            } else {
                0
            };
            self.group_finish(status, "endedWithDuration", true);
        }
    }

    fn group_send_answer(&mut self, status: u8) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        let call_id = group.call_id;
        let call_type = group.call_type;
        let group_id = group.group_id;
        let host_call = group.host_call;
        let session = group.sess_id.clone();
        self.emit(json!({
            "type":"sendSignal","command":12436,"data":{
                "callId":call_id,
                "data":json_cpp(&json!({"callType":call_type,"codec":"","extendData":"","groupId":group_id,"status":status})),
                "hostCall":host_call,"session":session,"status":status
            }
        }));
    }

    fn group_reject(&mut self, why: &str) {
        if !self
            .group
            .as_ref()
            .is_some_and(|group| group.phase == GroupPhase::Incoming)
        {
            return;
        }
        self.group_send_answer(3);
        self.group_finish(0, "groupDeclined", false);
        self.log(&format!("incoming group call rejected: {why}"));
    }

    fn group_answer(&mut self, cam_start_off: bool) {
        if !self
            .group
            .as_ref()
            .is_some_and(|group| group.phase == GroupPhase::Incoming)
        {
            return;
        }
        let (params, addresses, call_id) = {
            let group = self.group.as_mut().expect("group call");
            group.camera_on = !cam_start_off;
            group.phase = GroupPhase::Connecting;
            group.deadline = Some(Instant::now() + CALL_SETUP_TIMEOUT);
            (
                MediaSessionParams {
                    call_id: group.call_id,
                    host_call: group.host_call,
                    uid: group.local_uid,
                    peer_uid: group.host_call,
                    sess_id: group.sess_id.as_bytes().to_vec(),
                    is_caller: false,
                    is_group: true,
                    send_udp: self.send_udp,
                    video_enabled: group.camera_on,
                    video_config: group.config.clone(),
                    p2p_candidates: Vec::new(),
                },
                parse_server_addresses(&group.servers),
                group.call_id,
            )
        };
        self.group_send_ui_state("connecting", "connecting", None);
        let selected = match self.media.as_ref() {
            Some(media) => media.prepare(params, &addresses).ok().flatten(),
            None => None,
        };
        if self
            .group
            .as_ref()
            .is_none_or(|group| group.call_id != call_id)
        {
            return;
        }
        if self.send_udp && selected.is_none() {
            self.group_send_answer(3);
            self.group_finish(0, "groupJoinRejected", false);
            return;
        }
        if let Some(media) = self.media.as_ref() {
            media.start();
            if let Err(error) = media.send_group_video_quality_config() {
                self.log(&format!("group VidQualConfig failed: {error}"));
            }
        }
        if let Some(group) = self.group.as_mut() {
            group.phase = GroupPhase::Connected;
            group.answered = true;
            group.started_at = Some(Instant::now());
            group.deadline = None;
            group.next_ping_at = Some(Instant::now() + group.ping_interval);
        }
        // Accept the room first, then make the local capture/send pipeline
        // authoritative before advertising videoState=0 to the other peers.
        self.group_send_answer(0);
        if self.group.as_ref().is_some_and(|group| group.camera_on) {
            let camera_on = self.start_camera_capture();
            if let Some(group) = self.group.as_mut() {
                group.camera_on = camera_on;
            }
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.set_group_camera(camera_on) {
                    self.log(&format!(
                        "incoming group camera state update failed: {error}"
                    ));
                }
                if camera_on {
                    if let Err(error) = media.send_group_video_quality_config() {
                        self.log(&format!("incoming group VidQualConfig failed: {error}"));
                    }
                }
            }
        }
        self.group_broadcast(12098);
        self.group_update_status();
    }

    fn group_member_update(&mut self, uid: &str, update: GroupMember) {
        self.group_member_update_inner(uid, update);
    }

    fn group_member_update_from_media(&mut self, uid: &str, update: GroupMember) {
        self.group_member_update_inner(uid, update);
    }

    /// Resolve an SFU UID to the sole still-ringing invitee, matching the legacy
    /// group engine. This is intentionally limited to a one-invitee call: with
    /// multiple invitees there is no safe positional identity mapping.
    fn group_member_for_sfu_uid(&self, uid: &str) -> Option<GroupMember> {
        let group = self.group.as_ref()?;
        if let Some(member) = group.members.get(uid) {
            return Some(member.clone());
        }
        if group.role != GroupRole::Caller
            || group.invited.len() != 1
            || group.member_id_aliases.contains_key(&group.invited[0])
            || group.invited[0] == uid
        {
            return None;
        }
        group
            .members
            .get(&group.invited[0])
            .filter(|member| matches!(member.state.as_str(), "calling" | "ringing"))
            .cloned()
    }

    fn group_member_update_inner(&mut self, uid: &str, mut update: GroupMember) {
        let uid = uid.to_owned();
        let (joined, rekeyed_invitee, request_video_uid) = {
            let Some(group) = self.group.as_mut() else {
                return;
            };
            let media_uid = uid.parse::<u32>().ok();
            if uid == group.local_uid.to_string()
                || media_uid == Some(group.share_screen_id)
                || media_uid == Some(0)
            {
                return;
            }

            // Legacy _groupMember() moved the only outstanding invitee to the
            // newly observed SFU UID. The Zalo group controls in live calls use
            // that UID in fromId/userId, not the original invited account ID.
            let mut rekeyed = false;
            if group.role == GroupRole::Caller
                && group.invited.len() == 1
                && !group.members.contains_key(&uid)
                && !group.member_id_aliases.contains_key(&group.invited[0])
                && media_uid.is_some()
            {
                let invite_id = group.invited[0].clone();
                if invite_id != uid {
                    if let Some(invitee) = group.members.remove(&invite_id) {
                        if matches!(invitee.state.as_str(), "calling" | "ringing") {
                            if update.name.is_empty() {
                                update.name = invitee.name.clone();
                            }
                            if update.avatar.is_empty() {
                                update.avatar = invitee.avatar.clone();
                            }
                            group
                                .member_id_aliases
                                .insert(invite_id.clone(), uid.clone());
                            group.pending_member_ids.retain(|id| id != &invite_id);
                            group
                                .pending_sfu_uids
                                .retain(|pending| Some(*pending) != media_uid);
                            if let Some(media_uid) = media_uid {
                                group.observed_sfu_uids.insert(media_uid);
                            }
                            rekeyed = true;
                        } else {
                            group.members.insert(invite_id, invitee);
                        }
                    }
                }
            }

            let previous = group.members.get(&uid).map(|member| member.state.clone());

            // State-only roster/audio events must retain identity metadata.
            if let Some(existing) = group.members.get(&uid) {
                if update.name.is_empty() {
                    update.name = existing.name.clone();
                }
                if update.avatar.is_empty() {
                    update.avatar = existing.avatar.clone();
                }
            }
            if update.state == "left" {
                group.pending_member_ids.retain(|id| id != &uid);
                if let Some(media_uid) = media_uid {
                    group
                        .pending_sfu_uids
                        .retain(|pending| *pending != media_uid);
                    group.observed_sfu_uids.remove(&media_uid);
                }
            }
            group.members.insert(uid.clone(), update);
            let joined = group
                .members
                .get(&uid)
                .is_some_and(|member| member.state == "incall")
                && previous.as_deref() != Some("incall");
            let request_video_uid = media_uid.filter(|_| {
                group.phase == GroupPhase::Connected
                    && group
                        .members
                        .get(&uid)
                        .is_some_and(|member| member.state == "incall" && !member.cam_off)
            });
            (joined, rekeyed, request_video_uid)
        };

        if rekeyed_invitee {
            self.group_debug("single-invitee-rekeyed");
        }
        if let Some(uid) = request_video_uid {
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.request_group_video(uid) {
                    self.log(&format!("group video subscribe for {uid} failed: {error}"));
                }
            }
        }
        if joined {
            if let Some(group) = self.group.as_mut() {
                group.answered = true;
                group.deadline = None;
                group.camera_state_reassert_at = Some(Instant::now() + Duration::from_secs(2));
            }
            // Do not request remote video here. The legacy engine waited until
            // the first valid video RTP source arrived before sending VidQualReq.
            self.group_broadcast(12098);
        }
        self.group_update_status();
    }

    /// Remember a real Zalo participant joining under their long account ID.
    /// The SFU uses a separate u32 SSRC; pair identities only when both sides
    /// have independently announced a join, preserving event order.
    fn group_note_zalo_member_join(&mut self, zalo_id: &str) {
        let Some(group) = self.group.as_mut() else {
            return;
        };
        if group.role != GroupRole::Caller
            || zalo_id.is_empty()
            || zalo_id.parse::<u32>().is_ok()
            || !group.invited.iter().any(|id| id == zalo_id)
            || group.member_id_aliases.contains_key(zalo_id)
            || !group
                .members
                .get(zalo_id)
                .is_some_and(|member| member.state == "incall")
        {
            return;
        }
        if !group.pending_member_ids.iter().any(|id| id == zalo_id) {
            group.pending_member_ids.push_back(zalo_id.to_owned());
        }
        self.group_debug("zalo-member-joined");
        self.group_pair_unique_observed_member();
    }

    /// Remember a media source independently of roster delivery. A single
    /// unresolved Zalo invitee plus a single unresolved SFU source is an
    /// unambiguous one-to-one match, regardless of which side arrived first.
    fn group_note_sfu_member_media(&mut self, sfu_uid: u32) {
        if let Some(group) = self.group.as_mut() {
            if group.role != GroupRole::Caller
                || sfu_uid == 0
                || sfu_uid == group.local_uid
                || sfu_uid == group.share_screen_id
            {
                return;
            }
            group.observed_sfu_uids.insert(sfu_uid);
        }
        let tag = self
            .group
            .as_ref()
            .map(|group| Self::group_debug_uid(group, sfu_uid))
            .unwrap_or_default();
        self.group_debug(&format!("media-source {tag}"));
        self.group_pair_unique_observed_member();
    }

    /// A group state broadcast can carry both identities: userId is the
    /// participant's SFU UID, while fromId may be their long Zalo account ID.
    /// This explicit pair is authoritative and takes precedence over roster order.
    fn group_map_zalo_member_to_sfu_uid(&mut self, zalo_id: &str, sfu_uid: u32) {
        let mapped = {
            let Some(group) = self.group.as_mut() else {
                return;
            };
            if group.role != GroupRole::Caller
                || sfu_uid == 0
                || sfu_uid == group.local_uid
                || sfu_uid == group.share_screen_id
                || group.member_id_aliases.contains_key(zalo_id)
                || !group.invited.iter().any(|id| id == zalo_id)
            {
                return;
            }
            let Some(mut invitee) = group.members.remove(zalo_id) else {
                return;
            };
            let sfu_key = sfu_uid.to_string();
            if let Some(existing) = group.members.remove(&sfu_key) {
                if invitee.name.is_empty() {
                    invitee.name = existing.name;
                }
                if invitee.avatar.is_empty() {
                    invitee.avatar = existing.avatar;
                }
                if existing.state == "incall" {
                    invitee.state = "incall".into();
                }
                if !existing.cam_off {
                    invitee.cam_off = false;
                }
                invitee.muted |= existing.muted;
            }
            if invitee.state == "incall" {
                group.answered = true;
                group.deadline = None;
            }
            group
                .member_id_aliases
                .insert(zalo_id.to_owned(), sfu_key.clone());
            group.members.insert(sfu_key, invitee);
            group.pending_member_ids.retain(|id| id != zalo_id);
            group.pending_sfu_uids.retain(|uid| *uid != sfu_uid);
            group.observed_sfu_uids.insert(sfu_uid);
            true
        };
        if mapped {
            self.log(&format!(
                "group mapped Zalo participant {zalo_id} to explicit SFU UID {sfu_uid}"
            ));
            if let Some(group) = self.group.as_mut() {
                if group.phase == GroupPhase::Connected {
                    group.camera_state_reassert_at = Some(Instant::now() + Duration::from_secs(2));
                }
            }
            self.group_broadcast(12098);
            self.group_update_status();
        }
    }

    fn group_pair_unique_observed_member(&mut self) {
        self.group_debug("pair-check");
        let candidate = self.group.as_ref().and_then(|group| {
            if group.role != GroupRole::Caller || group.pending_member_ids.len() != 1 {
                return None;
            }
            let mut candidates = group.observed_sfu_uids.iter().copied().filter(|uid| {
                let key = uid.to_string();
                *uid != group.local_uid
                    && *uid != group.share_screen_id
                    && !group
                        .member_id_aliases
                        .values()
                        .any(|mapped_uid| mapped_uid == &key)
                    && !group.invited.iter().any(|id| id == &key)
            });
            let only = candidates.next()?;
            candidates.next().is_none().then_some(only)
        });
        if let Some(sfu_uid) = candidate {
            let tag = self
                .group
                .as_ref()
                .map(|group| Self::group_debug_uid(group, sfu_uid))
                .unwrap_or_default();
            self.group_debug(&format!("pair-unique-candidate {tag}"));
            if let Some(group) = self.group.as_mut() {
                group.pending_sfu_uids.clear();
                group.pending_sfu_uids.push_back(sfu_uid);
            }
            self.group_pair_pending_member_ids();
        }
    }

    /// Pair only a single unresolved invitee with a single unresolved SFU
    /// source. Multiple entries have no documented positional correspondence.
    fn group_pair_pending_member_ids(&mut self) {
        let mut mapped_uids = Vec::new();
        if let Some(group) = self.group.as_mut() {
            if group.role != GroupRole::Caller
                || group.pending_member_ids.len() != 1
                || group.pending_sfu_uids.len() != 1
            {
                return;
            }
            while let (Some(zalo_id), Some(sfu_uid)) = (
                group.pending_member_ids.front().cloned(),
                group.pending_sfu_uids.front().copied(),
            ) {
                group.pending_member_ids.pop_front();
                group.pending_sfu_uids.pop_front();
                if group.member_id_aliases.contains_key(&zalo_id) {
                    continue;
                }
                let Some(invitee) = group.members.remove(&zalo_id) else {
                    continue;
                };
                let sfu_key = sfu_uid.to_string();
                let existing_sfu = group.members.remove(&sfu_key);
                let mut merged = invitee;
                if let Some(existing) = existing_sfu {
                    if merged.name.is_empty() {
                        merged.name = existing.name;
                    }
                    if merged.avatar.is_empty() {
                        merged.avatar = existing.avatar;
                    }
                    if existing.state == "incall" {
                        merged.state = "incall".into();
                    }
                    // A decoded video source is stronger evidence than the
                    // earlier signalling state; never regress it to camera-off.
                    if !existing.cam_off {
                        merged.cam_off = false;
                    }
                }
                if merged.state == "incall" {
                    group.answered = true;
                    group.deadline = None;
                }
                group.member_id_aliases.insert(zalo_id, sfu_key.clone());
                group.members.insert(sfu_key, merged);
                mapped_uids.push(sfu_uid);
            }
        }
        if !mapped_uids.is_empty() {
            self.group_debug("identity-pair-applied");
            self.log(&format!(
                "group mapped {} Zalo participant ID(s) to SFU media UID(s)",
                mapped_uids.len()
            ));
            if let Some(group) = self.group.as_mut() {
                if group.phase == GroupPhase::Connected {
                    group.camera_state_reassert_at = Some(Instant::now() + Duration::from_secs(2));
                }
            }
            self.group_broadcast(12098);
            self.group_update_status();
        }
    }

    fn on_early_audio(&mut self, phase: &'static str) {
        let current = self
            .call
            .as_ref()
            .and_then(|call| (call.role == CallRole::Caller).then_some(call.phase));
        if !matches!(current, Some(CallPhase::Ringing | CallPhase::Connecting)) {
            return;
        }
        match phase {
            "first" if current == Some(CallPhase::Ringing) => {
                if let Some(call) = self.call.as_mut() {
                    call.phase = CallPhase::Connecting;
                    call.deadline = None;
                }
                self.phase = CallPhase::Connecting;
                self.log("early media received before answer; entering connecting phase");
                self.ui_state("connecting", "connecting");
            }
            "connect" => {
                self.log("sustained early media received; connecting caller media");
                self.connect_caller(&Value::Null);
            }
            _ => {}
        }
    }

    fn on_group_media_event(&mut self, event: GroupMediaEvent) {
        match event {
            GroupMediaEvent::Roster(uids) => self.group_roster(uids),
            GroupMediaEvent::MemberLeft(uid, reason) => {
                self.log(&format!("group SFU member {uid} left (reason {reason})"));
                let update = self
                    .group
                    .as_ref()
                    .and_then(|group| group.members.get(&uid.to_string()).cloned())
                    .map(|mut member| {
                        member.state = "left".into();
                        member
                    })
                    .unwrap_or(GroupMember {
                        name: String::new(),
                        avatar: String::new(),
                        state: "left".into(),
                        muted: false,
                        cam_off: true,
                    });
                self.group_member_update_from_media(&uid.to_string(), update);
                self.group_update_status();
                self.group_check_empty();
            }
            GroupMediaEvent::PeerEnd(reason) => {
                let status = if reason == 2 {
                    "groupKicked"
                } else {
                    "groupEnded"
                };
                self.log(&format!("removed from group SFU room (reason {reason})"));
                self.group_finish(0, status, false);
            }
            GroupMediaEvent::ShareReady | GroupMediaEvent::ShareKeyframeRequested => {
                if let Some(screen) = self.screen.as_ref() {
                    screen.request_keyframe();
                }
            }
            GroupMediaEvent::MemberVideo(uid) => {
                self.group_note_sfu_member_media(uid);
                let key = uid.to_string();
                let mut member = self.group_member_for_sfu_uid(&key).unwrap_or(GroupMember {
                    name: String::new(),
                    avatar: String::new(),
                    state: "incall".into(),
                    muted: false,
                    cam_off: false,
                });
                // An RTP video source proves that this participant's camera is
                // transmitting, even before its 12098 camera-state broadcast.
                member.state = "incall".into();
                member.cam_off = false;
                self.group_member_update_from_media(&key, member);
                self.group_debug("video-member-applied");
                // Like the legacy _noteVideoMember(), request a layer only
                // after this source has delivered a valid video RTP packet.
                if let Some(media) = self.media.as_ref() {
                    if let Err(error) = media.request_group_video(uid) {
                        self.log(&format!(
                            "group VidQualReq for video SSRC {uid} failed: {error}"
                        ));
                    }
                }
            }
            GroupMediaEvent::MemberAudio(uid) => {
                self.group_note_sfu_member_media(uid);
                let key = uid.to_string();
                let update = self
                    .group_member_for_sfu_uid(&key)
                    .map(|mut member| {
                        member.state = "incall".into();
                        member
                    })
                    .unwrap_or(GroupMember {
                        name: String::new(),
                        avatar: String::new(),
                        state: "incall".into(),
                        muted: false,
                        cam_off: true,
                    });
                self.group_member_update_from_media(&key, update);
                self.group_debug("audio-member-applied");
            }
            GroupMediaEvent::AudioFault(kind) => {
                self.ui_message(json!({"type":"audioFault","kind":kind}));
            }
            GroupMediaEvent::EarlyAudio(phase) => self.on_early_audio(phase),
            GroupMediaEvent::MemberSpeaking(uid, on) => {
                self.ui_message(json!({"type":"speaking","uid":uid,"on":on}));
            }
            GroupMediaEvent::ShareFailed(reason) => {
                self.log(&format!("group screen-share peer failed: {reason}"));
                self.stop_screen_capture();
                self.ui_message(json!({"type":"screenError","message":reason}));
            }
        }
    }

    fn group_roster(&mut self, uids: Vec<u32>) {
        let Some(group) = self.group.as_ref() else {
            return;
        };
        let local_uid = group.local_uid;
        let share_uid = group.share_screen_id;
        let mut seen = HashSet::new();
        let ordered = uids
            .into_iter()
            .filter(|uid| *uid != 0 && *uid != local_uid && *uid != share_uid && seen.insert(*uid))
            .collect::<Vec<_>>();
        let present = ordered.iter().copied().collect::<HashSet<_>>();
        let roster = ordered
            .iter()
            .map(|uid| Self::group_debug_uid(group, *uid))
            .collect::<Vec<_>>()
            .join(",");
        self.group_debug(&format!("roster local={} remote=[{roster}]", local_uid));

        // Record observed SFU members, but derive alias order only from this
        // ordered room snapshot, never from independently-timed camera/audio
        // packets. The snapshot also includes sources that made anonymous UI
        // tiles before ROOM_UPDATED was delivered.
        if let Some(group) = self.group.as_mut() {
            if group.role == GroupRole::Caller {
                group.observed_sfu_uids = present.clone();
                group.pending_sfu_uids.clear();
                for uid in &ordered {
                    let key = uid.to_string();
                    let already_mapped = group
                        .member_id_aliases
                        .values()
                        .any(|mapped_uid| mapped_uid == &key);
                    let direct_invitee = group.invited.iter().any(|id| id == &key);
                    if !already_mapped && !direct_invitee {
                        group.pending_sfu_uids.push_back(*uid);
                    }
                }
            }
        }
        self.group_pair_pending_member_ids();

        for uid in ordered {
            let key = uid.to_string();
            let member = self
                .group_member_for_sfu_uid(&key)
                .map(|mut member| {
                    member.state = "incall".into();
                    member
                })
                .unwrap_or(GroupMember {
                    name: String::new(),
                    avatar: String::new(),
                    state: "incall".into(),
                    muted: false,
                    cam_off: true,
                });
            self.group_member_update_from_media(&key, member);
        }

        let departed = self
            .group
            .as_ref()
            .map(|group| {
                group
                    .members
                    .iter()
                    .filter(|(uid, member)| {
                        member.state == "incall"
                            && uid.parse::<u32>().is_ok_and(|uid| !present.contains(&uid))
                    })
                    .map(|(uid, member)| (uid.clone(), member.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for (uid, mut member) in departed {
            member.state = "left".into();
            self.group_member_update_from_media(&uid, member);
        }
        self.group_debug("roster-applied");
        self.group_update_status();
        self.group_check_empty();
    }

    fn group_control(&mut self, act: &str, data: &Value) {
        if act == "group_request" {
            self.group_incoming(data);
            return;
        }
        let Some(group) = self.group.as_ref() else {
            return;
        };
        let id = number_at(data, "id").or_else(|| number_at(data, "callId"));
        if id.is_some_and(|id| id as u32 != group.call_id) {
            return;
        }
        let user_id = value_string(data.get("userId"));
        let from_id = value_string(data.get("fromId"));
        let user_tag = Self::group_debug_field(group, &user_id);
        let from_tag = Self::group_debug_field(group, &from_id);
        let is_known_member_id = |candidate: &str| {
            !candidate.is_empty()
                && (group.invited.iter().any(|id| id == candidate)
                    || group.members.contains_key(candidate)
                    || group.member_id_aliases.contains_key(candidate))
        };
        let invited_zalo_id = if group.invited.iter().any(|id| id == &from_id) {
            Some(from_id.clone())
        } else if group.invited.iter().any(|id| id == &user_id) {
            Some(user_id.clone())
        } else {
            None
        };
        // userId in a group-state packet is the sender's SFU/media UID.
        // Preserve it separately from fromId, which may be the long Zalo ID.
        let explicit_sfu_uid = if invited_zalo_id.is_some() {
            [user_id.as_str(), from_id.as_str()]
                .into_iter()
                .filter(|candidate| !group.invited.iter().any(|id| id == candidate))
                .find_map(|candidate| candidate.parse::<u32>().ok())
                .filter(|uid| *uid != 0 && *uid != group.local_uid && *uid != group.share_screen_id)
        } else {
            None
        };
        let who_raw = invited_zalo_id
            .clone()
            .or_else(|| {
                if is_known_member_id(&user_id) {
                    Some(user_id.clone())
                } else if is_known_member_id(&from_id) {
                    Some(from_id.clone())
                } else if !user_id.is_empty() && user_id != "0" {
                    Some(user_id.clone())
                } else if !from_id.is_empty() {
                    Some(from_id.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let who_uid = who_raw.parse::<u32>().unwrap_or(0);
        let who_is_zero = who_raw.is_empty() || who_raw == "0";
        let who_is_local = who_uid != 0 && who_uid == group.local_uid;
        let raw_who_key = if who_uid != 0 {
            who_uid.to_string()
        } else {
            who_raw.clone()
        };
        let who_key = group
            .member_id_aliases
            .get(&raw_who_key)
            .cloned()
            .unwrap_or(raw_who_key);
        if act == "group_end_call" || act == "group_cancel" {
            if group.phase == GroupPhase::Incoming
                && (who_is_zero || who_uid == group.host_call || who_is_local)
            {
                self.group_finish(0, "groupEnded", false);
            } else if who_is_local {
                self.group_finish(
                    if group.phase == GroupPhase::Connected {
                        50
                    } else {
                        0
                    },
                    "groupEnded",
                    false,
                );
            } else if !who_is_zero {
                if let Some(group) = self.group.as_mut() {
                    group.pending_member_ids.retain(|id| id != &who_key);
                    if let Some(member) = group.members.get_mut(&who_key) {
                        member.state = "left".into();
                    }
                }
                self.group_update_status();
                self.group_check_empty();
            }
            return;
        }
        if act == "group_answer"
            && group.phase == GroupPhase::Incoming
            && (who_is_zero || who_is_local)
        {
            self.group_finish(0, "groupEnded", false);
            return;
        }
        if who_is_zero || who_is_local {
            return;
        }
        let key = who_key;
        let current = self.group_member_for_sfu_uid(&key).unwrap_or(GroupMember {
            name: String::new(),
            avatar: String::new(),
            state: "ringing".into(),
            muted: false,
            cam_off: true,
        });
        if act == "group_ring_ring" && current.state == "incall" {
            return;
        }
        let mut next = current.clone();
        match act {
            "group_broadcast" => {
                next.state = if number_at(data, "callState") == Some(3) {
                    "incall"
                } else {
                    "ringing"
                }
                .into();
                if let Some(audio_state) = number_at(data, "audioState") {
                    next.muted = audio_state == 1;
                }
                if let Some(video_state) = number_at(data, "videoState") {
                    next.cam_off = video_state == 1;
                }
            }
            "group_answer" => {
                next.state = "incall".into();
                if let Some(audio_state) = number_at(data, "audioState") {
                    next.muted = audio_state == 1;
                }
                if let Some(video_state) = number_at(data, "videoState") {
                    next.cam_off = video_state == 1;
                }
            }
            "group_ring_ring" => {
                next.state = "ringing".into();
                if let Some(audio_state) = number_at(data, "audioState") {
                    next.muted = audio_state == 1;
                }
                if let Some(video_state) = number_at(data, "videoState") {
                    next.cam_off = video_state == 1;
                }
            }
            _ => return,
        }
        let joined = next.state == "incall";
        self.group_debug(&format!(
            "control act={act} userId={user_tag} fromId={from_tag} selected={} callState={:?} audioState={:?} videoState={:?} joined={joined} explicit_uid={:?}",
            Self::group_debug_field(group, &key),
            number_at(data, "callState"),
            number_at(data, "audioState"),
            number_at(data, "videoState"),
            explicit_sfu_uid
        ));
        self.group_member_update(&key, next);
        if let (Some(zalo_id), Some(sfu_uid)) = (invited_zalo_id.as_deref(), explicit_sfu_uid) {
            self.group_map_zalo_member_to_sfu_uid(zalo_id, sfu_uid);
        } else if joined && who_uid == 0 {
            self.group_note_zalo_member_join(&key);
            self.group_update_status();
        }
    }

    fn group_incoming(&mut self, data: &Value) {
        if self.call.is_some() || self.group.is_some() {
            return;
        }
        let setting = parse_json_value(data.get("callSetting"));
        let local_uid = number_at(data, "receiverId").unwrap_or(0).max(0) as u32;
        let host_call = number_at(data, "hostCall").unwrap_or(0).max(0) as u32;
        let mut members = HashMap::new();
        if let Some(partners) = data.get("partnerInfo").and_then(Value::as_array) {
            for partner in partners {
                let uid = number_at(partner, "userId").unwrap_or(0).max(0) as u32;
                if uid == 0 || uid == local_uid {
                    continue;
                }
                members.insert(
                    uid.to_string(),
                    GroupMember {
                        name: string_at(partner, "name").unwrap_or_default(),
                        avatar: string_at(partner, "avatar").unwrap_or_default(),
                        state: if number_at(partner, "callState") == Some(3) {
                            "incall"
                        } else {
                            "ringing"
                        }
                        .into(),
                        muted: number_at(partner, "audioState") == Some(1),
                        cam_off: number_at(partner, "videoState") == Some(1),
                    },
                );
            }
        }
        let interval = number_at(data, "interval").unwrap_or(10_000).max(1) as u64;
        let ping_interval = if interval < 1000 {
            Duration::from_millis(interval * 1000)
        } else {
            Duration::from_millis(interval)
        };
        let sess_id = string_at(&setting, "session")
            .or_else(|| string_at(data, "session"))
            .unwrap_or_default();
        let servers = setting
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let group = GroupCall {
            role: GroupRole::Callee,
            phase: GroupPhase::Incoming,
            call_id: number_at(data, "id").unwrap_or(0).max(0) as u32,
            host_call,
            group_id: number_at(data, "groupId").unwrap_or(0).max(0) as u32,
            group_id_str: value_string(data.get("groupId")),
            local_uid,
            call_type: if number_at(data, "callType") == Some(1) {
                1
            } else {
                0
            },
            sess_id,
            servers,
            config: parse_json_value(setting.get("zrtcConfig")),
            title: string_at(data, "groupName").unwrap_or_else(|| "Cuộc gọi nhóm".into()),
            avatar: string_at(data, "groupAvatar").unwrap_or_default(),
            max_users: 8,
            invited: Vec::new(),
            members,
            member_id_aliases: HashMap::new(),
            pending_member_ids: VecDeque::new(),
            pending_sfu_uids: VecDeque::new(),
            observed_sfu_uids: HashSet::new(),
            deadline: Some(Instant::now() + RING_TIMEOUT),
            started_at: None,
            next_ping_at: None,
            camera_state_reassert_at: None,
            ping_interval,
            muted: false,
            speaker_off: false,
            camera_on: false,
            camera_was_on: false,
            answered: false,
            share_screen_id: 0,
            sharing_screen: false,
        };
        let call_id = group.call_id;
        let session = group.sess_id.clone();
        let host_name = group
            .members
            .get(&host_call.to_string())
            .map(|m| m.name.clone())
            .or_else(|| string_at(data, "Dname"))
            .unwrap_or_default();
        self.group = Some(group);
        self.phase = CallPhase::Incoming;
        self.emit(json!({"type":"update","command":"callState","data":{"state":"incall"}}));
        self.emit(json!({
            "type":"sendSignal","command":12439,"data":{
                "callId":call_id,"data":json_cpp(&json!({"callType":1,"extraData":"","status":1})),
                "hostCall":host_call,"session":session,"status":1
            }
        }));
        self.ui_message(json!({
            "type":"state","phase":"incoming","statusCode":"incomingGroupInvite",
            "name":self.group.as_ref().unwrap().title,"avatar":self.group.as_ref().unwrap().avatar,
            "video":true,"group":true,"inviter":host_name,
            "selfAvatar":string_at(&self.local,"avatar").unwrap_or_default()
        }));
    }

    fn group_check_empty(&mut self) {
        let empty = self.group.as_ref().is_some_and(|group| {
            group.connected()
                && !group.members.iter().any(|(uid, member)| {
                    uid.parse::<u32>().ok() != Some(group.share_screen_id)
                        && matches!(member.state.as_str(), "incall" | "ringing" | "calling")
                })
        });
        if empty {
            self.group_hangup("alone");
        }
    }

    fn on_control(&mut self, act: &str, data: &Value) {
        let d = data.get("data").cloned().unwrap_or_else(|| json!({}));
        if act.starts_with("group_") {
            self.group_control(act, &d);
            return;
        }
        let call_id = d
            .get("callId")
            .or_else(|| d.get("id"))
            .or_else(|| data.get("callId"))
            .or_else(|| data.get("id"));
        let current_id = self.call.as_ref().map(|c| c.call_id);
        match act {
            "request" => self.incoming_call(&d),
            "ring_ring" => {
                if call_id
                    .and_then(number_value)
                    .is_some_and(|id| current_id != Some(id as u32))
                {
                    self.log("ignoring stale ring notification");
                    return;
                }
                if let Some(media) = self.media.as_ref() {
                    let phase = self
                        .call
                        .as_ref()
                        .map(|call| format!("{:?}", call.phase))
                        .unwrap_or_else(|| "none".into());
                    media.note_debug_event(&format!(
                        "peer-control act=ring_ring phase={phase} audioState={:?} videoState={:?}",
                        number_at(&d, "audioState"),
                        number_at(&d, "videoState")
                    ));
                }
                if let Some(call) = self.call.as_mut() {
                    if call.role == CallRole::Caller && call.phase == CallPhase::Outgoing {
                        call.phase = CallPhase::Ringing;
                        call.deadline = Some(Instant::now() + RING_TIMEOUT);
                        self.phase = CallPhase::Ringing;
                        self.ui_state("outgoing", "ringing");
                    }
                }
            }
            "answer" => {
                if let Some(id) = call_id.and_then(number_value).map(|n| n as u32) {
                    self.last_ended_call = Some(id);
                    if current_id.is_some_and(|current| current != id) {
                        return;
                    }
                }
                let Some(call) = self.call.as_ref() else {
                    return;
                };
                if call.role == CallRole::Callee && call.phase == CallPhase::Incoming {
                    self.log("incoming call answered on another device");
                    self.finish(0, None, false);
                    return;
                }
                if call.role != CallRole::Caller || call.phase == CallPhase::InCall {
                    return;
                }
                let params = parse_json_value(d.get("params"));
                let status = number_at(&d, "status")
                    .or_else(|| number_at(&params, "status"))
                    .unwrap_or(0);
                if status != 0 {
                    if status == 6 {
                        // Zalo reports a no-answer/handshake timeout through
                        // control.answer status 6. This is not an explicit
                        // receiver rejection: record it as caller-cancelled,
                        // with the same zero-duration semantics as local hangup.
                        self.log(
                            "outgoing call timed out remotely (answer status 6); sending end signal and recording 00:00",
                        );
                        let close_code = if call.phase == CallPhase::Outgoing {
                            "recipientUnreachable"
                        } else {
                            "recipientNoAnswer"
                        };
                        if let Some(call) = self.call.as_ref() {
                            self.emit(json!({"type":"sendSignal","command":405,"data":{
                                "callId":call.call_id,
                                "callType":if call.call_type == 3 {1} else {0},
                                "status":0,
                                "toId":call.peer_id
                            }}));
                        }
                        self.finish(103, Some(close_code), true);
                    } else {
                        self.log(&format!("peer declined call (status {status})"));
                        let code = if status == 3 {
                            "recipientDeclined"
                        } else {
                            "recipientUnreachable"
                        };
                        self.finish(160, Some(code), true);
                    }
                    return;
                }
                self.connect_caller(&params);
            }
            "end_call" | "cancel" | "reject" | "busy" => {
                if call_id.is_some()
                    && current_id.is_some()
                    && call_id.and_then(number_value).map(|n| n as u32) != current_id
                {
                    return;
                }
                if self
                    .call
                    .as_ref()
                    .is_some_and(|c| c.phase == CallPhase::Incoming)
                {
                    self.finish(0, None, false);
                } else {
                    self.peer_ended(act);
                }
            }
            "onoff_camera" | "mute_audio" | "share_screen" => {
                if call_id
                    .and_then(number_value)
                    .is_some_and(|id| current_id != Some(id as u32))
                {
                    self.log("ignoring peer media-state notification for another call");
                    return;
                }
                let params = parse_json_value(d.get("params"));
                let kind = number_at(&params, "type")
                    .or_else(|| number_at(&params, "status"))
                    .unwrap_or(0);
                let phase = self
                    .call
                    .as_ref()
                    .map(|call| format!("{:?}", call.phase))
                    .unwrap_or_else(|| "none".into());
                if let Some(media) = self.media.as_ref() {
                    media.note_debug_event(&format!(
                        "peer-control act={act} phase={phase} value={kind}"
                    ));
                }
                if let Some(call) = self.call.as_mut() {
                    match act {
                        "onoff_camera" => call.peer_camera_off = kind == 0,
                        "mute_audio" => call.peer_muted = kind == 1,
                        _ => call.peer_sharing = kind == 1,
                    }
                    self.ui_peer_state();
                }
            }
            _ => {}
        }
    }

    fn incoming_call(&mut self, data: &Value) {
        if self.call.is_some() || self.group.is_some() {
            self.log("incoming call while busy; ignored");
            return;
        }
        let call_id = number_at(data, "callId").unwrap_or(0).max(0) as u32;
        if self.last_ended_call == Some(call_id) {
            self.log("late incoming event for ended call; ignored");
            return;
        }
        let params = parse_json_value(data.get("params"));
        let extend = parse_json_value(params.get("extendData"));
        let server = extend
            .get("serverAddr")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let raw_call_type = number_at(&extend, "callType");
        let call_type = if raw_call_type == Some(1) { 3 } else { 1 };
        self.log(&format!("incoming call type map: extendData.callType={raw_call_type:?} -> internal.call_type={call_type} (1=video, otherwise voice)"));
        let p2p_candidates = parse_p2p_candidates(extend.get("p2p"));
        let media_config = parse_json_value(
            params
                .get("zrtc_config")
                .or_else(|| params.get("zrtcConfig")),
        );
        let peer_id = string_at(data, "uidN")
            .or_else(|| data.get("uidN").map(|v| value_string(Some(v))))
            .unwrap_or_default();
        let peer_uid = number_at(data, "uidFrom").unwrap_or(0).max(0) as u32;
        let local_uid = number_at(data, "uidTo").unwrap_or(0).max(0) as u32;
        let peer_name = string_at(&params, "Dname").unwrap_or_default();
        let avatar = string_at(&params, "avatar").unwrap_or_default();
        let sess = string_at(data, "session")
            .or_else(|| string_at(&params, "sessId"))
            .unwrap_or_default();
        self.call = Some(Call {
            role: CallRole::Callee,
            phase: CallPhase::Incoming,
            call_id,
            peer_id: peer_id.clone(),
            peer_uid,
            local_uid,
            peer_name,
            avatar,
            call_type,
            session_id: sess,
            server: Some(json!({
                "rtpaddr": data.get("rtpAddress").cloned().unwrap_or(Value::Null),
                "rtcpaddr": data.get("rtcpAddress").cloned().unwrap_or(Value::Null),
                "rtpIPv6":server.get("rtpIPv6").cloned().unwrap_or(json!("")),
                "rtcpIPv6":server.get("rtcpIPv6").cloned().unwrap_or(json!(""))
            })),
            servers: vec![server],
            p2p_candidates,
            media_config,
            deadline: Some(Instant::now() + RING_TIMEOUT),
            started_at: None,
            muted: false,
            speaker_off: false,
            camera_on: false,
            camera_was_on: false,
            camera_state_reassert_at: None,
            peer_muted: false,
            peer_camera_off: false,
            peer_sharing: false,
            sharing_screen: false,
        });
        self.phase = CallPhase::Incoming;
        if crate::devices::should_block_incoming() {
            self.log("rejecting incoming call because required audio devices are unavailable");
            self.reject_call("no-device");
            return;
        }
        self.emit(json!({"type":"update","command":"callState","data":{"state":"incall"}}));
        self.emit(json!({"type":"request","command":"getAliasName","data":{"noisedId":peer_id}}));
        self.emit(
            json!({"type":"sendSignal","command":407,"data":{"callId":call_id,"callerId":peer_id}}),
        );
        self.ui_state(
            "incoming",
            if call_type == 3 {
                "incomingVideo"
            } else {
                "incomingVoice"
            },
        );
        self.log(&format!("incoming call {call_id}"));
    }

    fn on_recv_signal(&mut self, command: &Value, data: &Value) {
        match value_string(Some(command)).as_str() {
            "401" => self.caller_init_answer(data),
            "12433" => self.group_request_answered(data),
            "12044" => {
                if self.group.as_ref().is_some_and(|group| {
                    number_at(data, "callId").is_some_and(|id| id as u32 != group.call_id)
                }) {
                    self.log("ignoring stale group screen-share response");
                    return;
                }
                self.group_share_response(data);
            }
            "405" | "409" => {
                let Some(current_id) = self.call.as_ref().map(|call| call.call_id) else {
                    return;
                };
                let signal_id = number_at(data, "callId")
                    .or_else(|| number_at(data, "id"))
                    .map(|value| value as u32);
                if signal_id != Some(current_id) {
                    self.log(&format!(
                        "ignoring stale/unmatched recvSignal {command} for call {:?}; active call is {current_id}",
                        signal_id
                    ));
                    return;
                }
                self.peer_ended("server-end");
            }
            "402" | "407" | "408" | "416" | "406" | "12006" | "12064" => {}
            other => self.log(&format!("unhandled recvSignal {other}")),
        }
    }

    fn caller_init_answer(&mut self, data: &Value) {
        if !self
            .call
            .as_ref()
            .is_some_and(|c| c.role == CallRole::Caller && c.phase == CallPhase::Outgoing)
        {
            return;
        }
        if number_at(data, "status").unwrap_or(0) != 0 {
            self.ui_status("callFailed", None);
            self.finish(103, Some("callFailed"), true);
            return;
        }
        let servers = data
            .get("servers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if servers.is_empty() {
            self.ui_status("callFailed", None);
            self.finish(103, Some("callFailed"), true);
            return;
        }
        let sess = string_at(data, "sessId").unwrap_or_default();
        let local_uid = number_at(data, "fromId")
            .or_else(|| number_at(&self.local, "id"))
            .unwrap_or(0)
            .max(0) as u32;
        let peer_uid = number_at(data, "toId").unwrap_or(0).max(0) as u32;
        let media_config =
            parse_json_value(data.get("zrtc_config").or_else(|| data.get("zrtcConfig")));
        let call_id = self.call.as_ref().map(|c| c.call_id).unwrap_or(0);
        let call_type = self.call.as_ref().map(|c| c.call_type).unwrap_or(1);
        if let Some(call) = self.call.as_mut() {
            call.session_id = sess.clone();
            call.local_uid = local_uid;
            call.peer_uid = peer_uid;
            call.servers = servers.clone();
            call.media_config = media_config.clone();
        }
        let addresses = parse_server_addresses(&servers);
        let params = MediaSessionParams {
            call_id,
            host_call: 0,
            uid: local_uid,
            peer_uid,
            sess_id: sess.as_bytes().to_vec(),
            is_caller: true,
            is_group: false,
            send_udp: self.send_udp,
            video_enabled: call_type == 3,
            video_config: media_config,
            p2p_candidates: Vec::new(),
        };
        let selected = match self.media.as_ref() {
            Some(media) => match media.prepare(params, &addresses) {
                Ok(addr) => addr,
                Err(err) => {
                    self.log(&format!("media prepare failed: {err}"));
                    None
                }
            },
            None => None,
        };
        let selected_server = select_server(&servers, selected).or_else(|| {
            if self.send_udp {
                None
            } else {
                servers.first().cloned()
            }
        });
        if selected_server.is_none() {
            self.ui_status("serverUnreachable", None);
            self.hangup("no-server");
            return;
        }
        let server = selected_server.unwrap_or_else(|| json!({}));
        if let Some(call) = self.call.as_mut() {
            call.server = Some(server.clone());
        }
        // Start receive-only RTP before the callee answers. The shared media
        // workers are upgraded to full duplex when connect_caller() runs.
        if let Some(media) = self.media.as_ref() {
            media.start_with_audio(false);
        }
        let p2p = self
            .media
            .as_ref()
            .map(|media| media.p2p_candidates(false))
            .unwrap_or_default();
        let extend = extend_data(&server, call_type, false, p2p, false);
        self.emit(json!({
            "type":"sendSignal","command":416,"data":{
                "callId":call_id,
                "calleeId":self.call.as_ref().map(|c| c.peer_id.clone()).unwrap_or_default(),
                "codec":CODEC,
                "extendData":json_cpp(&extend),
                "rtcpAddress":server.get("rtcpaddr").or_else(||server.get("rtcp")).cloned().unwrap_or(json!("")),
                "rtpAddress":server.get("rtpaddr").or_else(||server.get("rtp")).cloned().unwrap_or(json!("")),
                "session":sess
            }
        }));
        self.log("call-v2 401 handled; sent 416");
    }

    fn connect_caller(&mut self, params: &Value) {
        let Some(call) = self.call.as_ref() else {
            return;
        };
        if call.role != CallRole::Caller || call.phase == CallPhase::InCall {
            return;
        }
        // The 402 response carries the callee's P2P candidates inside
        // extendData. Install them before start() launches the binder worker.
        let extend = parse_json_value(
            params
                .get("extendData")
                .or_else(|| params.get("extend_data")),
        );
        let candidates = parse_p2p_candidates(extend.get("p2p"));
        if !candidates.is_empty() {
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.set_p2p_candidates(candidates.clone()) {
                    self.log(&format!("could not install callee P2P candidates: {error}"));
                } else {
                    self.log(&format!(
                        "installed {} callee P2P candidate(s)",
                        candidates.len()
                    ));
                }
            }
        }
        let Some(call) = self.call.as_mut() else {
            return;
        };
        call.phase = CallPhase::InCall;
        call.deadline = None;
        call.started_at = Some(Instant::now());
        let call_id = call.call_id;
        let peer_id = call.peer_id.clone();
        if let Some(media) = self.media.as_ref() {
            media.start();
        }
        self.phase = CallPhase::InCall;
        self.emit(
            json!({"type":"sendSignal","command":408,"data":{"callId":call_id,"calleeId":peer_id}}),
        );
        self.start_call_camera();
        self.ui_state("connected", "connected");
        let _ = params;
    }

    fn answer_call(&mut self, camera_off: bool) {
        if !self
            .call
            .as_ref()
            .is_some_and(|c| c.role == CallRole::Callee && c.phase == CallPhase::Incoming)
        {
            return;
        }
        // Match upstream's call timer origin: the callee starts counting when
        // the user answers, not after synchronous media preparation finishes.
        let answer_started_at = Instant::now();
        let (
            call_id,
            peer_id,
            peer_uid,
            local_uid,
            sess,
            server,
            call_type,
            p2p_candidates,
            media_config,
        ) = {
            let c = self.call.as_ref().expect("checked above");
            (
                c.call_id,
                c.peer_id.clone(),
                c.peer_uid,
                c.local_uid,
                c.session_id.clone(),
                c.server.clone().unwrap_or_else(|| json!({})),
                c.call_type,
                c.p2p_candidates.clone(),
                c.media_config.clone(),
            )
        };
        if let Some(c) = self.call.as_mut() {
            c.phase = CallPhase::Connecting;
            c.deadline = None;
            c.started_at = Some(answer_started_at);
            if c.call_type == 3 {
                c.camera_on = !camera_off;
            }
        }
        self.phase = CallPhase::Connecting;
        self.emit(json!({"type":"request","command":"action","data":{"id":5}}));
        let addresses = parse_server_addresses(std::slice::from_ref(&server));
        let params = MediaSessionParams {
            call_id,
            host_call: 0,
            uid: local_uid,
            peer_uid,
            sess_id: sess.as_bytes().to_vec(),
            is_caller: false,
            is_group: false,
            send_udp: self.send_udp,
            video_enabled: call_type == 3,
            video_config: media_config,
            p2p_candidates,
        };
        let selected = match self.media.as_ref() {
            Some(media) => media.prepare(params, &addresses).ok().flatten(),
            None => None,
        };
        if selected.is_none() && self.send_udp {
            self.emit(json!({"type":"sendSignal","command":402,"data":{
                "callId":call_id,"callerId":peer_id,"codec":EMPTY_CODEC,"extendData":json_cpp(&json!({})),
                "rtcpAddress":"","rtpAddress":"","session":sess,"status":3
            }}));
            self.finish(160, Some("serverUnreachable"), true);
            return;
        }
        let chosen = select_server(std::slice::from_ref(&server), selected).unwrap_or(server);
        if let Some(c) = self.call.as_mut() {
            c.phase = CallPhase::InCall;
            c.server = Some(chosen.clone());
        }
        self.phase = CallPhase::InCall;
        let p2p = self
            .media
            .as_ref()
            .map(|media| media.p2p_candidates(true))
            .unwrap_or_default();
        let extend = extend_data(&chosen, call_type, true, p2p, true);
        self.emit(json!({
            "type":"sendSignal","command":402,"data":{
                "callId":call_id,"callerId":peer_id,"codec":CODEC,
                "extendData":json_cpp(&extend),
                "rtcpAddress":chosen.get("rtcpaddr").or_else(||chosen.get("rtcp")).cloned().unwrap_or(json!("")),
                "rtpAddress":chosen.get("rtpaddr").or_else(||chosen.get("rtp")).cloned().unwrap_or(json!("")),
                "session":sess,"status":0
            }
        }));
        if let Some(media) = self.media.as_ref() {
            media.start();
        }
        self.start_call_camera();
        self.ui_state("connected", "connected");
    }

    fn reject_call(&mut self, reason: &str) {
        let Some(call) = self.call.as_ref() else {
            return;
        };
        if call.role != CallRole::Callee || call.phase != CallPhase::Incoming {
            return;
        }
        if reason == "no-device" {
            self.finish(0, None, false);
            return;
        }
        let (call_id, peer_id, sess) =
            (call.call_id, call.peer_id.clone(), call.session_id.clone());
        self.emit(json!({"type":"request","command":"action","data":{"id":8}}));
        self.emit(json!({
            "type":"sendSignal","command":402,"data":{
                "callId":call_id,"callerId":peer_id,"codec":EMPTY_CODEC,"extendData":json_cpp(&json!({})),
                "rtcpAddress":"","rtpAddress":"","session":sess,"status":3
            }
        }));
        self.finish(160, Some("ended"), true);
    }

    fn peer_ended(&mut self, reason: &str) {
        if let Some(call) = self.call.as_ref() {
            if call.phase == CallPhase::InCall {
                self.emit(json!({"type":"request","command":"showNotification","data":{
                    "isGroup":false,"reason":"partner_endcall","toUid":call.peer_id,"userId":call.peer_id
                }}));
                self.finish(50, Some("ended"), true);
            } else {
                self.log(&format!("peer ended during setup ({reason})"));
                self.finish(103, Some("ended"), true);
            }
        }
    }

    pub fn hangup(&mut self, reason: &str) {
        let Some(call) = self.call.as_ref() else {
            return;
        };
        if call.phase == CallPhase::Incoming {
            self.reject_call(reason);
            return;
        }
        let (call_id, peer_id, call_type, phase) = (
            call.call_id,
            call.peer_id.clone(),
            call.call_type,
            call.phase,
        );
        if phase == CallPhase::InCall {
            self.emit(
                json!({"type":"sendSignal","command":409,"data":{"callId":call_id,"toId":peer_id}}),
            );
            self.finish(50, Some("ended"), true);
        } else {
            self.emit(json!({"type":"sendSignal","command":405,"data":{
                "callId":call_id,"callType":if call_type == 3 {1} else {0},"status":0,"toId":peer_id
            }}));
            let status = match reason {
                "no-server" => "serverUnreachable",
                "timeout" if phase == CallPhase::Outgoing => "recipientUnreachable",
                "timeout" => "recipientNoAnswer",
                _ => "ended",
            };
            // Zalo's synchronized call-message path classifies 406/103 as a
            // missed call; 406/50 is a completed call and renders a green arrow.
            // Keep timeout duration at zero while preserving its missed-call status.
            self.finish(103, Some(status), true);
        }
    }

    fn finish(&mut self, status: i32, close_code: Option<&str>, signals: bool) {
        self.stop_camera_capture();
        self.stop_screen_capture();
        let Some(call) = self.call.take() else {
            return;
        };
        self.last_ended_call = Some(call.call_id);
        let connected = call.phase == CallPhase::InCall;
        let duration = if connected {
            elapsed_seconds_rounded(call.started_at)
        } else {
            0
        };
        let (rx_total, tx_total) = self
            .media
            .as_ref()
            .map(ZrtcMediaBackend::stats)
            .unwrap_or_default();
        if let Some(media) = self.media.as_ref() {
            media.stop();
        }
        if signals {
            let outgoing_timeout = call.role == CallRole::Caller
                && status == 103
                && matches!(
                    close_code,
                    Some("recipientUnreachable" | "recipientNoAnswer")
                );
            let missed_reason = match call.phase {
                CallPhase::Ringing => 2,  // TIMEOUT_HAVE_RINGING
                CallPhase::Outgoing => 5, // CALLER_REJECT_NOT_RINGING
                _ => 0,
            };
            self.emit(json!({"type":"update","command":"bubble","data":{
                "duration":duration,
                "partnerId":call.peer_id,
                "role":call.role == CallRole::Caller,
                "missed":outgoing_timeout,
                "reason":if outgoing_timeout {missed_reason} else {0},
                "callType":if call.call_type == 3 {1} else {0}
            }}));
            self.emit(json!({"type":"sendSignal","command":406,"data":{
                "callId":call.call_id,
                "data":json!({
                    "Codec":["opus/16000/1","20"],"NewZRTC":1,
                    "RxTotalPkt":rx_total,"SrtpMode":1,"TxTotalPkt":tx_total,"sysInfo":"native-engine"
                }).to_string(),
                "duration":duration,"partnerId":call.peer_id,
                "role":call.role == CallRole::Caller,"status":status
            }}));
        }
        self.emit(json!({"type":"update","command":"callState","data":{"state":"free"}}));
        self.phase = CallPhase::Idle;
        if let Some(code) = close_code {
            self.ui_message(
                json!({"type":"close","statusCode":code,"statusParams":{"duration":duration}}),
            );
        } else {
            self.ui_message(json!({"type":"close","statusCode":if connected {"endedWithDuration"} else {"ended"},"statusParams":{"duration":duration}}));
        }
        self.log(&format!(
            "call {} finished (status {status}, {duration}s)",
            call.call_id
        ));
    }

    fn on_alias_name(&mut self, data: &Value) {
        let alias = string_at(data, "aliasName").unwrap_or_default();
        let Some(call) = self.call.as_mut() else {
            return;
        };
        let noise_id = string_at(data, "noisedId").unwrap_or_default();
        if !alias.is_empty() && noise_id == call.peer_id && alias != call.peer_name {
            call.peer_name = alias;
            self.ui_state(self.ui_phase(), self.ui_status_code());
        }
    }

    fn on_ui_action(&mut self, action: UiAction) {
        match action.action.as_str() {
            "log" => {
                if let Some(message) = action.text.as_deref() {
                    let bounded: String = message.chars().take(512).collect();
                    self.log(&format!("[ui] {bounded}"));
                }
            }
            "accept" => {
                if self.group.is_some() {
                    self.group_answer(action.cam_start_off.unwrap_or(false));
                } else {
                    self.answer_call(action.cam_start_off.unwrap_or(false));
                }
            }
            "reject" => {
                if self.group.is_some() {
                    self.group_reject(action.why.as_deref().unwrap_or("reject"));
                } else {
                    self.reject_call(action.why.as_deref().unwrap_or("reject"));
                }
            }
            "hangup" => {
                if self.group.is_some() {
                    self.group_hangup("local");
                } else {
                    self.hangup("local");
                }
            }
            "micAutoGain" => {
                if let Some(on) = action.on {
                    if let Some(media) = self.media.as_ref() {
                        media.set_mic_auto_gain(on);
                    }
                    self.sync_ui_state();
                }
            }
            "micVolume" => {
                if let Some(value) = action.value {
                    if value.is_finite() {
                        if let Some(media) = self.media.as_ref() {
                            media.set_mic_volume(value.round().clamp(0.0, 100.0) as u8);
                        }
                    }
                }
            }
            "mute" => {
                if let Some(on) = action.on {
                    if let Some(call) = self.call.as_mut() {
                        call.muted = on;
                    }
                    if let Some(group) = self.group.as_mut() {
                        group.muted = on;
                    }
                    if let Some(media) = self.media.as_ref() {
                        media.set_mute(on);
                    }
                    if self.group.is_some() {
                        self.group_broadcast(12098);
                        self.group_update_status();
                    } else {
                        self.sync_ui_state();
                        self.mic_state(!on);
                    }
                }
            }
            "speaker" => {
                if let Some(on) = action.on {
                    if let Some(call) = self.call.as_mut() {
                        call.speaker_off = on;
                    }
                    if let Some(group) = self.group.as_mut() {
                        group.speaker_off = on;
                    }
                    if let Some(media) = self.media.as_ref() {
                        media.set_speaker_off(on);
                    }
                    if self.group.is_some() {
                        self.group_update_status();
                    } else {
                        self.sync_ui_state();
                    }
                }
            }
            "camera" => {
                if self.group.is_some() {
                    if let Some(on) = action.on {
                        let effective_on = if on {
                            self.start_camera_capture()
                        } else {
                            self.stop_camera_capture();
                            false
                        };
                        if let Some(group) = self.group.as_mut() {
                            group.camera_on = effective_on;
                            group.camera_was_on = false;
                        }
                        if let Some(media) = self.media.as_ref() {
                            if let Err(error) = media.set_group_camera(effective_on) {
                                self.log(&format!("group camera state update failed: {error}"));
                            }
                            if let Err(error) = media.send_group_video_quality_config() {
                                self.log(&format!("group VidQualConfig failed: {error}"));
                            }
                        }
                        self.group_broadcast(12098);
                        self.group_update_status();
                        if effective_on {
                            if let Some(camera) = self.camera.as_ref() {
                                camera.request_keyframe();
                            }
                        }
                    }
                    return;
                }
                if let Some(on) = action.on {
                    let screen_sharing = self.call.as_ref().is_some_and(|call| call.sharing_screen);
                    let effective_on = if on && !screen_sharing {
                        self.start_camera_capture()
                    } else {
                        self.stop_camera_capture();
                        false
                    };
                    if let Some(call) = self.call.as_mut() {
                        call.camera_on = effective_on;
                        call.camera_was_on = false;
                    }
                    self.sync_ui_state();
                    self.camera_state(effective_on);
                    if effective_on {
                        if let Some(camera) = self.camera.as_ref() {
                            camera.request_keyframe();
                        }
                    }
                }
            }
            "listScreenSources" => {
                if crate::screen::is_x11_session() {
                    match crate::screen::list_x11_windows() {
                        Ok(windows) => self.ui_message(json!({
                            "type":"screenSources",
                            "windows":windows.iter().map(|(id, label)| json!({"id":id,"label":label})).collect::<Vec<_>>()
                        })),
                        Err(error) => self.ui_message(json!({"type":"screenError","message":error})),
                    }
                } else {
                    self.log("window source requested on Wayland; delegating selection to XDG ScreenCast portal");
                    self.ui_message(json!({"type":"screenSourceMode","source":"window"}));
                }
            }
            "screen" => {
                if let Some(on) = action.on {
                    if on {
                        let group_config = self
                            .group
                            .as_ref()
                            .filter(|group| group.phase == GroupPhase::Connected)
                            .map(|group| group.config.clone());
                        let in_call = self.call.as_ref().is_some_and(|call| {
                            call.phase == CallPhase::InCall && call.call_type == 3
                        });
                        self.log(&format!(
                            "screen capture request received (video_call={in_call}, group_call={})",
                            group_config.is_some()
                        ));
                        if !in_call && group_config.is_none() {
                            self.log("screen capture rejected: no active video/group call");
                            self.ui_message(json!({
                                "type":"screenError",
                                "message":"Screen sharing requires an active call",
                                "requestId":action.request_id
                            }));
                        } else {
                            if self.screen.as_ref().is_some_and(ScreenCapture::is_finished) {
                                self.screen.take();
                            }
                            if self.screen.as_ref().is_some_and(ScreenCapture::is_stopping) {
                                // A user can retry while the cancelled portal
                                // request is still closing its session. Queue the
                                // latest request and start it after worker exit.
                                self.screen_restart_pending = true;
                                self.screen_restart_source = action.source.clone();
                                self.screen_restart_request_id = action.request_id.clone();
                                self.log("queued screen-share retry until previous capture exits");
                                return;
                            }
                            if self.screen.is_some() {
                                self.log(
                                    "screen capture rejected: a capture worker is already active",
                                );
                                self.ui_message(json!({
                                    "type":"screenError",
                                    "message":"Screen capture is already active",
                                    "requestId":action.request_id
                                }));
                                return;
                            }
                            self.start_screen_capture(
                                action.source.clone(),
                                action.request_id.clone(),
                            );
                        }
                    } else {
                        self.screen_restart_pending = false;
                        self.screen_restart_source = None;
                        self.screen_restart_request_id = None;
                        let request_id = action
                            .request_id
                            .clone()
                            .or_else(|| self.screen_request_id.clone());
                        self.screen_stop_ack_pending = self.screen.is_some();
                        self.stop_screen_capture();
                        let call_data = self.call.as_mut().and_then(|call| {
                            if !call.sharing_screen {
                                return None;
                            }
                            call.sharing_screen = false;
                            Some(json!({
                                "callId":call.call_id,
                                "status":0,
                                "uidTo":call.peer_id
                            }))
                        });
                        if let Some(data) = call_data {
                            self.emit(json!({"type":"sendSignal","command":12064,"data":data}));
                            self.restore_call_camera_after_screen_share();
                        }
                        self.ui_message(json!({"type":"screenStopped","requestId":request_id}));
                    }
                }
            }
            "device" => {
                if let (Some(kind), Some(id)) = (action.kind.as_deref(), action.id.as_deref()) {
                    if let Err(error) = crate::devices::set_device(kind, id) {
                        self.log(&format!("could not persist device selection: {error}"));
                    }
                    if let Some(media) = self.media.as_ref() {
                        media.set_device(kind, id);
                    }
                    if kind == "camera" {
                        let restart = self.call.as_ref().is_some_and(|call| {
                            call.phase == CallPhase::InCall
                                && call.camera_on
                                && !call.sharing_screen
                        }) || self.group.as_ref().is_some_and(|group| {
                            group.phase == GroupPhase::Connected && group.camera_on
                        });
                        if restart {
                            self.stop_camera_capture();
                            if self.start_camera_capture() {
                                self.log("camera source switched in the active call");
                            } else {
                                self.camera_state(false);
                            }
                        }
                    }
                }
            }
            "listDevices" => {
                let devices = crate::devices::list_devices();
                self.ui_message(json!({
                    "type":"devices",
                    "requestId":action.request_id,
                    "data":devices
                }));
            }
            "zaloListDevices" => {
                let devices = crate::devices::zalo_device_list();
                self.ui_message(json!({
                    "type":"zaloDeviceList",
                    "requestId":action.request_id,
                    "data":devices
                }));
            }
            "zaloDeviceConfig" => {
                if let Some(data) = action.data.as_ref() {
                    let previous_camera =
                        crate::devices::load_prefs().camera.map(|camera| camera.id);
                    if let Err(error) = crate::devices::update_from_zalo(data) {
                        self.log(&format!("could not persist Zalo device config: {error}"));
                    }
                    let prefs = crate::devices::load_prefs();
                    let (microphone, speaker) = zalo_audio_device_selection(data, &prefs);
                    if let Some(media) = self.media.as_ref() {
                        media.set_device("mic", &microphone);
                        media.set_device("speaker", &speaker);
                    }

                    let current_camera = prefs.camera.map(|camera| camera.id);
                    let camera_changed = previous_camera != current_camera;
                    let camera_active = self.call.as_ref().is_some_and(|call| {
                        call.phase == CallPhase::InCall && call.camera_on && !call.sharing_screen
                    }) || self.group.as_ref().is_some_and(|group| {
                        group.phase == GroupPhase::Connected && group.camera_on
                    });
                    if camera_changed && camera_active {
                        self.stop_camera_capture();
                        let camera_on = self.start_camera_capture();
                        if let Some(call) = self.call.as_mut() {
                            call.camera_on = camera_on;
                        }
                        if let Some(group) = self.group.as_mut() {
                            group.camera_on = camera_on;
                        }
                        if self.group.is_some() {
                            if let Some(media) = self.media.as_ref() {
                                let _ = media.set_group_camera(camera_on);
                            }
                            self.group_broadcast(12098);
                            self.group_update_status();
                        } else {
                            self.sync_ui_state();
                            self.camera_state(camera_on);
                        }
                    }
                }
            }
            "advancedOptions" => {
                if let Some(data) = action.data.as_ref() {
                    if let Err(error) = crate::devices::update_advanced_options(data) {
                        self.log(&format!("could not persist call options: {error}"));
                    }
                }
            }
            "tile" => {
                if let (Some(src), Some(width)) = (action.src, action.width) {
                    if let Some(media) = self.media.as_ref() {
                        if let Err(error) = media.set_group_member_render_width(src, width) {
                            self.log(&format!("group tile quality update failed: {error}"));
                        }
                    }
                }
            }
            "needkey" => {
                if let Some(media) = self.media.as_ref() {
                    if let Err(error) = media.request_peer_keyframe(action.src) {
                        self.log(&format!("could not send peer keyframe request: {error}"));
                    }
                }
            }
            "keyframe" => {
                let sharing_screen = self.call.as_ref().is_some_and(|call| call.sharing_screen)
                    || self
                        .group
                        .as_ref()
                        .is_some_and(|group| group.sharing_screen);
                if sharing_screen {
                    if let Some(screen) = self.screen.as_ref() {
                        screen.request_keyframe();
                    }
                } else if let Some(camera) = self.camera.as_ref() {
                    camera.request_keyframe();
                }
            }
            _ => {}
        }
    }

    #[cfg(test)]
    fn start_camera_capture(&mut self) -> bool {
        // Engine unit tests assert signaling/state transitions, not exclusive
        // access to the user's physical V4L2 device. Real capture is covered by
        // the explicitly ignored camera integration test.
        true
    }

    #[cfg(not(test))]
    fn start_camera_capture(&mut self) -> bool {
        if self.camera.is_some() {
            return true;
        }
        let device = crate::devices::load_prefs()
            .camera
            .map(|preference| preference.id)
            .filter(|value| !value.is_empty());
        let profile = self
            .media
            .as_ref()
            .and_then(ZrtcMediaBackend::video_profile);
        let (width, height, fps, bitrate) = profile
            .as_ref()
            .map(|profile| (profile.width, profile.height, profile.fps, profile.bitrate))
            .unwrap_or((640, 360, 15, 500_000));
        match CameraCapture::start(device.as_deref(), width, height, fps, bitrate) {
            Ok(capture) => {
                self.camera = Some(capture);
                self.log("native V4L2/OpenH264 camera capture started");
                if let Some(media) = self.media.as_ref() {
                    media.note_debug_event("camera-capture started");
                }
                true
            }
            Err(error) => {
                self.log(&format!("camera capture unavailable: {error}"));
                if let Some(media) = self.media.as_ref() {
                    media.note_debug_event("camera-capture failed");
                }
                if let Some(call) = self.call.as_mut() {
                    call.camera_on = false;
                }
                if let Some(group) = self.group.as_mut() {
                    group.camera_on = false;
                }
                if let Some(media) = self.media.as_ref() {
                    let _ = media.set_group_camera(false);
                }
                self.ui_message(json!({"type":"cameraError","message":error}));
                false
            }
        }
    }

    fn start_call_camera(&mut self) {
        let Some(wanted) = self.call.as_ref().and_then(|call| {
            (call.phase == CallPhase::InCall && call.call_type == 3).then_some(call.camera_on)
        }) else {
            return;
        };
        let camera_on = if wanted {
            self.start_camera_capture()
        } else {
            self.stop_camera_capture();
            false
        };
        if let Some(call) = self.call.as_mut() {
            call.camera_on = camera_on;
            call.camera_state_reassert_at = Some(Instant::now() + Duration::from_secs(2));
        }
        self.camera_state(camera_on);
    }

    fn restore_call_camera_after_screen_share(&mut self) {
        let restore_camera = self.call.as_ref().is_some_and(|call| call.camera_was_on);
        let camera_on = if restore_camera {
            self.start_camera_capture()
        } else {
            self.stop_camera_capture();
            false
        };
        if let Some(call) = self.call.as_mut() {
            call.sharing_screen = false;
            call.camera_on = camera_on;
            call.camera_was_on = false;
        }
        self.camera_state(camera_on);
        self.sync_ui_state();
    }

    fn stop_camera_capture(&mut self) {
        if let Some(capture) = self.camera.take() {
            capture.stop();
            self.log("native camera capture stopped");
        }
    }

    fn group_share_response(&mut self, data: &Value) {
        let Some(current_call_id) = self.group.as_ref().map(|group| group.call_id) else {
            return;
        };
        if number_at(data, "callId").is_some_and(|id| id as u32 != current_call_id) {
            self.log("ignoring group screen-share response for another call");
            return;
        }
        let mut response = parse_json_value(data.get("data"));
        merge_object(&mut response, &parse_json_value(data.get("params")));
        let nested_params = parse_json_value(response.get("params"));
        merge_object(&mut response, &nested_params);
        let share_uid = number_at(&response, "shareScreenId")
            .or_else(|| number_at(data, "shareScreenId"))
            .unwrap_or(0)
            .max(0) as u32;
        if share_uid == 0 {
            return;
        }
        if let Some(media) = self.media.as_ref() {
            media.set_group_share_remote_uid(share_uid);
        }
        let (should_start, call_id, host_call, local_uid, audio_state, video_state) = {
            let Some(group) = self.group.as_mut() else {
                return;
            };
            let changed = group.share_screen_id != share_uid;
            group.share_screen_id = share_uid;
            group.members.remove(&share_uid.to_string());
            (
                group.sharing_screen && changed,
                group.call_id,
                group.host_call,
                group.local_uid,
                if group.muted { 1 } else { 0 },
                if group.camera_on { 0 } else { 1 },
            )
        };
        if should_start {
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.start_group_share(share_uid) {
                    self.log(&format!("could not start group share peer: {error}"));
                    self.stop_screen_capture();
                    self.ui_message(json!({"type":"screenError","message":error}));
                    return;
                }
            }
            self.emit(json!({
                "type":"sendSignal","command":12044,"data":{
                    "audioState":audio_state,
                    "callId":call_id,
                    "data":json_cpp(&json!({"shareScreenId":share_uid})),
                    "hostCall":host_call,
                    "status":1,
                    "userId":local_uid,
                    "videoState":video_state
                }
            }));
            self.log(&format!("group share peer {share_uid} requested"));
        }
        self.group_update_status();
    }

    fn stop_group_screen_share(&mut self) {
        let stopped = self.group.as_mut().and_then(|group| {
            if !group.sharing_screen {
                return None;
            }
            group.sharing_screen = false;
            group.members.remove(&group.share_screen_id.to_string());
            let stop_signal = json!({
                "audioState":if group.muted {1} else {0},
                "callId":group.call_id,
                "data":json_cpp(&json!({"shareScreenId":group.share_screen_id})),
                "hostCall":group.host_call,
                "status":0,
                "userId":group.local_uid,
                "videoState":if group.camera_on {0} else {1}
            });
            let restore_camera = group.camera_was_on;
            group.camera_was_on = false;
            Some((stop_signal, restore_camera))
        });

        let Some((data, restore_camera)) = stopped else {
            return;
        };

        if let Some(media) = self.media.as_ref() {
            media.stop_group_share();
            media.set_group_share_remote_uid(0);
        }
        self.emit(json!({"type":"sendSignal","command":12044,"data":data}));

        let camera_on = if restore_camera {
            self.start_camera_capture()
        } else {
            self.stop_camera_capture();
            false
        };
        if let Some(group) = self.group.as_mut() {
            group.camera_on = camera_on;
        }
        if let Some(media) = self.media.as_ref() {
            if let Err(error) = media.set_group_camera(camera_on) {
                self.log(&format!("group camera restore failed: {error}"));
            }
            if let Err(error) = media.send_group_video_quality_config() {
                self.log(&format!(
                    "group VidQualConfig after screen share failed: {error}"
                ));
            }
        }
        self.group_broadcast(12098);
        self.group_update_status();
    }

    fn activate_screen_share(&mut self) {
        if self.group.is_some() {
            let details = self.group.as_mut().and_then(|group| {
                if group.phase != GroupPhase::Connected {
                    return None;
                }
                group.sharing_screen = true;
                group.camera_was_on = group.camera_on;
                // The legacy renderer stopped its camera track first, then
                // requested the share, then reasserted video-on. Preserve that
                // signaling order while Rust owns the actual capture worker.
                group.camera_on = false;
                Some((
                    group.share_screen_id,
                    group.call_id,
                    group.host_call,
                    group.local_uid,
                    group.camera_was_on,
                    if group.muted { 1 } else { 0 },
                    1,
                ))
            });
            let Some((
                share_uid,
                call_id,
                host_call,
                local_uid,
                camera_was_on,
                audio_state,
                video_state,
            )) = details
            else {
                self.stop_screen_capture();
                return;
            };
            self.stop_camera_capture();
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.set_group_camera(false) {
                    self.log(&format!(
                        "group camera pause for screen share failed: {error}"
                    ));
                }
                if let Err(error) = media.send_group_video_quality_config() {
                    self.log(&format!(
                        "group VidQualConfig before screen share failed: {error}"
                    ));
                }
            }
            self.group_broadcast(12098);
            self.emit(json!({
                "type":"sendSignal","command":12044,"data":{
                    "audioState":audio_state,
                    "callId":call_id,
                    "data":json_cpp(&json!({"shareScreenId":share_uid})),
                    "hostCall":host_call,
                    "status":1,
                    "userId":local_uid,
                    "videoState":video_state
                }
            }));
            if let Some(group) = self.group.as_mut() {
                group.camera_on = true;
            }
            if let Some(media) = self.media.as_ref() {
                if let Err(error) = media.set_group_camera(true) {
                    self.log(&format!(
                        "group camera video-on during screen share failed: {error}"
                    ));
                }
                if let Err(error) = media.send_group_video_quality_config() {
                    self.log(&format!(
                        "group VidQualConfig during screen share failed: {error}"
                    ));
                }
            }
            self.group_broadcast(12098);
            if share_uid > 0 {
                if let Some(media) = self.media.as_ref() {
                    if let Err(error) = media.start_group_share(share_uid) {
                        self.log(&format!("could not start group share peer: {error}"));
                        self.stop_screen_capture();
                        self.ui_message(json!({"type":"screenError","message":error,"requestId":self.screen_request_id}));
                    }
                }
            }
            self.group_update_status();
            self.ui_message(json!({"type":"screenStarted","cameraWasOn":camera_was_on,"requestId":self.screen_request_id}));
            self.log("native group screen capture started");
            return;
        }
        self.stop_camera_capture();
        let (camera_was_on, data) = self.call.as_mut().map_or((false, None), |call| {
            if call.phase != CallPhase::InCall {
                return (false, None);
            }
            let camera_was_on = call.camera_on;
            call.camera_was_on = camera_was_on;
            call.camera_on = false;
            (
                camera_was_on,
                Some(json!({
                    "callId":call.call_id,
                    "status":1,
                    "uidTo":call.peer_id
                })),
            )
        });
        if let Some(data) = data {
            // Match the legacy order: stop the camera bit first (when it was
            // active), start screen share, then keep the peer-visible video bit
            // on while screen frames replace camera frames.
            if camera_was_on {
                self.camera_state(false);
            }
            if let Some(call) = self.call.as_mut() {
                call.sharing_screen = true;
            }
            self.emit(json!({"type":"sendSignal","command":12064,"data":data}));
            self.camera_state(true);
            self.sync_ui_state();
            self.ui_message(json!({
                "type":"screenStarted",
                "cameraWasOn":camera_was_on,
                "requestId":self.screen_request_id
            }));
            self.log("native screen capture started");
        }
    }

    fn start_screen_capture(&mut self, source: Option<String>, request_id: Option<String>) {
        self.screen_request_id = request_id.clone();
        let group_config = self
            .group
            .as_ref()
            .filter(|group| group.phase == GroupPhase::Connected)
            .map(|group| group.config.clone());
        let in_call = self
            .call
            .as_ref()
            .is_some_and(|call| call.phase == CallPhase::InCall && call.call_type == 3);
        if !in_call && group_config.is_none() {
            self.ui_message(json!({
                "type":"screenError",
                "message":"Screen sharing requires an active call",
                "requestId":request_id
            }));
            return;
        }
        let x11_session = crate::screen::is_x11_session();
        let source = match (x11_session, source.as_deref()) {
            (true, Some("monitor")) => Some("screen:0".to_string()),
            (true, Some("window")) => return self.ui_message(json!({"type":"screenError","message":"Window selection requires choosing an X11 window first","requestId":request_id})),
            (false, Some("screen:0")) => Some("monitor".to_string()),
            (false, Some("window:select")) => Some("window".to_string()),
            (_, value) => value.map(str::to_owned),
        };
        self.log(&format!("starting screen capture: backend={}, source={:?}, request_id={:?}, call_phase={:?}, call_type={:?}", if x11_session {"x11"} else {"wayland-portal"}, source, request_id, self.call.as_ref().map(|call| call.phase), self.call.as_ref().map(|call| call.call_type)));
        let screen = if let Some(config) = group_config {
            let bitrate = number_at(&config, "maxShrScreenBr")
                .filter(|value| *value > 0)
                .unwrap_or(1500) as u32
                * 1000;
            let max_short = std::env::var("ZCALL_SHARE_RES")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(720);
            ScreenCapture::start_with_source(30, bitrate, max_short, source)
        } else {
            ScreenCapture::start_with_source(30, 1_200_000, 0, source)
        };
        match screen {
            Ok(screen) => {
                self.screen = Some(screen);
                self.log(
                    "screen-capture worker spawned; notifying UI to compact after Rust acceptance",
                );
                self.ui_message(json!({"type":"screenCaptureAccepted","requestId":request_id}));
            }
            Err(error) => {
                self.log(&format!("screen-capture worker spawn failed: {error}"));
                self.ui_message(
                    json!({"type":"screenError","message":error,"requestId":request_id}),
                );
            }
        }
    }

    fn stop_screen_capture(&mut self) {
        self.screen_restart_pending = false;
        self.screen_restart_source = None;
        if self
            .group
            .as_ref()
            .is_some_and(|group| group.sharing_screen)
        {
            self.stop_group_screen_share();
        }
        if let Some(screen) = self.screen.as_ref() {
            screen.stop();
            self.log("native screen capture stop requested");
        }
    }

    fn camera_state(&mut self, camera_on: bool) {
        let Some(call) = self.call.as_ref() else {
            return;
        };
        if call.phase != CallPhase::InCall || call.call_type != 3 {
            return;
        }
        let camera_on = camera_on || call.sharing_screen;
        self.emit(json!({"type":"sendSignal","command":12006,"data":{
            "callId":call.call_id,"calleeId":call.peer_id,"data":"",
            "isOnCam":if camera_on {1} else {0},"isOnMic":-1
        }}));
    }

    fn mic_state(&mut self, mic_on: bool) {
        let Some(call) = self.call.as_ref() else {
            return;
        };
        if call.phase != CallPhase::InCall {
            return;
        }
        self.emit(json!({"type":"sendSignal","command":12006,"data":{
            "callId":call.call_id,"calleeId":call.peer_id,"data":"",
            "isOnCam":-1,"isOnMic":if mic_on {0} else {1}
        }}));
    }

    fn ui_state_value(&self, phase: &str, status: &str, call: &Call) -> Value {
        let mut state = json!({
            "type":"state","phase":phase,"statusCode":status,
            "name":call.peer_name,"avatar":call.avatar,"video":call.call_type == 3,
            "muted":call.muted,"speakerOff":call.speaker_off,
            "peerMuted":call.peer_muted,"peerCamOff":call.peer_camera_off,
            "peerSharing":call.peer_sharing,"sharing":call.sharing_screen,
            "cameraOn":call.camera_on,"cameraWasOn":call.camera_was_on,
            "micVolume":self.media.as_ref().map(ZrtcMediaBackend::mic_volume).unwrap_or(100),
            "micAutoGain":self.media.as_ref().is_none_or(ZrtcMediaBackend::mic_auto_gain)
        });
        if let Some(started_at) = call.started_at {
            // Keep the legacy UI contract: since is an epoch-millisecond
            // timestamp, not an elapsed duration.
            state["since"] = json!(instant_to_unix_ms(started_at));
        }
        state
    }

    fn ui_peer_state(&self) {
        if let Some(call) = self.call.as_ref() {
            self.ui_message(self.ui_state_value(self.ui_phase(), self.ui_status_code(), call));
        }
    }

    fn sync_ui_state(&self) {
        if let Some(call) = self.call.as_ref() {
            self.ui_message(self.ui_state_value(self.ui_phase(), self.ui_status_code(), call));
        }
    }

    fn ui_state(&self, phase: &str, status: &str) {
        if let Some(call) = self.call.as_ref() {
            self.ui_message(self.ui_state_value(phase, status, call));
        }
    }

    fn ui_status(&self, status: &str, _params: Option<Value>) {
        if self.call.is_some() {
            self.ui_state(self.ui_phase(), status);
        }
    }

    fn ui_phase(&self) -> &'static str {
        match self.phase {
            CallPhase::Idle => "ended",
            CallPhase::Outgoing => "outgoing",
            CallPhase::Ringing => "outgoing",
            CallPhase::Incoming => "incoming",
            CallPhase::Connecting => "connecting",
            CallPhase::InCall => "connected",
        }
    }

    fn ui_status_code(&self) -> &'static str {
        match self.phase {
            CallPhase::Idle => "ended",
            CallPhase::Outgoing => "calling",
            CallPhase::Ringing => "ringing",
            CallPhase::Incoming => "incomingVoice",
            CallPhase::Connecting => "connecting",
            CallPhase::InCall => "connected",
        }
    }

    fn ui_message(&self, message: Value) {
        if let Some(ui) = self.ui.as_ref() {
            ui.send_message(&message);
        }
    }

    fn emit(&mut self, frame: Value) {
        self.outgoing_frames.push(frame);
    }

    fn log(&self, message: &str) {
        crate::zcall_log!("{message}");
    }
}

fn parse_json_value(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(text)) => serde_json::from_str(text).unwrap_or_else(|_| json!({})),
        Some(Value::Object(_)) => value.cloned().unwrap_or_else(|| json!({})),
        _ => json!({}),
    }
}

fn merge_object(target: &mut Value, update: &Value) {
    if !target.is_object() {
        *target = json!({});
    }
    if let (Some(t), Some(u)) = (target.as_object_mut(), update.as_object()) {
        for (key, value) in u {
            t.insert(key.clone(), value.clone());
        }
    }
}

fn string_at(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str().map(str::to_owned))
}

fn value_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

fn number_at(value: &Value, key: &str) -> Option<i64> {
    value
        .get(key)
        .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
}

fn number_value(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn parse_server_addresses(servers: &[Value]) -> Vec<SocketAddr> {
    servers
        .iter()
        .filter_map(|server| {
            let addr = string_at(server, "rtpaddr")
                .or_else(|| string_at(server, "rtp"))
                .or_else(|| string_at(server, "rtpAddress"))?;
            addr.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4)
        })
        .collect()
}

fn select_server(servers: &[Value], chosen: Option<SocketAddr>) -> Option<Value> {
    if let Some(chosen) = chosen {
        for server in servers {
            let address = string_at(server, "rtpaddr")
                .or_else(|| string_at(server, "rtp"))
                .or_else(|| string_at(server, "rtpAddress"));
            if address
                .as_deref()
                .and_then(|s| s.to_socket_addrs().ok())
                .is_some_and(|list| list.into_iter().any(|addr| addr == chosen))
            {
                return Some(server.clone());
            }
        }
    }
    servers.first().cloned()
}

fn extend_data(
    server: &Value,
    call_type: u8,
    callee: bool,
    p2p: Vec<Value>,
    enable_p2p: bool,
) -> Value {
    let rtp = server
        .get("rtpaddr")
        .or_else(|| server.get("rtp"))
        .cloned()
        .unwrap_or(json!(""));
    let rtcp = server
        .get("rtcpaddr")
        .or_else(|| server.get("rtcp"))
        .cloned()
        .unwrap_or(json!(""));
    json!({
        "callType":if call_type == 3 {1} else {0},"fecTP":0,"gccAudio":1,"gccEarlyCall":0,"gccMode":1,
        "gccSVLR":1,"maxFT":60,"newZrtc":1,"p2p":p2p,"packetMode":2,"platform":2,
        "sP2P":if enable_p2p {1} else {0},"serverAddr":[{
            "rtcp":rtcp,
            "rtcpIPv6":string_at(server,"rtcpIPv6").unwrap_or_default(),
            "rtp":rtp,
            "rtpIPv6":string_at(server,"rtpIPv6").unwrap_or_default(),
            "tpType":0
        }],
        "spTcp":1,"srtcp":0,"srtpMode":1,"supportCallBusy":if callee {0} else {1},
        "supportHevcDecode":0,"tpType":0,"select2side":if enable_p2p {1} else {0},
        "video":{"codec":[{"name":"h264","payload":97}]}
    })
}

fn parse_p2p_candidates(value: Option<&Value>) -> Vec<SocketAddr> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|candidate| {
            let ip = string_at(candidate, "ip")?;
            let port = number_at(candidate, "port")?;
            if port <= 0 || port > u16::MAX as i64 {
                return None;
            }
            format!("{ip}:{port}").parse().ok()
        })
        .filter(SocketAddr::is_ipv4)
        .collect()
}

fn json_cpp(value: &Value) -> String {
    // serde_json::Map is ordered by key in the default build, matching JsonCpp.
    let mut text = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    text.push('\n');
    text
}

fn instant_to_unix_ms(instant: Instant) -> u64 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    now_ms.saturating_sub(instant.elapsed().as_millis() as u64)
}

fn elapsed_seconds_rounded(started_at: Option<Instant>) -> u64 {
    started_at
        .map(|instant| instant.elapsed().as_secs_f64().round() as u64)
        .unwrap_or(0)
}

fn new_call_id() -> u32 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    1_000_000_000 + (millis % 1_147_483_646) as u32
}

fn zalo_audio_device_selection(
    data: &Value,
    preferences: &crate::devices::DevicePrefs,
) -> (String, String) {
    let microphone = if data.get("autoAudioInput").and_then(Value::as_bool) == Some(true) {
        String::new()
    } else {
        data.get("defaultAudInputDevice")
            .and_then(|device| device.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| preferences.mic.clone())
    };
    let speaker = if data.get("autoAudioOutput").and_then(Value::as_bool) == Some(true) {
        String::new()
    } else {
        data.get("defaultAudOutputDevice")
            .and_then(|device| device.get("id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| preferences.speaker.clone())
    };
    (microphone, speaker)
}

#[cfg(test)]
mod tests {
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
            call_id: 1,
            peer_id: "peer".into(),
            peer_uid: 2,
            local_uid: 1,
            peer_name: "Peer".into(),
            avatar: String::new(),
            call_type: 3,
            session_id: String::new(),
            server: None,
            servers: Vec::new(),
            p2p_candidates: Vec::new(),
            media_config: Value::Null,
            deadline: None,
            started_at: Some(Instant::now() - Duration::from_secs(3)),
            muted: false,
            speaker_off: false,
            camera_on: true,
            camera_was_on: true,
            camera_state_reassert_at: None,
            peer_muted: false,
            peer_camera_off: false,
            peer_sharing: false,
            sharing_screen: false,
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
            call.deadline = Some(Instant::now() - Duration::from_millis(1));
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
        let started_at = Some(Instant::now() - Duration::from_millis(1_600));
        assert_eq!(elapsed_seconds_rounded(started_at), 2);
    }

    #[test]
    fn ignores_peer_camera_state_for_a_stale_call_id() {
        let mut engine = engine();
        engine.call = Some(test_call(CallPhase::InCall));

        engine.on_zalo_frame(json!({
            "type":"control",
            "data":{
                "act":"onoff_camera",
                "data":{"callId":2,"params":{"type":0}}
            }
        }));

        assert!(!engine.call.as_ref().unwrap().peer_camera_off);
    }

    #[test]
    fn stale_answer_with_string_call_id_does_not_connect_current_call() {
        let mut engine = engine();
        engine.call = Some(test_call(CallPhase::Outgoing));
        engine.phase = CallPhase::Outgoing;

        engine.on_zalo_frame(json!({
            "type":"control",
            "data":{
                "act":"answer",
                "data":{"callId":"2","status":0,"params":{"rtpSerIp":"127.0.0.1"}}
            }
        }));

        assert_eq!(engine.call.as_ref().unwrap().phase, CallPhase::Outgoing);
        assert!(engine.poll_outgoing_frames().is_empty());
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
        engine.on_zalo_frame(json!({
            "type":"update","command":"init","data":{"local":{"id":111}}
        }));
        engine.on_zalo_frame(json!({
            "type":"request","command":"makeCall",
            "data":{"type":1,"partner":[{"id":"peer-123","name":"Peer","avatar":"avatar"}]}
        }));
        let initial = engine.poll_outgoing_frames();
        let call_id = command(&initial, "sendSignal", 401)["data"]["callId"]
            .as_u64()
            .unwrap();

        engine.on_zalo_frame(json!({
            "type":"recvSignal","command":401,"data":{
                "status":0,"sessId":"012345678901234567890123456789",
                "fromId":111,"toId":222,
                "servers":[{"rtpaddr":"127.0.0.1:9000","rtcpaddr":"127.0.0.1:9001"}]
            }
        }));
        engine.on_zalo_frame(json!({
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
    fn outgoing_call_negotiates_relay_and_finishes_over_call_v2() {
        let mut engine = engine();
        engine.on_zalo_frame(json!({
            "type":"update","command":"init","data":{"local":{"id":111}}
        }));
        engine.on_zalo_frame(json!({
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

        engine.on_zalo_frame(json!({
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

        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"ring_ring","data":{"callId":call_id}}
        }));
        engine.on_zalo_frame(json!({
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

        engine.on_zalo_frame(json!({
            "type":"recvSignal","command":409,"data":{"callId":call_id + 1}
        }));
        assert_eq!(
            engine.phase,
            CallPhase::InCall,
            "a late end signal for an old call must be ignored"
        );
        assert_eq!(
            engine.call.as_ref().map(|call| call.call_id),
            Some(call_id as u32)
        );

        engine.on_zalo_frame(json!({"type":"request","command":"endCall","data":{}}));
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
                let call_id = call.call_id;
                engine.call = Some(call);
                engine.phase = phase;

                engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
            "type":"update","command":"init","data":{"local":{"id":111}}
        }));
        engine.on_zalo_frame(json!({
            "type":"request","command":"makeCall",
            "data":{"type":3,"partner":[{"id":"peer-video","name":"Peer"}]}
        }));
        let initial = engine.poll_outgoing_frames();
        assert_eq!(command(&initial, "sendSignal", 12006)["data"]["isOnCam"], 0);
        let call_id = command(&initial, "sendSignal", 401)["data"]["callId"]
            .as_u64()
            .unwrap();

        engine.on_zalo_frame(json!({
            "type":"recvSignal","command":401,"data":{
                "status":0,"sessId":"012345678901234567890123456789",
                "fromId":111,"toId":222,
                "servers":[{"rtpaddr":"127.0.0.1:9000","rtcpaddr":"127.0.0.1:9001"}]
            }
        }));
        engine.poll_outgoing_frames();
        engine.on_zalo_frame(json!({
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
            call.camera_on = true;
            call.camera_state_reassert_at = Some(Instant::now() - Duration::from_millis(1));
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
        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"request","data":{
                "callId":654321,"uidN":"caller-video","uidFrom":222,"uidTo":111,
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
        assert!(engine.call.as_ref().unwrap().camera_on);
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
        engine.on_zalo_frame(json!({
            "type":"control","data":{
                "act":"request",
                "data":{
                    "callId":123456,"uidN":"caller-zalo","uidFrom":222,"uidTo":111,
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
            123456
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
        assert_eq!(command(&ended, "sendSignal", 409)["data"]["callId"], 123456);
        assert_eq!(command(&ended, "sendSignal", 406)["data"]["status"], 50);
        assert_eq!(engine.phase, CallPhase::Idle);
    }

    #[test]
    fn screen_share_is_signalled_only_after_native_capture_is_ready() {
        let mut engine = engine();
        engine.on_zalo_frame(json!({
            "type":"control","data":{
                "act":"request","data":{
                    "callId":123456,"uidN":"caller-zalo","uidFrom":222,"uidTo":111,
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
            call.call_type = 3;
            call.camera_on = true;
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
        assert_eq!(share["data"]["callId"], 123456);
        assert_eq!(share["data"]["uidTo"], "caller-zalo");
        assert_eq!(share["data"]["status"], 1);
        let camera_states = frames
            .iter()
            .filter(|frame| frame["type"] == "sendSignal" && frame["command"] == 12006)
            .map(|frame| frame["data"]["isOnCam"].as_i64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(camera_states, vec![0, 1]);
        assert!(engine.call.as_ref().unwrap().sharing_screen);
        assert!(engine.call.as_ref().unwrap().camera_was_on);
        assert!(!engine.call.as_ref().unwrap().camera_on);
    }

    #[test]
    fn screen_share_keeps_peer_video_enabled_when_camera_was_off() {
        let mut engine = engine();
        engine.on_zalo_frame(json!({
            "type":"control","data":{
                "act":"request","data":{
                    "callId":123456,"uidN":"caller-zalo","uidFrom":222,"uidTo":111,
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
            call.call_type = 3;
            call.camera_on = false;
            engine.phase = CallPhase::InCall;
        }

        engine.activate_screen_share();
        let frames = engine.poll_outgoing_frames();
        assert_eq!(command(&frames, "sendSignal", 12006)["data"]["isOnCam"], 1);
        let call = engine.call.as_ref().unwrap();
        assert!(call.sharing_screen);
        assert!(!call.camera_was_on);
        assert!(!call.camera_on);

        // The delayed connect reassertion must not overwrite the screen's
        // peer-visible video bit with the internal (camera capture) state.
        engine.call.as_mut().unwrap().camera_state_reassert_at =
            Some(Instant::now() - Duration::from_millis(1));
        engine.tick();
        let reasserted = engine.poll_outgoing_frames();
        assert_eq!(
            command(&reasserted, "sendSignal", 12006)["data"]["isOnCam"],
            1
        );
    }

    #[test]
    fn incoming_one_to_one_call_does_not_replace_an_active_group_call() {
        let mut engine = group_engine();
        engine.on_zalo_frame(json!({
            "type":"update","command":"init","data":{"local":{"id":111}}
        }));
        engine.group_outgoing(&json!({
            "groupInfo":{"id":"g-1","name":"Busy group"},
            "partner":[{"id":"222","name":"Member"}]
        }));
        let group_call_id = engine.group.as_ref().unwrap().call_id;
        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"request","data":{
                "callId":987654,"uidN":"caller","uidFrom":333,"uidTo":111,
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
        engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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

        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"group_ring_ring","data":{
                "id":call_id,"fromId":222
            }}
        }));
        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"group_answer","data":{
                "id":call_id,"fromId":222
            }}
        }));
        engine.on_zalo_frame(json!({
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

        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"group_ring_ring","data":{
                "id":call_id,"fromId":463951353
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

        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"group_ring_ring","data":{
                "id":call_id,"fromId":319623368
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
        engine.on_zalo_frame(json!({
            "type":"control","data":{"act":"group_end_call","data":{
                "id":call_id,"fromId":463951353
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
        engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
            engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
            engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
            engine.on_zalo_frame(json!({
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
            engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
        assert!(group.answered);
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
        engine.on_zalo_frame(json!({
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
            group.camera_on = true;
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

        engine.group.as_mut().unwrap().camera_state_reassert_at =
            Some(Instant::now() - Duration::from_millis(1));
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
        engine.on_zalo_frame(json!({
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
        engine.on_zalo_frame(json!({
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
            group.camera_on = true;
            group.members.get_mut("777").unwrap().state = "incall".into();
        }

        engine.activate_screen_share();
        assert!(engine.group.as_ref().unwrap().sharing_screen);
        assert!(engine.group.as_ref().unwrap().camera_was_on);
        assert!(engine.group.as_ref().unwrap().camera_on);
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
        assert!(!engine.group.as_ref().unwrap().sharing_screen);
        assert!(engine.group.as_ref().unwrap().camera_on);
        assert!(!engine.group.as_ref().unwrap().camera_was_on);
        assert!(!engine.group.as_ref().unwrap().members.contains_key("777"));
        let stopped = engine.poll_outgoing_frames();
        assert!(stopped.iter().any(|event| {
            event["type"] == "sendSignal"
                && event["command"] == 12044
                && event["data"]["status"] == 0
        }));
    }

    #[test]
    fn group_screen_share_restores_an_initially_off_camera_bit() {
        let mut engine = group_engine();
        engine.media = None;
        engine.on_zalo_frame(json!({
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
            group.camera_on = false;
        }

        engine.activate_screen_share();
        assert!(engine.group.as_ref().unwrap().camera_on);
        assert!(!engine.group.as_ref().unwrap().camera_was_on);
        engine.stop_group_screen_share();
        assert!(!engine.group.as_ref().unwrap().camera_on);
        assert!(!engine.group.as_ref().unwrap().camera_was_on);
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
        assert!(group.camera_on);
    }

    #[test]
    fn outgoing_group_call_negotiates_sfu_and_sends_group_join_signal() {
        let mut engine = group_engine();
        engine.on_zalo_frame(json!({
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

        engine.on_zalo_frame(json!({
            "type":"recvSignal","command":12433,
            "data":{
                "callId":123456,
                "hostCall":111,
                "data":{
                    "status":0,
                    "callId":123456,
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
        assert_eq!(join["data"]["callId"], 123456);
        assert_eq!(join["data"]["groupId"], "789");
        assert_eq!(join["data"]["session"], "012345678901234567890123456789");
        let join_payload: Value =
            serde_json::from_str(join["data"]["data"].as_str().unwrap()).unwrap();
        assert_eq!(join_payload["rtpAddress"], "127.0.0.1:9100");
        let nested: Value = serde_json::from_str(join_payload["data"].as_str().unwrap()).unwrap();
        assert_eq!(nested["hostCall"], 111);
        assert_eq!(nested["noiseId"][0], "zalo-2");
        assert!(joined
            .iter()
            .any(|frame| frame["type"] == "sendSignal" && frame["command"] == 12098));
        assert_eq!(engine.group.as_ref().unwrap().phase, GroupPhase::Connected);
        assert!(!engine.group.as_ref().unwrap().camera_on);

        engine.group.as_mut().unwrap().next_ping_at = Some(Instant::now() - Duration::from_secs(1));
        engine.tick();
        let ping = engine.poll_outgoing_frames();
        assert_eq!(
            command(&ping, "sendSignal", 12097)["data"]["callId"],
            123456
        );

        engine.on_zalo_frame(json!({
            "type":"control","data":{
                "act":"group_broadcast",
                "data":{"id":123456,"fromId":222,"callState":3,"audioState":1,"videoState":1}
            }
        }));
        let group = engine.group.as_ref().unwrap();
        assert!(group.answered);
        assert!(group.deadline.is_none());
        assert!(group.members["222"].muted);

        engine.group_hangup("local");
        let ended = engine.poll_outgoing_frames();
        assert_eq!(
            command(&ended, "sendSignal", 12438)["data"]["callId"],
            123456
        );
        assert_eq!(command(&ended, "sendSignal", 12446)["data"]["status"], 50);
        assert_eq!(engine.phase, CallPhase::Idle);
        assert!(engine.group.is_none());
    }

    #[test]
    fn incoming_group_call_can_be_accepted_or_rejected_through_ui_actions() {
        let mut engine = group_engine();
        engine.on_zalo_frame(json!({
            "type":"control","data":{
                "act":"group_request",
                "data":{
                    "id":654321,
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
        engine.on_ui_action(accept);
        let accepted = engine.poll_outgoing_frames();
        assert_eq!(command(&accepted, "sendSignal", 12436)["data"]["status"], 0);
        assert!(accepted
            .iter()
            .any(|frame| frame["type"] == "sendSignal" && frame["command"] == 12098));
        assert_eq!(engine.group.as_ref().unwrap().phase, GroupPhase::Connected);
        assert!(!engine.group.as_ref().unwrap().camera_on);

        engine.group_hangup("local");
        let ended = engine.poll_outgoing_frames();
        assert_eq!(
            command(&ended, "sendSignal", 12438)["data"]["callId"],
            654321
        );

        let mut rejected = group_engine();
        rejected.on_zalo_frame(json!({
            "type":"control","data":{
                "act":"group_request",
                "data":{
                    "id":654322,"hostCall":222,"receiverId":111,"groupId":987,
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
        rejected.on_ui_action(reject);
        let declined = rejected.poll_outgoing_frames();
        assert_eq!(command(&declined, "sendSignal", 12436)["data"]["status"], 3);
        assert!(rejected.group.is_none());
    }
}
