use super::*;

#[test]
fn preferences_preserve_advanced_options() {
    let preferences: DevicePrefs = serde_json::from_value(json!({
        "mic":"alsa_input.test",
        "speaker":"alsa_output.test",
        "camera":{"id":"/dev/video2","label":"USB Cam"},
        "noMicBlock":true,
        "noSpeakerBlock":false
    }))
    .unwrap();
    assert_eq!(preferences.mic, "alsa_input.test");
    assert_eq!(preferences.speaker, "alsa_output.test");
    assert_eq!(
        preferences.camera.as_ref().map(|camera| camera.id.as_str()),
        Some("/dev/video2")
    );
    assert!(preferences.no_mic_block);
    assert!(!preferences.no_speaker_block);
}

#[cfg(target_os = "linux")]
#[test]
fn camera_enumeration_excludes_metadata_only_video_nodes() {
    use v4l::capability::Flags;
    assert!(is_capture_node(Flags::VIDEO_CAPTURE));
    assert!(is_capture_node(Flags::VIDEO_CAPTURE_MPLANE));
    assert!(!is_capture_node(Flags::META_CAPTURE));
    assert!(!is_capture_node(Flags::empty()));
}

#[test]
fn audio_device_filter_excludes_monitors_and_internal_loopbacks() {
    let devices = vec![
        PulseAudioDevice {
            id: "alsa_input.usb".into(),
            label: "USB microphone".into(),
        },
        PulseAudioDevice {
            id: "alsa_output.speaker.monitor".into(),
            label: "Monitor".into(),
        },
        PulseAudioDevice {
            id: "zcall_ec_virtual".into(),
            label: "Internal".into(),
        },
        PulseAudioDevice {
            id: "raop_sink.living_room".into(),
            label: "AirPlay".into(),
        },
    ];
    let listed = audio_devices(&devices);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], "alsa_input.usb");
    assert_eq!(listed[0]["label"], "USB microphone");
}

#[test]
fn zalo_device_response_preserves_auto_flags() {
    let prefs = DevicePrefs::default();
    let value = serde_json::to_value(prefs).unwrap();
    assert_eq!(value["mic"], "");
    assert_eq!(value["speaker"], "");
    assert!(value["camera"].is_null());
}
