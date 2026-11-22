//! Call UI contract, action model, and the existing frontend transport.
//!
//! The engine communicates directly through this module. The TCP transport
//! preserves the authenticated loopback protocol opened by plugins/zcall.

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, Clone)]
pub struct UiAction {
    pub action: String,
    #[serde(default)]
    pub on: Option<bool>,
    #[serde(default)]
    pub why: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(rename = "camStartOff", default)]
    pub cam_start_off: Option<bool>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default, rename = "requestId")]
    pub request_id: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub screen: Option<bool>,
    #[serde(default)]
    pub w: Option<u32>,
    #[serde(default)]
    pub h: Option<u32>,
    #[serde(default)]
    pub src: Option<u32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub source: Option<String>,
}

/// Transport contract consumed by the call engine. The TCP implementation
/// below this module owns serialization, framing, and frontend socket I/O.
pub trait UiTransport: Send + Sync {
    fn try_recv_action(&self) -> Option<UiAction>;
    fn send_message(&self, message: &Value);
    fn send_video_frame(&self, key: bool, codec: &str, data: &[u8], src: Option<u32>);
    fn send_local_video_frame(&self, key: bool, codec: &str, data: &[u8]);
}

/// Useful for embedding and deterministic core tests without a frontend.
#[derive(Default)]
pub struct NullUiTransport;

impl UiTransport for NullUiTransport {
    fn try_recv_action(&self) -> Option<UiAction> {
        None
    }
    fn send_message(&self, _message: &Value) {}
    fn send_video_frame(&self, _key: bool, _codec: &str, _data: &[u8], _src: Option<u32>) {}
    fn send_local_video_frame(&self, _key: bool, _codec: &str, _data: &[u8]) {}
}

mod tcp;
pub use tcp::TcpUiTransport;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_frontend_diagnostic_and_keyframe_actions() {
        let log: UiAction = serde_json::from_value(serde_json::json!({
            "action": "log",
            "text": "video decode failed"
        }))
        .unwrap();
        assert_eq!(log.action, "log");
        assert_eq!(log.text.as_deref(), Some("video decode failed"));

        let keyframe: UiAction = serde_json::from_value(serde_json::json!({
            "action": "needkey",
            "why": "decoder",
            "src": 1234
        }))
        .unwrap();
        assert_eq!(keyframe.action, "needkey");
        assert_eq!(keyframe.why.as_deref(), Some("decoder"));
        assert_eq!(keyframe.src, Some(1234));

        let screen: UiAction = serde_json::from_value(serde_json::json!({
            "action": "screen",
            "on": true,
            "source": "window:42",
            "requestId": "screen-42"
        }))
        .unwrap();
        assert_eq!(screen.source.as_deref(), Some("window:42"));
        assert_eq!(screen.request_id.as_deref(), Some("screen-42"));
    }
}
