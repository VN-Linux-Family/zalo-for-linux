use super::*;
use crate::camera::v4l2::is_capture_node;

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
    assert_eq!(preferences.camera_backend, CameraBackend::Auto);
    assert_eq!(preferences.audio_backend, AudioBackend::Auto);
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

#[test]
fn backend_preferences_default_to_auto_and_accept_explicit_values() {
    let defaults = DevicePrefs::default();
    assert_eq!(defaults.camera_backend, CameraBackend::Auto);
    assert_eq!(defaults.audio_backend, AudioBackend::Auto);

    let configured: DevicePrefs = serde_json::from_value(json!({
        "cameraBackend":"pipewire",
        "audioBackend":"pulse"
    }))
    .unwrap();
    assert_eq!(configured.camera_backend, CameraBackend::PipeWire);
    assert_eq!(configured.audio_backend, AudioBackend::Pulse);
}

fn temporary_prefs_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "zcall-device-prefs-{}-{}.json",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn device_preferences_round_trip_through_the_shared_json_file() {
    let path = temporary_prefs_path();
    let expected = DevicePrefs {
        mic: "alsa_input.usb".into(),
        speaker: "alsa_output.usb".into(),
        camera: Some(CameraPref {
            id: "/dev/video4".into(),
            label: "USB camera".into(),
        }),
        camera_backend: CameraBackend::Portal,
        audio_backend: AudioBackend::PipeWire,
        no_mic_block: true,
        no_speaker_block: false,
    };

    save_prefs_to(&path, &expected).expect("persist device preferences");
    let actual = load_prefs_from(&path);

    assert_eq!(actual.mic, expected.mic);
    assert_eq!(actual.speaker, expected.speaker);
    assert_eq!(
        actual.camera.as_ref().map(|camera| camera.id.as_str()),
        Some("/dev/video4")
    );
    assert_eq!(
        actual.camera.as_ref().map(|camera| camera.label.as_str()),
        Some("USB camera")
    );
    assert_eq!(actual.camera_backend, CameraBackend::Portal);
    assert_eq!(actual.audio_backend, AudioBackend::PipeWire);
    assert!(actual.no_mic_block);
    assert!(!actual.no_speaker_block);

    std::fs::remove_file(&path).expect("remove temporary preferences");
}

#[test]
fn missing_or_malformed_preferences_fall_back_to_defaults() {
    let path = temporary_prefs_path();
    assert_eq!(load_prefs_from(&path).audio_backend, AudioBackend::Auto);
    std::fs::write(&path, b"{not-json").expect("write malformed preferences");
    let loaded = load_prefs_from(&path);
    assert_eq!(loaded.camera_backend, CameraBackend::Auto);
    assert_eq!(loaded.audio_backend, AudioBackend::Auto);
    std::fs::remove_file(path).expect("remove malformed preferences");
}

#[test]
fn advanced_backend_options_update_without_overwriting_device_choices() {
    let mut prefs = DevicePrefs {
        mic: "alsa_input.keep".into(),
        speaker: "alsa_output.keep".into(),
        camera: Some(CameraPref {
            id: "/dev/video2".into(),
            label: "Camera".into(),
        }),
        no_mic_block: true,
        no_speaker_block: false,
        ..DevicePrefs::default()
    };

    apply_advanced_options(
        &mut prefs,
        &json!({
            "cameraBackend":"pipewire",
            "audioBackend":"pulse",
            "receiveCallWhileNoMicrophoneDetected":true,
            "receiveCallWhileNoSpeakerDetected":false
        }),
    )
    .expect("apply advanced options");

    assert_eq!(prefs.camera_backend, CameraBackend::PipeWire);
    assert_eq!(prefs.audio_backend, AudioBackend::Pulse);
    assert_eq!(prefs.mic, "alsa_input.keep");
    assert_eq!(prefs.speaker, "alsa_output.keep");
    assert_eq!(
        prefs.camera.as_ref().map(|camera| camera.id.as_str()),
        Some("/dev/video2")
    );
    assert!(!prefs.no_mic_block);
    assert!(prefs.no_speaker_block);
}

#[test]
fn invalid_advanced_backend_is_rejected_without_partial_changes() {
    let mut prefs = DevicePrefs::default();
    let error = apply_advanced_options(
        &mut prefs,
        &json!({
            "cameraBackend":"portal",
            "audioBackend":"alsa"
        }),
    )
    .unwrap_err();
    assert!(error.contains("unknown audio backend"));
    assert_eq!(prefs.camera_backend, CameraBackend::Auto);
    assert_eq!(prefs.audio_backend, AudioBackend::Auto);
}

#[test]
fn zalo_device_updates_preserve_backend_preferences() {
    let mut prefs = DevicePrefs {
        camera_backend: CameraBackend::PipeWire,
        audio_backend: AudioBackend::Pulse,
        ..DevicePrefs::default()
    };
    apply_zalo_device_config(
        &mut prefs,
        &json!({
            "autoAudioInput":false,
            "defaultAudInputDevice":{"id":"alsa_input.selected"},
            "autoAudioOutput":true,
            "defaultVidDevice":{"id":"/dev/video3","name":"USB camera"}
        }),
    );
    assert_eq!(prefs.mic, "alsa_input.selected");
    assert_eq!(prefs.speaker, "");
    assert_eq!(
        prefs.camera.as_ref().map(|camera| camera.id.as_str()),
        Some("/dev/video3")
    );
    assert_eq!(prefs.camera_backend, CameraBackend::PipeWire);
    assert_eq!(prefs.audio_backend, AudioBackend::Pulse);
}

#[test]
fn preferred_device_uses_explicit_then_default_then_first_available() {
    let devices = vec![
        json!({"id":"first","label":"First"}),
        json!({"id":"default","label":"Default"}),
        json!({"id":"preferred","label":"Preferred"}),
    ];
    assert_eq!(
        preferred_device(&devices, "preferred", "default").unwrap()["id"],
        "preferred"
    );
    assert_eq!(
        preferred_device(&devices, "missing", "default").unwrap()["id"],
        "default"
    );
    assert_eq!(
        preferred_device(&devices, "missing", "also-missing").unwrap()["id"],
        "first"
    );
    assert!(preferred_device(&[], "missing", "missing").is_none());
}

#[test]
fn advanced_settings_are_persisted_and_reloaded_for_the_next_stream() {
    let path = temporary_prefs_path();
    let initial = DevicePrefs {
        mic: "alsa_input.keep".into(),
        speaker: "alsa_output.keep".into(),
        ..DevicePrefs::default()
    };
    save_prefs_to(&path, &initial).expect("write initial device settings");

    update_advanced_options_at(
        &path,
        &json!({
            "cameraBackend":"portal",
            "audioBackend":"pipewire"
        }),
    )
    .expect("save Advanced Settings backend choices");

    let reloaded = load_prefs_from(&path);
    assert_eq!(reloaded.camera_backend, CameraBackend::Portal);
    assert_eq!(reloaded.audio_backend, AudioBackend::PipeWire);
    assert_eq!(reloaded.mic, "alsa_input.keep");
    assert_eq!(reloaded.speaker, "alsa_output.keep");
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(json["cameraBackend"], "portal");
    assert_eq!(json["audioBackend"], "pipewire");

    std::fs::remove_file(path).expect("remove temporary preferences");
}

#[test]
fn rejected_advanced_settings_leave_the_persisted_configuration_unchanged() {
    let path = temporary_prefs_path();
    let initial = DevicePrefs {
        camera_backend: CameraBackend::V4l2,
        audio_backend: AudioBackend::Pulse,
        ..DevicePrefs::default()
    };
    save_prefs_to(&path, &initial).expect("write initial device settings");

    assert!(update_advanced_options_at(
        &path,
        &json!({
            "cameraBackend":"pipewire",
            "audioBackend":"invalid"
        })
    )
    .is_err());

    let reloaded = load_prefs_from(&path);
    assert_eq!(reloaded.camera_backend, CameraBackend::V4l2);
    assert_eq!(reloaded.audio_backend, AudioBackend::Pulse);
    std::fs::remove_file(path).expect("remove temporary preferences");
}

#[test]
fn zalo_device_settings_update_the_same_file_without_resetting_backend_choices() {
    let path = temporary_prefs_path();
    let initial = DevicePrefs {
        camera_backend: CameraBackend::PipeWire,
        audio_backend: AudioBackend::PipeWire,
        ..DevicePrefs::default()
    };
    save_prefs_to(&path, &initial).expect("write initial device settings");

    update_from_zalo_at(
        &path,
        &json!({
            "autoAudioInput":false,
            "defaultAudInputDevice":{"id":"alsa_input.new"},
            "autoAudioOutput":false,
            "defaultAudOutputDevice":{"id":"alsa_output.new"},
            "defaultVidDevice":{"id":"/dev/video5","name":"New camera"}
        }),
    )
    .expect("save Zalo device selection");

    let reloaded = load_prefs_from(&path);
    assert_eq!(reloaded.mic, "alsa_input.new");
    assert_eq!(reloaded.speaker, "alsa_output.new");
    assert_eq!(
        reloaded.camera.as_ref().map(|camera| camera.id.as_str()),
        Some("/dev/video5")
    );
    assert_eq!(reloaded.camera_backend, CameraBackend::PipeWire);
    assert_eq!(reloaded.audio_backend, AudioBackend::PipeWire);
    std::fs::remove_file(path).expect("remove temporary preferences");
}
