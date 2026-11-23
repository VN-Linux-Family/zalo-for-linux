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
#[path = "../tests/ui/mod.rs"]
mod tests;
