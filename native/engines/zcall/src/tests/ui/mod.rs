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
