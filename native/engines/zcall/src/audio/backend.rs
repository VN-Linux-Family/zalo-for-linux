//! PulseAudio / PipeWire audio backend.

use pulseaudio::{protocol, Client, PlaybackSource, PlaybackStream, RecordStream};
use std::ffi::CString;
use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc::{self, Receiver, Sender};

const PULSE_OPERATION_TIMEOUT: Duration = Duration::from_millis(1500);
const PA_VOLUME_NORM: u32 = 65_536;

/// Synchronous control connection over PulseAudio's native Unix-socket protocol.
struct PulseConnection {
    socket: BufReader<UnixStream>,
    protocol_version: u16,
    sequence: u32,
}
impl PulseConnection {
    fn connect() -> Result<Self, String> {
        let socket_path = pulseaudio::socket_path_from_env()
            .ok_or_else(|| "PulseAudio server socket is unavailable".to_owned())?;
        let socket = UnixStream::connect(&socket_path)
            .map_err(|error| format!("could not connect to PulseAudio socket: {error}"))?;
        socket
            .set_read_timeout(Some(PULSE_OPERATION_TIMEOUT))
            .and_then(|()| socket.set_write_timeout(Some(PULSE_OPERATION_TIMEOUT)))
            .map_err(|error| format!("could not configure PulseAudio socket timeout: {error}"))?;
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|path| std::fs::read(path).ok())
            .unwrap_or_default();
        let mut connection = Self {
            socket: BufReader::new(socket),
            protocol_version: protocol::MAX_VERSION,
            sequence: 0,
        };
        let auth: protocol::AuthReply = connection.roundtrip_with_version(
            protocol::Command::Auth(protocol::AuthParams {
                version: protocol::MAX_VERSION,
                supports_shm: false,
                supports_memfd: false,
                cookie,
            }),
            protocol::MAX_VERSION,
        )?;
        connection.protocol_version = protocol::MAX_VERSION.min(auth.version);
        let mut props = protocol::Props::new();
        props.set(
            protocol::Prop::ApplicationName,
            CString::new("ZaloCall-VolCtl").expect("static application name"),
        );
        props.set(
            protocol::Prop::ApplicationProcessId,
            CString::new(std::process::id().to_string()).expect("numeric process id"),
        );
        let _: protocol::SetClientNameReply =
            connection.roundtrip(protocol::Command::SetClientName(props))?;
        Ok(connection)
    }
    fn next_sequence(&mut self) -> u32 {
        let seq = self.sequence;
        self.sequence = self.sequence.wrapping_add(1);
        seq
    }
    fn roundtrip_with_version<T: protocol::CommandReply>(
        &mut self,
        command: protocol::Command,
        version: u16,
    ) -> Result<T, String> {
        let sequence = self.next_sequence();
        protocol::write_command_message(self.socket.get_mut(), sequence, &command, version)
            .and_then(|()| {
                self.socket
                    .get_mut()
                    .flush()
                    .map_err(protocol::ProtocolError::from)
            })
            .map_err(|error| format!("PulseAudio command write failed: {error}"))?;
        let (reply_sequence, reply) = protocol::read_reply_message::<T>(&mut self.socket, version)
            .map_err(|error| format!("PulseAudio command failed: {error}"))?;
        if reply_sequence != sequence {
            return Err(format!(
                "PulseAudio reply sequence mismatch: expected {sequence}, got {reply_sequence}"
            ));
        }
        Ok(reply)
    }
    fn roundtrip<T: protocol::CommandReply>(
        &mut self,
        command: protocol::Command,
    ) -> Result<T, String> {
        self.roundtrip_with_version(command, self.protocol_version)
    }
    fn acknowledge(&mut self, command: protocol::Command) -> Result<(), String> {
        let sequence = self.next_sequence();
        protocol::write_command_message(
            self.socket.get_mut(),
            sequence,
            &command,
            self.protocol_version,
        )
        .and_then(|()| {
            self.socket
                .get_mut()
                .flush()
                .map_err(protocol::ProtocolError::from)
        })
        .map_err(|error| format!("PulseAudio command write failed: {error}"))?;
        let reply_sequence = protocol::read_ack_message(&mut self.socket)
            .map_err(|error| format!("PulseAudio command rejected: {error}"))?;
        if reply_sequence != sequence {
            return Err(format!(
                "PulseAudio ack sequence mismatch: expected {sequence}, got {reply_sequence}"
            ));
        }
        Ok(())
    }
}
fn pulse_connection() -> Result<PulseConnection, String> {
    PulseConnection::connect()
}
fn optional_c_string(value: Option<&str>) -> Option<CString> {
    value
        .filter(|value| !value.is_empty())
        .and_then(|value| CString::new(value).ok())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PulseSourceInfo {
    pub pct: u8,
    pub muted: bool,
    pub resolved_name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PulseAudioDevice {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PulseDeviceInventory {
    pub sources: Vec<PulseAudioDevice>,
    pub sinks: Vec<PulseAudioDevice>,
    pub default_source: String,
    pub default_sink: String,
}

#[derive(Clone, Debug)]
struct SourceSnapshot {
    index: u32,
    channels: u8,
    average_volume: u32,
    muted: bool,
    resolved_name: Option<String>,
}

#[derive(Clone, Debug)]
struct SourceOutputSnapshot {
    index: u32,
    channels: u8,
    average_volume: u32,
    muted: bool,
    has_volume: bool,
    volume_writable: bool,
}

fn average_channel_volume(channels: u8, values: &[u32]) -> Option<u32> {
    if channels == 0 || values.len() < usize::from(channels) {
        return None;
    }
    let sum: u64 = values[..usize::from(channels)]
        .iter()
        .map(|&value| u64::from(value))
        .sum();
    // pa_cvolume_avg uses integer division (rounds down).
    Some((sum / u64::from(channels)) as u32)
}

fn volume_to_percent(volume: u32) -> u8 {
    let percent =
        (u64::from(volume) * 100 + u64::from(PA_VOLUME_NORM / 2)) / u64::from(PA_VOLUME_NORM);
    percent.min(150) as u8
}

fn percent_to_volume(percent: u8) -> u32 {
    (f64::from(PA_VOLUME_NORM) * f64::from(percent.min(150)) / 100.0).round() as u32
}

fn parse_pid_like_atoi(value: &str) -> i64 {
    let value = value.trim_start();
    let (negative, digits) = match value.as_bytes().first() {
        Some(b'-') => (true, &value[1..]),
        Some(b'+') => (false, &value[1..]),
        _ => (false, value),
    };
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    let parsed = digits.parse::<i64>().unwrap_or(0);
    if negative {
        -parsed
    } else {
        parsed
    }
}

fn resolve_source_name(
    connection: &mut PulseConnection,
    source: Option<&str>,
) -> Result<String, String> {
    if let Some(source) = source.filter(|name| {
        !name.is_empty() && !name.as_bytes().contains(&0) && *name != "@DEFAULT_SOURCE@"
    }) {
        return Ok(source.to_owned());
    }
    let info: protocol::ServerInfo = connection.roundtrip(protocol::Command::GetServerInfo)?;
    info.default_source_name
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "PulseAudio did not report a default source".into())
}
fn fetch_source_info(
    connection: &mut PulseConnection,
    source_name: &str,
) -> Result<SourceSnapshot, String> {
    let name = CString::new(source_name)
        .map_err(|_| "PulseAudio source name contains an interior NUL byte".to_owned())?;
    let info: protocol::SourceInfo =
        connection.roundtrip(protocol::Command::GetSourceInfo(protocol::GetSourceInfo {
            index: None,
            name: Some(name),
        }))?;
    let values: Vec<u32> = info.cvolume.channels().iter().map(|v| v.as_u32()).collect();
    let channels = u8::try_from(values.len())
        .map_err(|_| "PulseAudio source reports too many channels".to_owned())?;
    let average_volume = average_channel_volume(channels, &values)
        .ok_or_else(|| format!("PulseAudio source '{source_name}' has no channels"))?;
    Ok(SourceSnapshot {
        index: info.index,
        channels,
        average_volume,
        muted: info.muted,
        resolved_name: Some(info.name.to_string_lossy().into_owned()),
    })
}
fn selected_source_info(
    connection: &mut PulseConnection,
    source: Option<&str>,
) -> Result<SourceSnapshot, String> {
    let source_name = resolve_source_name(connection, source)?;
    fetch_source_info(connection, &source_name)
}
fn channel_volume(channels: u8, volume: u32) -> Result<protocol::ChannelVolume, String> {
    if channels == 0 || channels > 32 {
        return Err(format!("invalid PulseAudio channel count: {channels}"));
    }
    let mut result = protocol::ChannelVolume::empty();
    for _ in 0..channels {
        result.push(protocol::Volume::from_u32_clamped(volume));
    }
    Ok(result)
}
fn set_source_volume_on_connection(
    connection: &mut PulseConnection,
    info: &SourceSnapshot,
    percent: u8,
) -> Result<(), String> {
    connection.acknowledge(protocol::Command::SetSourceVolume(
        protocol::SetDeviceVolumeParams {
            device_index: Some(info.index),
            device_name: None,
            volume: channel_volume(info.channels, percent_to_volume(percent))?,
        },
    ))
}
fn set_source_mute_on_connection(
    connection: &mut PulseConnection,
    info: &SourceSnapshot,
    mute: bool,
) -> Result<(), String> {
    connection.acknowledge(protocol::Command::SetSourceMute(
        protocol::SetDeviceMuteParams {
            device_index: Some(info.index),
            device_name: None,
            mute,
        },
    ))
}
fn set_source_output_mute_on_connection(
    connection: &mut PulseConnection,
    index: u32,
    mute: bool,
) -> Result<(), String> {
    connection.acknowledge(protocol::Command::SetSourceOutputMute(
        protocol::SetStreamMuteParams { index, mute },
    ))
}
fn set_source_output_volume_on_connection(
    connection: &mut PulseConnection,
    index: u32,
    channels: u8,
) -> Result<(), String> {
    connection.acknowledge(protocol::Command::SetSourceOutputVolume(
        protocol::SetStreamVolumeParams {
            index,
            volume: channel_volume(channels, PA_VOLUME_NORM)?,
        },
    ))
}

/// Read the current PulseAudio/PipeWire source volume, mute state and resolved source name.
pub fn pulse_get_source_info(source: Option<&str>) -> Option<PulseSourceInfo> {
    let mut connection = pulse_connection().ok()?;
    let info = selected_source_info(&mut connection, source).ok()?;
    Some(PulseSourceInfo {
        pct: volume_to_percent(info.average_volume),
        muted: info.muted,
        resolved_name: info.resolved_name,
    })
}
/// Enumerate PulseAudio-compatible sources/sinks and their current defaults.
pub fn pulse_device_inventory() -> Option<PulseDeviceInventory> {
    let mut connection = pulse_connection().ok()?;
    let server: protocol::ServerInfo = connection
        .roundtrip(protocol::Command::GetServerInfo)
        .ok()?;
    let sources: protocol::SourceInfoList = connection
        .roundtrip(protocol::Command::GetSourceInfoList)
        .ok()?;
    let sinks: protocol::SinkInfoList = connection
        .roundtrip(protocol::Command::GetSinkInfoList)
        .ok()?;
    let to_device = |name: std::ffi::CString, description: Option<std::ffi::CString>| {
        let id = name.to_string_lossy().into_owned();
        let label = description
            .map(|value| value.to_string_lossy().into_owned())
            .filter(|value| !value.is_empty() && value != "(null)")
            .unwrap_or_else(|| id.clone());
        PulseAudioDevice { id, label }
    };
    Some(PulseDeviceInventory {
        sources: sources
            .into_iter()
            .map(|info| to_device(info.name, info.description))
            .collect(),
        sinks: sinks
            .into_iter()
            .map(|info| to_device(info.name, info.description))
            .collect(),
        default_source: server
            .default_source_name
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default(),
        default_sink: server
            .default_sink_name
            .map(|value| value.to_string_lossy().into_owned())
            .unwrap_or_default(),
    })
}

/// Set hardware/PulseAudio/PipeWire source volume (0..=100%) through the native protocol.
pub fn pulse_set_source_volume(source: Option<&str>, percent: u8) -> Result<(), String> {
    let mut connection = pulse_connection()?;
    let info = selected_source_info(&mut connection, source)?;
    set_source_volume_on_connection(&mut connection, &info, percent.min(100))
}
/// Set microphone mute state through the PulseAudio native protocol.
pub fn pulse_set_source_mute(source: Option<&str>, mute: bool) -> Result<(), String> {
    let mut connection = pulse_connection()?;
    let info = selected_source_info(&mut connection, source)?;
    set_source_mute_on_connection(&mut connection, &info, mute)
}
pub fn set_source_mute(source: Option<&str>, mute: bool) -> Result<(), String> {
    pulse_set_source_mute(source, mute)
}

fn source_output_for_pid(
    connection: &mut PulseConnection,
    target_pid: i64,
) -> Result<Option<SourceOutputSnapshot>, String> {
    let outputs: protocol::SourceOutputInfoList =
        connection.roundtrip(protocol::Command::GetSourceOutputInfoList)?;
    Ok(outputs.into_iter().find_map(|info| {
        let pid = info
            .props
            .get(protocol::Prop::ApplicationProcessId)
            .and_then(|v| std::str::from_utf8(v).ok())
            .map(|v| v.trim_end_matches('\0'));
        if pid.map(parse_pid_like_atoi) != Some(target_pid) {
            return None;
        }
        let values: Vec<u32> = info.cvolume.channels().iter().map(|v| v.as_u32()).collect();
        let channels = u8::try_from(values.len()).ok()?;
        let average_volume = average_channel_volume(channels, &values).unwrap_or(0);
        Some(SourceOutputSnapshot {
            index: info.index,
            channels,
            average_volume,
            muted: info.muted,
            has_volume: info.has_volume,
            volume_writable: info.volume_writable,
        })
    }))
}

/// Normalize this process's PulseAudio/PipeWire capture stream to unity and unmuted.
pub fn pulse_normalize_own_stream() {
    let Ok(mut connection) = pulse_connection() else {
        return;
    };
    let Ok(Some(stream)) = source_output_for_pid(&mut connection, std::process::id() as i64) else {
        return;
    };

    if stream.muted {
        let _ = set_source_output_mute_on_connection(&mut connection, stream.index, false);
    }
    if stream.has_volume
        && stream.volume_writable
        && stream.channels > 0
        && stream.average_volume < (PA_VOLUME_NORM * 95) / 100
    {
        let _ =
            set_source_output_volume_on_connection(&mut connection, stream.index, stream.channels);
    }
}

/// Read PulseAudio/PipeWire source volume through the native protocol.
pub fn get_source_volume(source: Option<&str>) -> Option<u8> {
    pulse_get_source_info(source).map(|info| info.pct)
}

/// Read-only startup diagnostics using the PulseAudio native protocol.
pub fn probe_audio_hardware(speaker: Option<&str>, microphone: Option<&str>) {
    let Ok(mut connection) = pulse_connection() else {
        crate::zcall_log!("[zcall-rust] hw: PulseAudio-compatible server unavailable");
        return;
    };
    let server = connection.roundtrip::<protocol::ServerInfo>(protocol::Command::GetServerInfo);
    let backend = server
        .as_ref()
        .ok()
        .and_then(|info| info.server_name.as_ref())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "PulseAudio-compatible".into());
    crate::zcall_log!("[zcall-rust] hw: audio backend={backend}");
    crate::zcall_log!(
        "[zcall-rust] hw: target speaker='{}', mic='{}'",
        speaker.filter(|name| !name.is_empty()).unwrap_or("default"),
        microphone
            .filter(|name| !name.is_empty())
            .unwrap_or("default")
    );

    if let Ok(sources) =
        connection.roundtrip::<protocol::SourceInfoList>(protocol::Command::GetSourceInfoList)
    {
        for source in sources {
            let name = source.name.to_string_lossy();
            let description = source
                .description
                .as_ref()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default();
            let identity = format!("{name} {description}");
            if identity.to_ascii_lowercase().contains("bluez") {
                crate::zcall_log!("[zcall-rust] hw: detected bluetooth source: {identity}");
                let profile = identity.to_ascii_lowercase();
                if [
                    "headset-head-unit",
                    "headset_head_unit",
                    "handsfree",
                    "hfp",
                    "hsp",
                ]
                .iter()
                .any(|term| profile.contains(term))
                {
                    crate::zcall_log!("[zcall-rust] hw: bluetooth HFP/HSP speech profile active");
                } else if profile.contains("a2dp") {
                    crate::zcall_log!("[zcall-rust] hw: bluetooth A2DP profile active");
                }
            }
            crate::zcall_log!(
                "[zcall-rust] hw: source '{name}' sample rate={} Hz",
                source.sample_spec.sample_rate
            );
        }
    }
    if let Ok(sinks) =
        connection.roundtrip::<protocol::SinkInfoList>(protocol::Command::GetSinkInfoList)
    {
        for sink in sinks {
            let name = sink.name.to_string_lossy();
            let description = sink
                .description
                .as_ref()
                .map(|value| value.to_string_lossy())
                .unwrap_or_default();
            let identity = format!("{name} {description}");
            if identity.to_ascii_lowercase().contains("bluez") {
                crate::zcall_log!("[zcall-rust] hw: detected bluetooth sink: {identity}");
                let profile = identity.to_ascii_lowercase();
                if [
                    "headset-head-unit",
                    "headset_head_unit",
                    "handsfree",
                    "hfp",
                    "hsp",
                ]
                .iter()
                .any(|term| profile.contains(term))
                {
                    crate::zcall_log!("[zcall-rust] hw: bluetooth HFP/HSP speech profile active");
                } else if profile.contains("a2dp") {
                    crate::zcall_log!("[zcall-rust] hw: bluetooth A2DP profile active");
                }
            }
            crate::zcall_log!(
                "[zcall-rust] hw: sink '{name}' sample rate={} Hz",
                sink.sample_spec.sample_rate
            );
        }
    }
    if let Ok(cards) =
        connection.roundtrip::<protocol::CardInfoList>(protocol::Command::GetCardInfoList)
    {
        for card in cards {
            if let Some(profile) = card.active_profile {
                let profile = profile.to_string_lossy();
                let lower = profile.to_ascii_lowercase();
                if ["headset", "handsfree", "hfp", "hsp", "a2dp"]
                    .iter()
                    .any(|term| lower.contains(term))
                {
                    crate::zcall_log!("[zcall-rust] hw: bluetooth profile {profile}");
                }
            }
        }
    }
}

/// Set PulseAudio/PipeWire source volume. The UI slider is capped at 100%.
pub fn set_source_volume(source: Option<&str>, percent: u8) -> Result<(), String> {
    pulse_set_source_volume(source, percent.min(100))
}

fn pulse_runtime() -> Result<Runtime, String> {
    Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("could not create PulseAudio runtime: {error}"))
}

fn pulse_client() -> Result<Client, String> {
    let name = CString::new("ZaloCall").expect("static client name");
    Client::from_env(name).map_err(|error| format!("could not connect to PulseAudio: {error}"))
}

fn pulse_sample_spec() -> protocol::SampleSpec {
    protocol::SampleSpec {
        format: protocol::SampleFormat::S16Le,
        channels: 1,
        sample_rate: 16_000,
    }
}

fn pulse_stream_props(name: &str) -> protocol::Props {
    let mut props = protocol::Props::new();
    props.set(
        protocol::Prop::MediaName,
        CString::new(name).expect("static stream name"),
    );
    props
}

struct PulsePlaybackSource {
    receiver: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    offset: usize,
}

impl PulsePlaybackSource {
    fn new(receiver: Receiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            pending: Vec::new(),
            offset: 0,
        }
    }
}

impl PlaybackSource for PulsePlaybackSource {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<usize> {
        let this = self.as_mut().get_mut();
        if buffer.is_empty() {
            return Poll::Ready(0);
        }

        loop {
            if this.offset < this.pending.len() {
                let count = buffer.len().min(this.pending.len() - this.offset);
                buffer[..count].copy_from_slice(&this.pending[this.offset..this.offset + count]);
                this.offset += count;
                return Poll::Ready(count);
            }

            this.pending.clear();
            this.offset = 0;
            match Pin::new(&mut this.receiver).poll_recv(context) {
                Poll::Ready(Some(chunk)) => this.pending = chunk,
                Poll::Ready(None) => return Poll::Ready(0),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

pub struct PulseRecorder {
    runtime: Runtime,
    stream: Option<RecordStream>,
    buffer: pulseaudio::RecordBuffer,
}

impl PulseRecorder {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let runtime = pulse_runtime()?;
        let client = pulse_client()?;
        let buffer = pulseaudio::RecordBuffer::new(640 * 8);
        let params = protocol::RecordStreamParams {
            sample_spec: pulse_sample_spec(),
            channel_map: protocol::ChannelMap::mono(),
            source_name: optional_c_string(device),
            buffer_attr: protocol::stream::BufferAttr {
                max_length: u32::MAX,
                fragment_size: 640,
                ..Default::default()
            },
            cvolume: Some(protocol::ChannelVolume::norm(1)),
            props: pulse_stream_props("microphone"),
            ..Default::default()
        };

        let sink = buffer.as_record_sink();
        let stream = runtime.block_on(async move {
            client
                .create_record_stream(params, sink)
                .await
                .map_err(|error| format!("could not create PulseAudio record stream: {error}"))
        })?;

        Ok(Self {
            runtime,
            stream: Some(stream),
            buffer,
        })
    }

    pub fn read(&mut self, buffer: &mut [u8]) -> Result<(), String> {
        use futures_util::io::AsyncReadExt;

        self.runtime
            .block_on(self.buffer.read_exact(buffer))
            .map_err(|error| format!("PulseAudio record stream read failed: {error}"))
    }
}

impl Drop for PulseRecorder {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = self.runtime.block_on(stream.delete());
        }
    }
}

pub struct PulsePlayer {
    runtime: Runtime,
    stream: Option<PlaybackStream>,
    sender: Sender<Vec<u8>>,
}

impl PulsePlayer {
    pub fn new(device: Option<&str>) -> Result<Self, String> {
        let runtime = pulse_runtime()?;
        let client = pulse_client()?;
        let (sender, receiver) = mpsc::channel(3);
        let params = protocol::PlaybackStreamParams {
            sample_spec: pulse_sample_spec(),
            channel_map: protocol::ChannelMap::mono(),
            sink_name: optional_c_string(device),
            buffer_attr: protocol::stream::BufferAttr {
                max_length: u32::MAX,
                target_length: 1920,
                pre_buffering: u32::MAX,
                minimum_request_length: u32::MAX,
                ..Default::default()
            },
            cvolume: Some(protocol::ChannelVolume::norm(1)),
            props: pulse_stream_props("playback"),
            ..Default::default()
        };

        let stream = runtime.block_on(async move {
            client
                .create_playback_stream(params, PulsePlaybackSource::new(receiver))
                .await
                .map_err(|error| format!("could not create PulseAudio playback stream: {error}"))
        })?;

        Ok(Self {
            runtime,
            stream: Some(stream),
            sender,
        })
    }

    pub fn write(&mut self, buffer: &[u8]) -> Result<(), String> {
        if buffer.is_empty() {
            return Ok(());
        }
        self.sender
            .blocking_send(buffer.to_vec())
            .map_err(|_| "PulseAudio playback stream disconnected".to_owned())
    }

    pub fn latency_ms(&self) -> Option<u64> {
        let stream = self.stream.as_ref()?;
        self.runtime
            .block_on(stream.timing_info())
            .ok()
            .map(|timing| timing.sink_usec.saturating_add(timing.source_usec) / 1000)
    }
}

impl Drop for PulsePlayer {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = self.runtime.block_on(stream.delete());
        }
    }
}

#[cfg(test)]
mod pulse_backend_tests {
    use super::*;

    #[test]
    #[ignore = "requires a running PulseAudio/PipeWire session"]
    fn live_pulseaudio_rs_streams_capture_and_playback() {
        let mut recorder = PulseRecorder::new(None).expect("create Rust record stream");
        let mut captured = [0_u8; 640];
        recorder
            .read(&mut captured)
            .expect("read one 20 ms audio frame");

        let mut player = PulsePlayer::new(None).expect("create Rust playback stream");
        player
            .write(&[0_u8; 640])
            .expect("queue one silent 20 ms frame");
        std::thread::sleep(Duration::from_millis(100));
        assert!(player.latency_ms().is_some(), "query playback timing");
    }

    #[test]
    #[ignore = "requires a running PulseAudio/PipeWire session; query-only test"]
    fn live_pulse_queries_read_default_source_and_own_capture_stream() {
        let mut connection = pulse_connection().expect("connect to PulseAudio-compatible server");
        let source =
            selected_source_info(&mut connection, None).expect("read default source information");
        assert!(source
            .resolved_name
            .as_deref()
            .is_some_and(|name| !name.is_empty()));
        assert!(source.channels > 0);
        assert!(source.average_volume <= PA_VOLUME_NORM * 3);

        let _own_stream = source_output_for_pid(&mut connection, std::process::id() as i64)
            .expect("query source-output list for current process");
    }

    #[test]
    #[ignore = "requires a running PulseAudio/PipeWire session; sends current values unchanged"]
    fn live_pulse_native_control_roundtrips_preserve_current_source_state() {
        let mut connection = pulse_connection().expect("connect to PulseAudio-compatible server");
        let _: protocol::SinkInfoList = connection
            .roundtrip(protocol::Command::GetSinkInfoList)
            .expect("query sink inventory");
        let _: protocol::CardInfoList = connection
            .roundtrip(protocol::Command::GetCardInfoList)
            .expect("query card and active-profile inventory");
        let inventory = pulse_device_inventory().expect("enumerate audio devices and defaults");
        assert!(
            !inventory.sources.is_empty(),
            "at least one capture source should be listed"
        );
        assert!(
            !inventory.sinks.is_empty(),
            "at least one playback sink should be listed"
        );
        let source = selected_source_info(&mut connection, None).expect("query default source");
        let source_name = CString::new(
            source
                .resolved_name
                .as_deref()
                .expect("resolved source name"),
        )
        .expect("valid source name");
        let current: protocol::SourceInfo = connection
            .roundtrip(protocol::Command::GetSourceInfo(protocol::GetSourceInfo {
                index: None,
                name: Some(source_name),
            }))
            .expect("read current source state");
        connection
            .acknowledge(protocol::Command::SetSourceVolume(
                protocol::SetDeviceVolumeParams {
                    device_index: Some(current.index),
                    device_name: None,
                    volume: current.cvolume,
                },
            ))
            .expect("write unchanged per-channel source volume");
        connection
            .acknowledge(protocol::Command::SetSourceMute(
                protocol::SetDeviceMuteParams {
                    device_index: Some(current.index),
                    device_name: None,
                    mute: current.muted,
                },
            ))
            .expect("write unchanged source mute state");
        let _recorder = PulseRecorder::new(None).expect("create a temporary own capture stream");
        let outputs: protocol::SourceOutputInfoList = connection
            .roundtrip(protocol::Command::GetSourceOutputInfoList)
            .expect("query source-output inventory");
        let own_output = outputs
            .into_iter()
            .find(|output| {
                output
                    .props
                    .get(protocol::Prop::ApplicationProcessId)
                    .and_then(|value| std::str::from_utf8(value).ok())
                    .map(|value| parse_pid_like_atoi(value.trim_end_matches('\0')))
                    == Some(std::process::id() as i64)
            })
            .expect("find this process's capture stream");
        if own_output.has_volume && own_output.volume_writable {
            connection
                .acknowledge(protocol::Command::SetSourceOutputVolume(
                    protocol::SetStreamVolumeParams {
                        index: own_output.index,
                        volume: own_output.cvolume,
                    },
                ))
                .expect("write unchanged per-channel source-output volume");
        }
        connection
            .acknowledge(protocol::Command::SetSourceOutputMute(
                protocol::SetStreamMuteParams {
                    index: own_output.index,
                    mute: own_output.muted,
                },
            ))
            .expect("write unchanged source-output mute state");
        let _own_stream = source_output_for_pid(&mut connection, std::process::id() as i64)
            .expect("query own source-output snapshot");
    }

    #[test]
    fn channel_volume_average_matches_pulse_integer_average() {
        assert_eq!(average_channel_volume(2, &[1, 2]), Some(1));
        assert_eq!(average_channel_volume(3, &[1, 2, 3]), Some(2));
        assert_eq!(average_channel_volume(0, &[1, 2]), None);
        assert_eq!(average_channel_volume(3, &[1, 2]), None);
    }

    #[test]
    fn volume_percentage_conversion_matches_pulse_rounding_and_clamps() {
        assert_eq!(volume_to_percent(0), 0);
        assert_eq!(volume_to_percent(32_768), 50);
        assert_eq!(volume_to_percent(PA_VOLUME_NORM), 100);
        assert_eq!(volume_to_percent(PA_VOLUME_NORM * 2), 150);
        assert_eq!(percent_to_volume(0), 0);
        assert_eq!(percent_to_volume(50), 32_768);
        assert_eq!(percent_to_volume(100), PA_VOLUME_NORM);
        assert_eq!(percent_to_volume(150), PA_VOLUME_NORM * 3 / 2);
    }

    #[test]
    fn pid_parser_matches_atoi_prefix_behavior() {
        assert_eq!(parse_pid_like_atoi("123"), 123);
        assert_eq!(parse_pid_like_atoi("  +123tail"), 123);
        assert_eq!(parse_pid_like_atoi("-2"), -2);
        assert_eq!(parse_pid_like_atoi("not-a-pid"), 0);
    }

    #[test]
    fn invalid_or_default_source_names_resolve_through_server_info() {
        assert!(source_name_uses_default(None));
        assert!(source_name_uses_default(Some("")));
        assert!(source_name_uses_default(Some("@DEFAULT_SOURCE@")));
        assert!(source_name_uses_default(Some("bad\0name")));
        assert!(!source_name_uses_default(Some("alsa_input.test")));
    }

    fn source_name_uses_default(source: Option<&str>) -> bool {
        source
            .filter(|name| {
                !name.is_empty() && !name.as_bytes().contains(&0) && *name != "@DEFAULT_SOURCE@"
            })
            .is_none()
    }
}
