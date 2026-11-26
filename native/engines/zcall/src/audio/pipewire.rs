//! Native-protocol `PipeWire` microphone capture.

use pipewire_client::{
    audio::{AudioFormatCaps, AudioSampleFormat},
    client::PipeWireClient,
    remote::{PipeWireRemote, PIPEWIRE_ID_ANY},
    AudioFrame, NegotiatedAudioFormat, PipeWireGlobal, PipeWireObject, PipeWireStream,
    StreamOptions,
};
use std::io;
use std::time::{Duration, Instant};

pub struct PipeWireRecorder {
    pub(crate) stream: PipeWireStream,
    pending: Vec<u8>,
    converted_pending: Vec<u8>,
    last_meter: Option<super::meter::AudioMeter>,
}

impl PipeWireRecorder {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let discovery_remote = PipeWireRemote::connect_default(PIPEWIRE_ID_ANY)
            .map_err(|error| format!("connect to PipeWire socket: {error}"))?;
        let mut discovery = PipeWireClient::connect(discovery_remote)
            .map_err(|error| format!("initialize PipeWire registry client: {error}"))?;
        let mut sources = discovery
            .list_globals()
            .map_err(|error| format!("enumerate PipeWire audio sources: {error}"))?
            .into_iter()
            .filter(|global| {
                global.interface == "PipeWire:Interface:Node"
                    && global.property("media.class") == Some("Audio/Source")
            })
            .collect::<Vec<_>>();
        sources.sort_by_key(|global| {
            std::cmp::Reverse(
                global
                    .property("priority.session")
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0),
            )
        });
        let selected = if let Some(device) = device.filter(|device| !device.is_empty()) {
            sources
                .iter()
                .find(|global| {
                    global.property("node.name") == Some(device)
                        || global.property("object.serial") == Some(device)
                })
                .ok_or_else(|| format!("PipeWire audio source '{device}' was not found"))?
        } else {
            sources
                .first()
                .ok_or_else(|| "PipeWire has no capture audio source nodes".to_owned())?
        };
        let node_id = selected.id;
        let target_object = selected.target_object().map(str::to_owned);
        let target_serial = selected
            .property("object.serial")
            .unwrap_or_default()
            .to_owned();
        drop(discovery);

        let caps = AudioFormatCaps {
            default: AudioSampleFormat::F32P,
            alternatives: vec![AudioSampleFormat::F32P],
            rate: (48_000, 48_000, 48_000),
            channels: (1, 1, 1),
        };
        let options = StreamOptions {
            name: "ZaloCall Microphone".into(),
            node_properties: vec![
                ("media.name".into(), "ZaloCall Microphone".into()),
                ("media.role".into(), "Communication".into()),
                ("audio.position".into(), "[ FL FR ]".into()),
                ("audio.channels".into(), "2".into()),
                ("stream.is-live".into(), "true".into()),
                ("node.autoconnect".into(), "true".into()),
                ("node.loop.class".into(), "main".into()),
                ("node.async".into(), "true".into()),
                ("node.rate".into(), "1/48000".into()),
                ("node.latency".into(), "1024/48000".into()),
                ("node.target".into(), target_serial),
                (
                    "target.object".into(),
                    target_object.unwrap_or_else(|| node_id.to_string()),
                ),
            ],
            client_properties: vec![("application.name".into(), "ZaloCall".into())],
            auto_link: true,
            target_node_id: Some(node_id),
        };
        let remote = PipeWireRemote::connect_default(PIPEWIRE_ID_ANY)
            .map_err(|error| format!("connect PipeWire capture source: {error}"))?;
        let stream = PipeWireStream::connect_audio_with_options(remote, &caps, &options)
            .map_err(|error| format!("open PipeWire microphone stream: {error}"))?;
        Ok(Self {
            stream,
            pending: Vec::with_capacity(8192),
            converted_pending: Vec::with_capacity(2048),
            last_meter: None,
        })
    }

    /// Return the most recently captured PCM level, before downsampling.
    #[must_use]
    pub fn last_meter(&self) -> Option<&super::meter::AudioMeter> {
        self.last_meter.as_ref()
    }

    pub fn read(&mut self, output: &mut [u8]) -> Result<(), String> {
        const INPUT_FRAME_BYTES: usize = 960 * 8; // 20 ms, 48 kHz, stereo F32LE
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.converted_pending.len() < output.len() {
            while self.pending.len() < INPUT_FRAME_BYTES {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("timed out waiting for PipeWire microphone PCM".into());
                }
                match self
                    .stream
                    .next_audio_frame(remaining)
                    .map_err(|error| format!("read PipeWire microphone stream: {error}"))?
                {
                    Some(AudioFrame { data, format }) => {
                        if format
                            != (NegotiatedAudioFormat {
                                sample_format: AudioSampleFormat::F32Le,
                                rate: 48_000,
                                channels: 2,
                            })
                        {
                            return Err(format!(
                                "PipeWire negotiated unsupported microphone format: {format:?}"
                            ));
                        }
                        self.last_meter =
                            Some(super::meter::AudioMeter::measure(&data, format).map_err(
                                |error| format!("measure PipeWire microphone level: {error}"),
                            )?);
                        self.pending.extend_from_slice(&data);
                    }
                    None => return Err("PipeWire microphone stream timed out".into()),
                }
            }
            let input: Vec<u8> = self.pending.drain(..INPUT_FRAME_BYTES).collect();
            // Low-pass/downsample 48 kHz mono to the call pipeline's 16 kHz mono.
            let (input_frames, _) = input.as_chunks::<24>();
            for samples in input_frames {
                let (samples, _) = samples.as_chunks::<4>();
                let average = samples
                    .iter()
                    .map(|sample| f32::from_le_bytes(*sample))
                    .sum::<f32>()
                    / 6.0;
                let mono = (average * f32::from(i16::MAX))
                    .round()
                    .clamp(f32::from(i16::MIN), f32::from(i16::MAX))
                    as i16;
                self.converted_pending
                    .extend_from_slice(&mono.to_le_bytes());
            }
        }
        output.copy_from_slice(&self.converted_pending[..output.len()]);
        self.converted_pending.drain(..output.len());
        Ok(())
    }
}

fn connect_control_client() -> Result<(PipeWireClient, Vec<PipeWireGlobal>), String> {
    let remote = PipeWireRemote::connect_default(PIPEWIRE_ID_ANY)
        .map_err(|error| format!("connect PipeWire control socket: {error}"))?;
    let mut client = PipeWireClient::connect(remote)
        .map_err(|error| format!("initialize PipeWire control client: {error}"))?;
    let globals = client
        .list_globals()
        .map_err(|error| format!("enumerate PipeWire control globals: {error}"))?;
    Ok((client, globals))
}

fn selected_audio_global<'a>(
    globals: &'a [PipeWireGlobal],
    device: Option<&str>,
    media_class: &str,
) -> Result<&'a PipeWireGlobal, String> {
    let matches = |global: &&PipeWireGlobal| {
        global.interface == "PipeWire:Interface:Node"
            && global.property("media.class") == Some(media_class)
    };
    if let Some(device) = device.filter(|value| !value.is_empty()) {
        return globals
            .iter()
            .filter(matches)
            .find(|global| {
                global.property("node.name") == Some(device)
                    || global.property("object.serial") == Some(device)
                    || global.id.to_string() == device
            })
            .ok_or_else(|| format!("PipeWire audio node '{device}' was not found"));
    }
    globals
        .iter()
        .filter(matches)
        .max_by_key(|global| {
            global
                .property("priority.session")
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0)
        })
        .ok_or_else(|| format!("PipeWire has no {media_class} nodes"))
}

fn with_audio_node<T>(
    device: Option<&str>,
    media_class: &str,
    action: impl FnOnce(&mut PipeWireClient, &PipeWireObject) -> io::Result<T>,
) -> Result<T, String> {
    let (mut client, globals) = connect_control_client()?;
    let global = selected_audio_global(&globals, device, media_class)?;
    let object = client
        .bind_global(global)
        .map_err(|error| format!("bind PipeWire audio node: {error}"))?;
    action(&mut client, &object)
        .map_err(|error| format!("PipeWire audio node control failed: {error}"))
}

/// Current `PipeWire` source controls, converted to the percentage scale used by zcall.
pub fn pipewire_get_source_info(source: Option<&str>) -> Option<super::pulse::PulseSourceInfo> {
    let controls = with_audio_node(source, "Audio/Source", |client, object| {
        client.get_audio_controls(object)
    })
    .ok()
    .flatten()?;
    Some(super::pulse::PulseSourceInfo {
        pct: controls.volume.map_or(100, |volume| {
            (volume * 100.0).round().clamp(0.0, 150.0) as u8
        }),
        muted: controls.mute.unwrap_or(false),
        resolved_name: source.map(str::to_owned),
    })
}

pub fn pipewire_set_source_volume(source: Option<&str>, percent: u8) -> Result<(), String> {
    let volume = f32::from(percent.min(100)) / 100.0;
    with_audio_node(source, "Audio/Source", |client, object| {
        client.set_audio_controls(object, Some(volume), None)
    })
}

pub fn pipewire_set_source_mute(source: Option<&str>, mute: bool) -> Result<(), String> {
    with_audio_node(source, "Audio/Source", |client, object| {
        client.set_audio_controls(object, None, Some(mute))
    })
}

/// Current `PipeWire` output sink volume and mute state.
pub fn pipewire_get_sink_info(sink: Option<&str>) -> Option<super::pulse::PulseSourceInfo> {
    let controls = with_audio_node(sink, "Audio/Sink", |client, object| {
        client.get_audio_controls(object)
    })
    .ok()
    .flatten()?;
    Some(super::pulse::PulseSourceInfo {
        pct: controls.volume.map_or(100, |volume| {
            (volume * 100.0).round().clamp(0.0, 150.0) as u8
        }),
        muted: controls.mute.unwrap_or(false),
        resolved_name: sink.map(str::to_owned),
    })
}

pub fn pipewire_set_sink_volume(sink: Option<&str>, percent: u8) -> Result<(), String> {
    let volume = f32::from(percent.min(100)) / 100.0;
    with_audio_node(sink, "Audio/Sink", |client, object| {
        client.set_audio_controls(object, Some(volume), None)
    })
}

pub fn pipewire_set_sink_mute(sink: Option<&str>, mute: bool) -> Result<(), String> {
    with_audio_node(sink, "Audio/Sink", |client, object| {
        client.set_audio_controls(object, None, Some(mute))
    })
}

pub fn pipewire_device_inventory() -> Option<super::pulse::PulseDeviceInventory> {
    let (client, globals) = connect_control_client().ok()?;
    drop(client);
    Some(pipewire_inventory_from_globals(&globals))
}

fn pipewire_inventory_from_globals(
    globals: &[PipeWireGlobal],
) -> super::pulse::PulseDeviceInventory {
    let mut sources = Vec::new();
    let mut sinks = Vec::new();
    for global in globals
        .iter()
        .filter(|global| global.interface == "PipeWire:Interface:Node")
    {
        let class = global.property("media.class");
        if !matches!(class, Some("Audio/Source" | "Audio/Sink")) {
            continue;
        }
        let id = global
            .property("node.name")
            .or_else(|| global.property("object.serial"))
            .map_or_else(|| global.id.to_string(), str::to_owned);
        let label = global
            .property("node.description")
            .or_else(|| global.property("node.nick"))
            .unwrap_or(&id)
            .to_owned();
        let device = super::pulse::PulseAudioDevice { id, label };
        if class == Some("Audio/Source") {
            sources.push(device);
        } else {
            sinks.push(device);
        }
    }
    super::pulse::PulseDeviceInventory {
        sources,
        sinks,
        default_source: String::new(),
        default_sink: String::new(),
    }
}

/// Native `PipeWire` PCM playback using an output `ClientNode` and SPA buffers.
pub struct PipeWirePlayer {
    stream: PipeWireStream,
}

impl PipeWirePlayer {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let (discovery, globals) = connect_control_client()?;
        let selected = selected_audio_global(&globals, device, "Audio/Sink")?;
        let node_id = selected.id;
        let target_object = selected
            .target_object()
            .map_or_else(|| node_id.to_string(), str::to_owned);
        let target_serial = selected
            .property("object.serial")
            .unwrap_or_default()
            .to_owned();
        drop(discovery);

        let caps = AudioFormatCaps {
            default: AudioSampleFormat::F32P,
            alternatives: vec![AudioSampleFormat::F32P],
            rate: (16_000, 16_000, 16_000),
            channels: (1, 1, 1),
        };
        let options = StreamOptions {
            name: "ZaloCall Playback".into(),
            node_properties: vec![
                ("media.name".into(), "ZaloCall Playback".into()),
                ("media.role".into(), "Communication".into()),
                ("audio.position".into(), "[ MONO ]".into()),
                ("audio.channels".into(), "1".into()),
                ("stream.is-live".into(), "true".into()),
                ("node.autoconnect".into(), "true".into()),
                ("node.loop.class".into(), "main".into()),
                ("node.async".into(), "false".into()),
                ("node.rate".into(), "1/16000".into()),
                ("node.latency".into(), "320/16000".into()),
                ("node.target".into(), target_serial),
                ("target.object".into(), target_object),
            ],
            client_properties: vec![("application.name".into(), "ZaloCall".into())],
            auto_link: true,
            target_node_id: Some(node_id),
        };
        let remote = PipeWireRemote::connect_default(node_id)
            .map_err(|error| format!("connect PipeWire playback: {error}"))?;
        let stream = PipeWireStream::connect_audio_playback(remote, &caps, &options)
            .map_err(|error| format!("open PipeWire playback stream: {error}"))?;
        Ok(Self { stream })
    }

    pub fn write(&mut self, buffer: &[u8]) -> Result<(), String> {
        if buffer.is_empty() {
            return Ok(());
        }
        if !buffer.len().is_multiple_of(2) {
            return Err("PipeWire playback received a partial S16 sample".into());
        }
        let samples = buffer
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|sample| (f32::from(i16::from_le_bytes(*sample)) / 32_768.0).to_le_bytes())
            .collect::<Vec<_>>();
        if self
            .stream
            .queue_audio_frame(&samples, Duration::from_millis(100))
            .map_err(|error| format!("PipeWire playback write failed: {error}"))?
        {
            Ok(())
        } else {
            Err("PipeWire playback timed out waiting for an output buffer".into())
        }
    }

    #[must_use]
    pub const fn latency_ms() -> Option<u64> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{pipewire_inventory_from_globals, selected_audio_global};
    use pipewire_client::PipeWireGlobal;

    fn global(id: u32, interface: &str, properties: &[(&str, &str)]) -> PipeWireGlobal {
        PipeWireGlobal {
            id,
            permissions: 0,
            interface: interface.into(),
            version: 3,
            properties: properties
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
                .collect(),
        }
    }

    #[test]
    fn selected_audio_node_matches_name_serial_or_global_id_and_class() {
        let globals = vec![
            global(
                11,
                "PipeWire:Interface:Node",
                &[
                    ("media.class", "Audio/Source"),
                    ("node.name", "mic.usb"),
                    ("object.serial", "101"),
                ],
            ),
            global(
                12,
                "PipeWire:Interface:Node",
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.name", "speaker.usb"),
                    ("object.serial", "102"),
                ],
            ),
        ];
        assert_eq!(
            selected_audio_global(&globals, Some("mic.usb"), "Audio/Source")
                .unwrap()
                .id,
            11
        );
        assert_eq!(
            selected_audio_global(&globals, Some("101"), "Audio/Source")
                .unwrap()
                .id,
            11
        );
        assert_eq!(
            selected_audio_global(&globals, Some("12"), "Audio/Sink")
                .unwrap()
                .id,
            12
        );
        assert!(selected_audio_global(&globals, Some("speaker.usb"), "Audio/Source").is_err());
        assert!(selected_audio_global(&globals, Some("missing"), "Audio/Sink").is_err());
    }

    #[test]
    fn default_audio_node_uses_highest_session_priority() {
        let globals = vec![
            global(
                11,
                "PipeWire:Interface:Node",
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.name", "speaker.low"),
                    ("priority.session", "10"),
                ],
            ),
            global(
                12,
                "PipeWire:Interface:Node",
                &[
                    ("media.class", "Audio/Sink"),
                    ("node.name", "speaker.default"),
                    ("priority.session", "100"),
                ],
            ),
        ];
        assert_eq!(
            selected_audio_global(&globals, None, "Audio/Sink")
                .unwrap()
                .id,
            12
        );
    }

    #[test]
    fn pipewire_inventory_lists_only_audio_nodes_with_stable_ids_and_labels() {
        let globals = vec![
            global(
                11,
                "PipeWire:Interface:Node",
                &[
                    ("media.class", "Audio/Source"),
                    ("node.name", "mic.usb"),
                    ("node.description", "USB Microphone"),
                ],
            ),
            global(
                12,
                "PipeWire:Interface:Node",
                &[
                    ("media.class", "Audio/Sink"),
                    ("object.serial", "202"),
                    ("node.nick", "USB Speakers"),
                ],
            ),
            global(
                13,
                "PipeWire:Interface:Node",
                &[("media.class", "Video/Source")],
            ),
            global(14, "PipeWire:Interface:Client", &[]),
            global(
                15,
                "PipeWire:Interface:Node",
                &[("media.class", "Audio/Source")],
            ),
        ];
        let inventory = pipewire_inventory_from_globals(&globals);
        assert_eq!(inventory.sources.len(), 2);
        assert_eq!(inventory.sources[0].id, "mic.usb");
        assert_eq!(inventory.sources[0].label, "USB Microphone");
        assert_eq!(inventory.sources[1].id, "15");
        assert_eq!(inventory.sources[1].label, "15");
        assert_eq!(inventory.sinks.len(), 1);
        assert_eq!(inventory.sinks[0].id, "202");
        assert_eq!(inventory.sinks[0].label, "USB Speakers");
    }

    #[test]
    #[ignore = "requires a live PipeWire session with an audio source; query-only"]
    fn native_pipewire_audio_inventory_is_queryable() {
        let inventory = super::pipewire_device_inventory().expect("connect to PipeWire");
        assert!(
            !inventory.sources.is_empty(),
            "PipeWire reported no audio sources"
        );
        assert!(
            !inventory.sinks.is_empty(),
            "PipeWire reported no audio sinks"
        );
    }

    #[test]
    #[ignore = "requires a live PipeWire session with an audio source; reads a short PCM block"]
    fn native_pipewire_microphone_stream_exposes_pcm_meter() {
        let mut recorder = super::PipeWireRecorder::new(None).expect("open PipeWire microphone");
        let mut pcm = [0_u8; 640];
        recorder.read(&mut pcm).expect("capture microphone PCM");
        let meter = recorder.last_meter().expect("capture stream meter");
        assert_eq!(meter.channels.len(), 2);
        assert!(meter.frames > 0);
        assert!(meter.peak.is_finite() && meter.peak >= 0.0);
        assert!(meter.rms.is_finite() && meter.rms >= 0.0);
    }

    #[test]
    #[ignore = "requires a live PipeWire session; changes and restores output sink controls"]
    fn native_pipewire_sink_volume_and_mute_are_applied_and_restored() {
        let before = super::pipewire_get_sink_info(None)
            .expect("read default PipeWire output sink controls");
        let exercise = || -> Result<(u8, bool), String> {
            let changed_volume = if before.pct >= 90 { 70 } else { 95 };
            super::pipewire_set_sink_volume(None, changed_volume)?;
            let volume = super::pipewire_get_sink_info(None)
                .ok_or_else(|| "read changed PipeWire sink volume".to_owned())?;
            super::pipewire_set_sink_mute(None, !before.muted)?;
            let mute = super::pipewire_get_sink_info(None)
                .ok_or_else(|| "read changed PipeWire sink mute".to_owned())?;
            Ok((volume.pct, mute.muted))
        };
        let exercise_result = exercise();
        let restore_volume = super::pipewire_set_sink_volume(None, before.pct);
        let restore_mute = super::pipewire_set_sink_mute(None, before.muted);
        let (changed_volume, changed_mute) =
            exercise_result.expect("PipeWire sink controls must take effect");
        restore_volume.expect("restore original PipeWire sink volume");
        restore_mute.expect("restore original PipeWire sink mute");
        assert_eq!(changed_volume, if before.pct >= 90 { 70 } else { 95 });
        assert_eq!(changed_mute, !before.muted);
        let after =
            super::pipewire_get_sink_info(None).expect("read restored PipeWire sink controls");
        assert_eq!(after.pct, before.pct);
        assert_eq!(after.muted, before.muted);
    }

    #[test]
    #[ignore = "requires a live PipeWire session; changes and restores microphone controls"]
    fn native_pipewire_microphone_volume_and_mute_are_applied_and_restored() {
        let before = super::pipewire_get_source_info(None)
            .expect("read default PipeWire microphone controls");
        let exercise = || -> Result<(u8, bool), String> {
            let changed_volume = if before.pct >= 90 { 70 } else { 95 };
            super::pipewire_set_source_volume(None, changed_volume)?;
            let volume = super::pipewire_get_source_info(None)
                .ok_or_else(|| "read changed PipeWire microphone volume".to_owned())?;
            super::pipewire_set_source_mute(None, !before.muted)?;
            let mute = super::pipewire_get_source_info(None)
                .ok_or_else(|| "read changed PipeWire microphone mute".to_owned())?;
            Ok((volume.pct, mute.muted))
        };
        let exercise_result = exercise();

        // Restore the user's original microphone settings even if an assertion fails.
        let restore_volume = super::pipewire_set_source_volume(None, before.pct);
        let restore_mute = super::pipewire_set_source_mute(None, before.muted);
        let (changed_volume, changed_mute) =
            exercise_result.expect("PipeWire microphone controls must take effect");
        restore_volume.expect("restore original PipeWire microphone volume");
        restore_mute.expect("restore original PipeWire microphone mute");
        assert_eq!(changed_volume, if before.pct >= 90 { 70 } else { 95 });
        assert_eq!(changed_mute, !before.muted);
        let after = super::pipewire_get_source_info(None)
            .expect("verify restored PipeWire microphone controls");
        assert_eq!(after.pct, before.pct);
        assert_eq!(after.muted, before.muted);
    }
}
