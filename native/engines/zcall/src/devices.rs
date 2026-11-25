//! Native Linux device discovery and call-device preferences.
//!
//! Device discovery, preference storage, and Zalo device-list formatting live here;
//! host frontends render the returned lists and forward a user's selection.
use crate::audio::backend::{pulse_device_inventory, PulseAudioDevice};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use v4l::{format::FourCC, video::Capture};

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

#[must_use]
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

fn audio_devices(devices: &[PulseAudioDevice]) -> Vec<Value> {
    devices
        .iter()
        .filter(|device| {
            !device.id.ends_with(".monitor")
                && !device.id.starts_with("zcall_ec_")
                && !device.id.starts_with("raop_sink.")
                && device.label != "(null)"
        })
        .map(|device| json!({"id": device.id, "label": device.label}))
        .collect()
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
        if !is_capture_node(capabilities.capabilities)
            || !capabilities
                .capabilities
                .contains(v4l::capability::Flags::STREAMING)
        {
            continue;
        }
        let Ok(formats) = device.enum_formats() else {
            continue;
        };
        if !formats
            .iter()
            .any(|format| format.fourcc == FourCC::new(b"YUYV"))
        {
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

#[must_use]
pub fn list_devices() -> Value {
    let inventory = pulse_device_inventory().unwrap_or_default();
    let mics = audio_devices(&inventory.sources);
    let speakers = audio_devices(&inventory.sinks);
    let cameras = camera_devices();
    let default_mic = inventory.default_source;
    let default_speaker = inventory.default_sink;
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

#[must_use]
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
            .map_or_default(|pref| pref.id.as_str()),
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

#[must_use]
pub fn should_block_incoming() -> bool {
    let prefs = load_prefs();
    if !prefs.no_mic_block && !prefs.no_speaker_block {
        return false;
    }
    let devices = list_devices();
    (prefs.no_mic_block
        && devices["mics"]
            .as_array()
            .is_none_or(std::vec::Vec::is_empty))
        || (prefs.no_speaker_block
            && devices["speakers"]
                .as_array()
                .is_none_or(std::vec::Vec::is_empty))
}

#[cfg(test)]
#[path = "tests/devices.rs"]
mod tests;
