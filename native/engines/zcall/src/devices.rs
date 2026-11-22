//! Native Linux device discovery and call-device preferences.
//! Device discovery, preference storage, and Zalo device-list formatting live here;
//! host frontends render the returned lists and forward a user's selection.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DevicePrefs {
    pub mic: String,
    pub speaker: String,
    pub camera: Option<CameraPref>,
    #[serde(rename = "noMicBlock")]
    pub no_mic_block: bool,
    #[serde(rename = "noSpeakerBlock")]
    pub no_speaker_block: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CameraPref {
    pub id: String,
    pub label: String,
}

fn prefs_path() -> Option<PathBuf> {
    std::env::var_os("ZALO_USER_DATA_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .map(|dir| dir.join("zcall-devices.json"))
}

pub fn load_prefs() -> DevicePrefs {
    prefs_path()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save_prefs(prefs: &DevicePrefs) -> Result<(), String> {
    let Some(path) = prefs_path() else {
        return Err("ZALO_USER_DATA_DIR is not configured".into());
    };
    let parent = path
        .parent()
        .ok_or_else(|| "device preference path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let bytes = serde_json::to_vec_pretty(prefs).map_err(|error| error.to_string())?;
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, bytes).map_err(|error| error.to_string())?;
    fs::rename(&temp, &path).map_err(|error| error.to_string())
}

fn pactl(args: &[&str]) -> Option<String> {
    let output = Command::new("pactl").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn audio_devices(kind: &str) -> Vec<Value> {
    let mut devices = Vec::new();
    if let Some(output) = pactl(&["-f", "json", "list", kind]) {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&output) {
            for item in items {
                let id = item.get("name").and_then(Value::as_str).unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                let label = item
                    .get("description")
                    .and_then(Value::as_str)
                    .filter(|label| !label.is_empty() && *label != "(null)")
                    .unwrap_or(id);
                devices.push(json!({"id":id,"label":label}));
            }
        }
    }
    if devices.is_empty() {
        if let Some(output) = pactl(&["list", "short", kind]) {
            for line in output.lines() {
                let Some(id) = line.split('\t').nth(1).filter(|id| !id.is_empty()) else {
                    continue;
                };
                devices.push(json!({"id":id,"label":id}));
            }
        }
    }

    devices.retain(|device| {
        let id = device.get("id").and_then(Value::as_str).unwrap_or_default();
        let label = device
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        !id.ends_with(".monitor")
            && !id.starts_with("zcall_ec_")
            && !id.starts_with("raop_sink.")
            && label != "(null)"
    });
    devices
}

#[cfg(target_os = "linux")]
fn is_capture_node(flags: v4l::capability::Flags) -> bool {
    use v4l::capability::Flags;
    flags.intersects(Flags::VIDEO_CAPTURE | Flags::VIDEO_CAPTURE_MPLANE)
}

pub(crate) fn camera_device_paths() -> Vec<(String, String)> {
    let mut devices = Vec::new();
    let directory = Path::new("/sys/class/video4linux");
    let Ok(entries) = fs::read_dir(directory) else {
        return devices;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("video") {
            continue;
        }
        let id = format!("/dev/{name}");
        if !Path::new(&id).exists() {
            continue;
        }
        // UVC devices often expose a second metadata-only node with the same
        // product name. Only advertise nodes that can actually capture frames.
        let Ok(device) = v4l::Device::with_path(&id) else {
            continue;
        };
        let Ok(capabilities) = device.query_caps() else {
            continue;
        };
        if !is_capture_node(capabilities.capabilities) {
            continue;
        }
        let label = fs::read_to_string(entry.path().join("name"))
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| name.clone());
        devices.push((id, label));
    }
    devices.sort_by(|a, b| a.0.cmp(&b.0));
    devices
}

fn camera_devices() -> Vec<Value> {
    camera_device_paths()
        .into_iter()
        .map(|(id, label)| json!({"id":id,"label":label}))
        .collect()
}

fn preferred_device<'a>(
    devices: &'a [Value],
    preference: &str,
    default: &str,
) -> Option<&'a Value> {
    if !preference.is_empty() {
        if let Some(device) = devices
            .iter()
            .find(|device| device["id"].as_str() == Some(preference))
        {
            return Some(device);
        }
    }
    if !default.is_empty() {
        if let Some(device) = devices
            .iter()
            .find(|device| device["id"].as_str() == Some(default))
        {
            return Some(device);
        }
    }
    devices.first()
}

pub fn list_devices() -> Value {
    let mics = audio_devices("sources");
    let speakers = audio_devices("sinks");
    let cameras = camera_devices();
    let default_mic = pactl(&["get-default-source"]).unwrap_or_default();
    let default_speaker = pactl(&["get-default-sink"]).unwrap_or_default();
    let prefs = load_prefs();

    json!({
        "mics":mics,
        "speakers":speakers,
        "cameras":cameras,
        "defaultMic":default_mic,
        "defaultSpeaker":default_speaker,
        "prefs":prefs
    })
}

pub fn zalo_device_list() -> Value {
    let devices = list_devices();
    let prefs = load_prefs();
    let mics = devices["mics"].as_array().cloned().unwrap_or_default();
    let speakers = devices["speakers"].as_array().cloned().unwrap_or_default();
    let cameras = devices["cameras"].as_array().cloned().unwrap_or_default();
    let default_mic = devices["defaultMic"].as_str().unwrap_or_default();
    let default_speaker = devices["defaultSpeaker"].as_str().unwrap_or_default();

    let to_zalo_item = |item: &Value| {
        let id = item["id"].as_str().unwrap_or_default();
        let label = item["label"].as_str().unwrap_or(id);
        json!({"id":id,"name":label,"guid":id})
    };
    let to_zalo = |items: &[Value]| -> Vec<Value> { items.iter().map(to_zalo_item).collect() };
    let other_mics = to_zalo(&mics);
    let other_speakers = to_zalo(&speakers);
    let other_cameras = to_zalo(&cameras);
    let selected_mic = preferred_device(&mics, &prefs.mic, default_mic).map(to_zalo_item);
    let selected_speaker =
        preferred_device(&speakers, &prefs.speaker, default_speaker).map(to_zalo_item);
    let selected_camera = preferred_device(
        &cameras,
        prefs
            .camera
            .as_ref()
            .map(|pref| pref.id.as_str())
            .unwrap_or_default(),
        "",
    )
    .map(to_zalo_item);

    json!({
        "autoAudioInput":prefs.mic.is_empty(),
        "autoAudioOutput":prefs.speaker.is_empty(),
        "defaultAudInputDevice":selected_mic,
        "defaultAudOutputDevice":selected_speaker,
        "defaultVidDevice":selected_camera,
        "otherAudInputDevice":other_mics,
        "otherAudOutputDevice":other_speakers,
        "otherVidDevice":other_cameras
    })
}

pub fn update_from_zalo(data: &Value) -> Result<(), String> {
    let mut prefs = load_prefs();
    if let Some(auto) = data.get("autoAudioInput").and_then(Value::as_bool) {
        prefs.mic = if auto {
            String::new()
        } else {
            data["defaultAudInputDevice"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
    } else if let Some(id) = data["defaultAudInputDevice"]["id"].as_str() {
        prefs.mic = id.to_string();
    }
    if let Some(auto) = data.get("autoAudioOutput").and_then(Value::as_bool) {
        prefs.speaker = if auto {
            String::new()
        } else {
            data["defaultAudOutputDevice"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        };
    } else if let Some(id) = data["defaultAudOutputDevice"]["id"].as_str() {
        prefs.speaker = id.to_string();
    }
    if let Some(device) = data
        .get("defaultVidDevice")
        .filter(|value| value.is_object())
    {
        if let Some(id) = device.get("id").and_then(Value::as_str) {
            prefs.camera = Some(CameraPref {
                id: id.to_string(),
                label: device
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            });
        }
    }
    save_prefs(&prefs)
}

pub fn update_advanced_options(data: &Value) -> Result<(), String> {
    let mut prefs = load_prefs();
    if let Some(value) = data
        .get("receiveCallWhileNoMicrophoneDetected")
        .and_then(Value::as_bool)
    {
        prefs.no_mic_block = !value;
    }
    if let Some(value) = data
        .get("receiveCallWhileNoSpeakerDetected")
        .and_then(Value::as_bool)
    {
        prefs.no_speaker_block = !value;
    }
    save_prefs(&prefs)
}

pub fn set_device(kind: &str, id: &str) -> Result<(), String> {
    let mut prefs = load_prefs();
    match kind {
        "mic" => prefs.mic = id.to_string(),
        "speaker" => prefs.speaker = id.to_string(),
        "camera" => {
            prefs.camera = if id.is_empty() {
                None
            } else {
                Some(CameraPref {
                    id: id.to_string(),
                    label: id.to_string(),
                })
            }
        }
        _ => return Err(format!("unknown device kind: {kind}")),
    }
    save_prefs(&prefs)
}

pub fn should_block_incoming() -> bool {
    let prefs = load_prefs();
    if !prefs.no_mic_block && !prefs.no_speaker_block {
        return false;
    }
    let devices = list_devices();
    (prefs.no_mic_block
        && devices["mics"]
            .as_array()
            .is_none_or(|items| items.is_empty()))
        || (prefs.no_speaker_block
            && devices["speakers"]
                .as_array()
                .is_none_or(|items| items.is_empty()))
}

#[cfg(test)]
mod tests {
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
    fn zalo_device_response_preserves_auto_flags() {
        let prefs = DevicePrefs::default();
        let value = serde_json::to_value(prefs).unwrap();
        assert_eq!(value["mic"], "");
        assert_eq!(value["speaker"], "");
        assert!(value["camera"].is_null());
    }
}
