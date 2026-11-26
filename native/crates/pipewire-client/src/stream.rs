//! `PipeWire` stream lifecycle built on the native `ClientNode` protocol.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, BorrowedFd};
use std::time::{Duration, Instant};

use pipewire_native_spa::{
    param::{ParamInfoFlags, ParamType},
    pod::{types::Type, RawPod},
};
use rustix::event::PollFlags;

use crate::audio::{
    encode_audio_buffers_param, encode_audio_format_caps, AudioFormatCaps, AudioSampleFormat,
    NegotiatedAudioFormat,
};
use crate::client::{PipeWireClient, PipeWireObject};
use crate::format::{
    decode_video_format, encode_buffers_io_param, encode_busy_meta_param,
    encode_video_buffers_param, encode_video_format_caps, NegotiatedVideoFormat, VideoFormat,
    VideoFormatCaps,
};
use crate::node::{
    BufferPlane, ClientNode, ClientNodeEvent, IoBuffers, MappedBufferPlane, NodeCommand, NodeInfo,
    ParamInfo, PipeWireBuffer, PortDirection, PortInfo, PortUseBuffers, NODE_CHANGE_FLAGS,
    NODE_CHANGE_PARAMS, NODE_CHANGE_PROPS, NODE_FLAG_ASYNC, NODE_FLAG_RT, PORT_CHANGE_FLAGS,
    PORT_CHANGE_PARAMS, PORT_CHANGE_PROPS,
};
use crate::remote::{PipeWireRemote, PIPEWIRE_ID_ANY};

const INTERFACE_NODE: &str = "PipeWire:Interface:Node";
const PORT_ID: u32 = 0;
const PORT_COUNT: u32 = 2;
const NODE_WAIT_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioPortLayout {
    Single,
    SplitStereo,
}

// Use the SPA ABI definitions from pipewire-native-spa for every parameter
// type/flag it models. Capability and PeerCapability are PipeWire extensions
// not yet represented by that crate's ParamType enum.
const SPA_PARAM_PROP_INFO: u32 = ParamType::PropInfo as u32;
const SPA_PARAM_PROPS: u32 = ParamType::Props as u32;
const SPA_PARAM_ENUM_FORMAT: u32 = ParamType::EnumFormat as u32;
const SPA_PARAM_FORMAT: u32 = ParamType::Format as u32;
const SPA_PARAM_BUFFERS: u32 = ParamType::Buffers as u32;
const SPA_PARAM_META: u32 = ParamType::Meta as u32;
const SPA_PARAM_IO: u32 = ParamType::IO as u32;
const SPA_PARAM_LATENCY: u32 = ParamType::Latency as u32;
const SPA_PARAM_PROCESS_LATENCY: u32 = ParamType::ProcessLatency as u32;
const SPA_PARAM_TAG: u32 = ParamType::Tag as u32;
const SPA_PARAM_CAPABILITY: u32 = 18;
const SPA_PARAM_PEER_CAPABILITY: u32 = 19;
const SPA_PARAM_INFO_READ: u32 = ParamInfoFlags::READ.bits();
const SPA_PARAM_INFO_WRITE: u32 = ParamInfoFlags::WRITE.bits();
const SPA_IO_BUFFERS: u32 = 1;
const SPA_NODE_BUFFERS_FLAG_ALLOC: u32 = 1 << 0;

/// A packed raw-video frame received from a `PipeWire` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    /// Pixel bytes in the negotiated packed `BGRA`, `BGRx`, `YUY2` or `UYVY` format.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: NegotiatedVideoFormat,
}

impl VideoFrame {
    /// Consume this frame's packed bytes, filling the unused X byte for `BGRx`.
    #[must_use]
    pub fn into_bgra(mut self) -> Vec<u8> {
        if self.format.pixel_format == VideoFormat::Bgrx {
            let (pixels, _) = self.data.as_chunks_mut::<4>();
            for pixel in pixels {
                pixel[3] = u8::MAX;
            }
        }
        self.data
    }
}

struct PeerActivation {
    memory_id: u32,
    offset: u64,
    size: u32,
    signal_fd: File,
}

/// Properties used when creating a local `PipeWire` stream node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOptions {
    /// Human-readable node name. Defaults to `PipeWire Stream`.
    pub name: String,
    /// Additional node properties; these override defaults with matching keys.
    pub node_properties: Vec<(String, String)>,
    /// Properties published on the native `PipeWire` Client object.
    pub client_properties: Vec<(String, String)>,
    /// Explicitly create a graph link to the selected target node.
    pub auto_link: bool,
    /// Explicit remote node global ID to capture from.
    pub target_node_id: Option<u32>,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            name: "PipeWire Stream".into(),
            node_properties: Vec::new(),
            client_properties: Vec::new(),
            auto_link: false,
            target_node_id: None,
        }
    }
}

/// A native `PipeWire` stream attached to a portal-provided remote and node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamMedia {
    Video,
    Audio,
}

#[derive(Clone, Copy)]
struct StreamConfiguration {
    media: StreamMedia,
    port_direction: PortDirection,
    audio_default_format: Option<NegotiatedAudioFormat>,
    split_audio_ports: bool,
}

/// Interleaved PCM bytes from a raw-audio `PipeWire` stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub data: Vec<u8>,
    pub format: NegotiatedAudioFormat,
}

/// Public stream lifecycle states corresponding to `pw_stream_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeWireStreamState {
    Unconnected,
    Connecting,
    Paused,
    Streaming,
}

pub struct PipeWireStream {
    client: PipeWireClient,
    client_node: ClientNode,
    target_global_id: u32,
    graph_links: Vec<PipeWireObject>,
    port_info: PortInfo,
    port_buffers: Vec<PipeWireBuffer>,
    second_port_buffers: Vec<PipeWireBuffer>,
    audio_port_layout: AudioPortLayout,
    audio_enum_format: Option<Vec<u8>>,
    audio_io_param: Option<Vec<u8>>,
    audio_busy_param: Option<Vec<u8>>,
    io: Option<(u32, u32, u32)>,
    second_io: Option<(u32, u32, u32)>,
    pending_left: Option<Vec<u8>>,
    pending_right: Option<Vec<u8>>,
    transport_read: Option<File>,
    transport_write: Option<File>,
    activation_signals: Vec<PeerActivation>,
    format: Option<NegotiatedVideoFormat>,
    audio_format: Option<NegotiatedAudioFormat>,
    audio_default_format: Option<NegotiatedAudioFormat>,
    media: StreamMedia,
    port_direction: PortDirection,
    active: bool,
    streaming: bool,
    disconnected: bool,
}

impl PipeWireStream {
    /// Create a screen capture stream using the native `PipeWire` SPA data path.
    pub fn connect(remote: PipeWireRemote) -> io::Result<Self> {
        Self::connect_with_options(
            remote,
            &VideoFormatCaps::default(),
            &StreamOptions::default(),
        )
    }

    /// Create a raw-video capture stream with caller-supplied SPA capabilities.
    pub fn connect_with_caps(remote: PipeWireRemote, caps: &VideoFormatCaps) -> io::Result<Self> {
        Self::connect_with_options(remote, caps, &StreamOptions::default())
    }

    /// Create a raw-video capture stream with caller-supplied stream properties.
    pub fn connect_with_options(
        remote: PipeWireRemote,
        caps: &VideoFormatCaps,
        options: &StreamOptions,
    ) -> io::Result<Self> {
        Self::connect_with_cancellation_and_options(remote, caps, options, || false)
    }

    /// Create a raw-video capture stream and publish application properties.
    pub fn connect_with_caps_and_client_properties(
        remote: PipeWireRemote,
        caps: &VideoFormatCaps,
        client_properties: &[(String, String)],
    ) -> io::Result<Self> {
        let options = StreamOptions {
            client_properties: client_properties.to_vec(),
            ..StreamOptions::default()
        };
        Self::connect_with_options(remote, caps, &options)
    }

    /// Create a stream while allowing the caller to cancel portal setup.
    pub fn connect_with_cancellation<F>(
        remote: PipeWireRemote,
        caps: &VideoFormatCaps,
        client_properties: &[(String, String)],
        cancelled: F,
    ) -> io::Result<Self>
    where
        F: FnMut() -> bool,
    {
        let options = StreamOptions {
            client_properties: client_properties.to_vec(),
            ..StreamOptions::default()
        };
        Self::connect_with_cancellation_and_options(remote, caps, &options, cancelled)
    }

    /// Create a stream with custom properties and a cancellation predicate.
    pub fn connect_with_cancellation_and_options<F>(
        remote: PipeWireRemote,
        caps: &VideoFormatCaps,
        options: &StreamOptions,
        cancelled: F,
    ) -> io::Result<Self>
    where
        F: FnMut() -> bool,
    {
        let enum_format = encode_video_format_caps(caps)?;
        Self::connect_internal(
            remote,
            &enum_format,
            options,
            cancelled,
            StreamConfiguration {
                media: StreamMedia::Video,
                port_direction: PortDirection::Input,
                audio_default_format: None,
                split_audio_ports: false,
            },
        )
    }

    /// Connect a raw-audio capture stream over a portal-provided `PipeWire` remote.
    pub fn connect_audio(remote: PipeWireRemote) -> io::Result<Self> {
        Self::connect_audio_with_options(
            remote,
            &AudioFormatCaps::default(),
            &StreamOptions::default(),
        )
    }

    /// Connect raw audio with explicit format capabilities and stream properties.
    pub fn connect_audio_with_options(
        remote: PipeWireRemote,
        caps: &AudioFormatCaps,
        options: &StreamOptions,
    ) -> io::Result<Self> {
        Self::connect_audio_with_cancellation_and_options(remote, caps, options, || false)
    }

    /// Connect audio while allowing portal setup to be cancelled by the caller.
    pub fn connect_audio_with_cancellation_and_options<F>(
        remote: PipeWireRemote,
        caps: &AudioFormatCaps,
        options: &StreamOptions,
        cancelled: F,
    ) -> io::Result<Self>
    where
        F: FnMut() -> bool,
    {
        let mut options = options.clone();
        for (key, value) in [
            ("media.type", "Audio"),
            ("media.category", "Capture"),
            ("media.role", "Communication"),
            ("media.class", "Stream/Input/Audio"),
        ] {
            if let Some((_, current)) = options
                .node_properties
                .iter_mut()
                .find(|(name, _)| name == key)
            {
                *current = value.into();
            } else {
                options.node_properties.push((key.into(), value.into()));
            }
        }
        let enum_format = encode_audio_format_caps(caps)?;
        Self::connect_internal(
            remote,
            &enum_format,
            &options,
            cancelled,
            StreamConfiguration {
                media: StreamMedia::Audio,
                port_direction: PortDirection::Input,
                audio_default_format: Some(NegotiatedAudioFormat {
                    sample_format: caps.default,
                    rate: caps.rate.0,
                    channels: caps.channels.0,
                }),
                split_audio_ports: caps.default == AudioSampleFormat::F32P,
            },
        )
    }

    /// Connect a native PCM playback stream using a local `PipeWire` remote.
    pub fn connect_audio_playback(
        remote: PipeWireRemote,
        caps: &AudioFormatCaps,
        options: &StreamOptions,
    ) -> io::Result<Self> {
        let mut options = options.clone();
        for (key, value) in [
            ("media.type", "Audio"),
            ("media.category", "Playback"),
            ("media.role", "Communication"),
            ("media.class", "Stream/Output/Audio"),
        ] {
            if let Some((_, current)) = options
                .node_properties
                .iter_mut()
                .find(|(name, _)| name == key)
            {
                *current = value.into();
            } else {
                options.node_properties.push((key.into(), value.into()));
            }
        }
        let enum_format = encode_audio_format_caps(caps)?;
        Self::connect_internal(
            remote,
            &enum_format,
            &options,
            || false,
            StreamConfiguration {
                media: StreamMedia::Audio,
                port_direction: PortDirection::Output,
                audio_default_format: Some(NegotiatedAudioFormat {
                    sample_format: caps.default,
                    rate: caps.rate.0,
                    channels: caps.channels.0,
                }),
                split_audio_ports: caps.default == AudioSampleFormat::F32P && caps.channels.0 > 1,
            },
        )
    }

    fn connect_internal<F>(
        remote: PipeWireRemote,
        enum_format: &[u8],
        options: &StreamOptions,
        mut cancelled: F,
        config: StreamConfiguration,
    ) -> io::Result<Self>
    where
        F: FnMut() -> bool,
    {
        let StreamConfiguration {
            media,
            port_direction,
            audio_default_format,
            split_audio_ports,
        } = config;
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "PipeWire stream setup was cancelled",
            ));
        }
        let target_node_id = options.target_node_id.unwrap_or_else(|| remote.node_id());
        let mut client =
            PipeWireClient::connect_with_properties(remote, &options.client_properties)?;
        client.list_globals_with_cancellation(NODE_WAIT_TIMEOUT, &mut cancelled)?;
        let target_global_id = resolve_target_global(&mut client, target_node_id, &mut cancelled)?;

        let (node_info, port_info) = stream_info(
            target_node_id,
            options,
            media,
            port_direction,
            split_audio_ports,
        );
        let client_node =
            ClientNode::create(&mut client, &node_properties(target_node_id, options))?;
        client_node.update(&mut client, &node_info, &[])?;

        let mut stream = Self {
            client,
            client_node,
            target_global_id,
            graph_links: Vec::new(),
            port_info,
            port_buffers: Vec::new(),
            second_port_buffers: Vec::new(),
            audio_port_layout: if split_audio_ports {
                AudioPortLayout::SplitStereo
            } else {
                AudioPortLayout::Single
            },
            audio_enum_format: (media == StreamMedia::Audio).then_some(enum_format.to_vec()),
            audio_io_param: (media == StreamMedia::Audio).then_some(encode_buffers_io_param()?),
            audio_busy_param: (media == StreamMedia::Audio).then_some(encode_busy_meta_param()?),
            io: None,
            second_io: None,
            pending_left: None,
            pending_right: None,
            transport_read: None,
            transport_write: None,
            activation_signals: Vec::new(),
            format: None,
            audio_format: None,
            audio_default_format,
            media,
            port_direction,
            active: false,
            streaming: false,
            disconnected: false,
        };

        // PipeWire first installs the activation transport, then the stream
        // marks its node active and publishes the initial port params.
        let deadline = Instant::now() + NODE_WAIT_TIMEOUT;
        while stream.transport_read.is_none() {
            if cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "PipeWire stream setup was cancelled",
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Timed out waiting for PipeWire ClientNode transport",
                ));
            }
            stream.wait_and_dispatch(remaining.min(Duration::from_millis(50)))?;
        }

        stream.client_node.set_active(&mut stream.client, true)?;
        stream.active = true;

        stream.configure_ports(enum_format)?;
        if target_global_id != 0 && options.auto_link {
            stream.create_graph_link(options)?;
        }
        Ok(stream)
    }

    fn configure_ports(&mut self, enum_format: &[u8]) -> io::Result<()> {
        let io_param = encode_buffers_io_param()?;
        let busy_param = encode_busy_meta_param()?;
        self.port_info.change_mask =
            u64::from(PORT_CHANGE_FLAGS | PORT_CHANGE_PROPS | PORT_CHANGE_PARAMS);
        self.client_node.update_port(
            &mut self.client,
            self.port_direction,
            PORT_ID,
            &self.port_info,
            &[enum_format.to_vec(), io_param.clone(), busy_param.clone()],
        )?;
        if self.audio_port_layout == AudioPortLayout::SplitStereo {
            let mut right_port = self.port_info.clone();
            for (key, value) in &mut right_port.properties {
                match key.as_str() {
                    "audio.channel" => *value = "FR".into(),
                    "port.name" => {
                        *value = if self.port_direction == PortDirection::Input {
                            "input_1"
                        } else {
                            "output_1"
                        }
                        .into();
                    }
                    "port.alias" => {
                        *value = value.replace(
                            if self.port_direction == PortDirection::Input {
                                "input_0"
                            } else {
                                "output_0"
                            },
                            if self.port_direction == PortDirection::Input {
                                "input_1"
                            } else {
                                "output_1"
                            },
                        );
                    }
                    _ => {}
                }
            }
            self.client_node.update_port(
                &mut self.client,
                self.port_direction,
                1,
                &right_port,
                &[enum_format.to_vec(), io_param, busy_param],
            )?;
        }
        Ok(())
    }

    fn create_graph_link(&mut self, options: &StreamOptions) -> io::Result<()> {
        let deadline = Instant::now() + NODE_WAIT_TIMEOUT;
        let stream_node = loop {
            if let Some(global) = self.client.list_globals()?.into_iter().find(|global| {
                global.interface == INTERFACE_NODE
                    && global.property("node.name") == Some(options.name.as_str())
            }) {
                break global;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "PipeWire did not publish the local stream node",
                ));
            }
            self.wait_and_dispatch(remaining.min(Duration::from_millis(50)))?;
        };
        let globals = self.client.refresh_globals()?;
        let stream_ports = globals
            .iter()
            .filter(|global| {
                global.interface == "PipeWire:Interface:Port"
                    && global
                        .property("node.id")
                        .and_then(|value| value.parse::<u32>().ok())
                        == Some(stream_node.id)
                    && global
                        .property("port.id")
                        .and_then(|value| value.parse::<u32>().ok())
                        .is_some_and(|port_id| {
                            port_id <= u32::from(self.media == StreamMedia::Audio)
                        })
                    && global.property("port.direction")
                        == Some(match self.port_direction {
                            PortDirection::Input => "in",
                            PortDirection::Output => "out",
                        })
            })
            .collect::<Vec<_>>();
        if stream_ports.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "PipeWire did not publish local stream ports",
            ));
        }
        let target_direction = match self.port_direction {
            PortDirection::Input => "out",
            PortDirection::Output => "in",
        };
        let target_ports = globals
            .iter()
            .filter(|global| {
                global.interface == "PipeWire:Interface:Port"
                    && global
                        .property("node.id")
                        .and_then(|value| value.parse::<u32>().ok())
                        == Some(self.target_global_id)
                    && global.property("port.direction") == Some(target_direction)
            })
            .collect::<Vec<_>>();
        if target_ports.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "PipeWire target node has no compatible ports",
            ));
        }
        for target_port in target_ports {
            let target_port_id = target_port.property("port.id");
            let stream_port = stream_ports
                .iter()
                .find(|port| {
                    matches!(self.media, StreamMedia::Video)
                        || port.property("port.id") == target_port_id
                })
                .unwrap_or(&stream_ports[0]);
            let properties = graph_link_properties(
                self.port_direction,
                self.target_global_id,
                target_port.id,
                stream_node.id,
                stream_port.id,
            );
            self.graph_links.push(self.client.create_object(
                "link-factory",
                "PipeWire:Interface:Link",
                3,
                &properties,
            )?);
        }
        self.client.sync()?;
        Ok(())
    }

    /// Return the fixed SPA format accepted by the remote node, once negotiated.
    #[must_use]
    pub const fn format(&self) -> Option<NegotiatedVideoFormat> {
        self.format
    }

    /// Return the fixed raw-audio format accepted by the remote node.
    #[must_use]
    pub const fn audio_format(&self) -> Option<NegotiatedAudioFormat> {
        self.audio_format
    }

    /// Wait for and return one interleaved raw-audio buffer.
    pub fn next_audio_frame(&mut self, timeout: Duration) -> io::Result<Option<AudioFrame>> {
        if self.media != StreamMedia::Audio {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Stream was not opened for audio",
            ));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.dequeue_audio_frame()? {
                return Ok(Some(frame));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            self.wait_and_dispatch(remaining)?;
        }
    }

    /// Current lifecycle state, corresponding to `pw_stream_get_state`.
    #[must_use]
    pub fn state(&self) -> PipeWireStreamState {
        if self.disconnected {
            PipeWireStreamState::Unconnected
        } else if self.streaming {
            PipeWireStreamState::Streaming
        } else if self.port_buffers.is_empty()
            || (self.audio_port_layout == AudioPortLayout::SplitStereo
                && self.second_port_buffers.is_empty())
        {
            PipeWireStreamState::Connecting
        } else {
            PipeWireStreamState::Paused
        }
    }

    /// Activate or deactivate processing, corresponding to `pw_stream_set_active`.
    pub fn set_active(&mut self, active: bool) -> io::Result<()> {
        if self.disconnected {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "PipeWire stream is disconnected",
            ));
        }
        if self.active == active {
            return Ok(());
        }
        self.client_node.set_active(&mut self.client, active)?;
        self.active = active;
        if !active {
            self.streaming = false;
        }
        Ok(())
    }

    /// Whether the remote graph has sent the SPA Start command.
    #[must_use]
    pub const fn is_streaming(&self) -> bool {
        self.streaming
    }

    /// Borrow the optional transport write eventfd, used by `PipeWire` profiling.
    #[must_use]
    pub fn transport_write_fd(&self) -> Option<BorrowedFd<'_>> {
        self.transport_write.as_ref().map(AsFd::as_fd)
    }

    /// Wait for and return one captured frame, or None when the timeout expires.
    pub fn next_frame(&mut self, timeout: Duration) -> io::Result<Option<VideoFrame>> {
        if self.media != StreamMedia::Video {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Stream was not opened for video",
            ));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(frame) = self.dequeue_frame()? {
                return Ok(Some(frame));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            self.wait_and_dispatch(remaining)?;
        }
    }

    fn available_output_buffer(&self, requested: u32) -> io::Result<Option<usize>> {
        let is_available = |index: usize| -> io::Result<bool> {
            self.port_buffers[index]
                .busy_count(&self.client)
                .map(|busy| busy.unwrap_or(0) == 0)
        };
        if let Ok(index) = usize::try_from(requested) {
            if index < self.port_buffers.len() && is_available(index)? {
                return Ok(Some(index));
            }
        }
        for index in 0..self.port_buffers.len() {
            if is_available(index)? {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    /// Queue one raw PCM frame on an output port, waiting for a free `PipeWire` buffer.
    pub fn queue_audio_frame(&mut self, data: &[u8], timeout: Duration) -> io::Result<bool> {
        if self.media != StreamMedia::Audio || self.port_direction != PortDirection::Output {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Stream was not opened for audio playback",
            ));
        }
        let deadline = Instant::now() + timeout;
        loop {
            if self.streaming {
                if let Some(format) = self.audio_format {
                    if format.sample_format == AudioSampleFormat::F32P && format.channels != 1 {
                        return Err(io::Error::new(
                            io::ErrorKind::Unsupported,
                            "Multi-channel planar playback requires the multi-plane queue API",
                        ));
                    }
                    let frame_stride = format
                        .channels
                        .checked_mul(match format.sample_format {
                            AudioSampleFormat::S16Le => 2,
                            AudioSampleFormat::F32Le | AudioSampleFormat::F32P => 4,
                        })
                        .ok_or_else(|| invalid_data("PipeWire playback stride overflow"))?;
                    if !data
                        .len()
                        .is_multiple_of(usize::try_from(frame_stride).unwrap_or(usize::MAX))
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "PCM data does not contain a whole number of audio frames",
                        ));
                    }
                }
                if let Some((memory_id, offset, size)) = self.io {
                    let io = IoBuffers::read(&self.client, memory_id, offset, size)?;
                    if matches!(
                        io.status,
                        IoBuffers::STATUS_OK | IoBuffers::STATUS_NEED_DATA
                    ) {
                        let index = self.available_output_buffer(io.buffer_id)?;
                        if let Some(index) = index {
                            let format = self.audio_format.ok_or_else(|| {
                                invalid_data("PipeWire playback format is not negotiated")
                            })?;
                            let frame_stride = format
                                .channels
                                .checked_mul(match format.sample_format {
                                    AudioSampleFormat::S16Le => 2,
                                    AudioSampleFormat::F32Le | AudioSampleFormat::F32P => 4,
                                })
                                .ok_or_else(|| invalid_data("PipeWire playback stride overflow"))?;
                            let buffer = &self.port_buffers[index];
                            buffer.acquire(&self.client)?;
                            if let Err(error) = buffer.write_plane(
                                &self.client,
                                0,
                                data,
                                i32::try_from(frame_stride)
                                    .map_err(|_| invalid_data("PCM stride exceeds SPA ABI"))?,
                            ) {
                                let _ = buffer.release(&self.client);
                                return Err(error);
                            }
                            IoBuffers::write(
                                &self.client,
                                memory_id,
                                offset,
                                IoBuffers::STATUS_HAVE_DATA,
                                u32::try_from(index)
                                    .map_err(|_| invalid_data("PipeWire buffer index overflow"))?,
                            )?;
                            self.trigger_peer_activations()?;
                            return Ok(true);
                        }
                    }
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(false);
            }
            self.wait_and_dispatch(remaining.min(Duration::from_millis(20)))?;
        }
    }

    /// Stop the remote node and release its server-side `ClientNode` resource.
    pub fn disconnect(&mut self) -> io::Result<()> {
        if self.disconnected {
            return Ok(());
        }
        self.disconnected = true;
        self.streaming = false;
        let mut first_error = if self.active {
            self.active = false;
            self.client_node.set_active(&mut self.client, false).err()
        } else {
            None
        };

        // Explicit links outlive the ClientNode proxy. Destroy every one before
        // tearing down the stream, even if an earlier destroy request failed.
        for link in self.graph_links.drain(..) {
            if let Err(error) = self.client.destroy_object(&link) {
                first_error.get_or_insert(error);
            }
        }
        if let Err(error) = self.client.destroy_object(self.client_node.object()) {
            first_error.get_or_insert(error);
        }
        if let Err(error) = self.client.sync().map(|_| ()) {
            first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)
    }

    fn wait_and_dispatch(&mut self, timeout: Duration) -> io::Result<()> {
        let additional = self
            .transport_read
            .as_ref()
            .map(AsFd::as_fd)
            .into_iter()
            .collect::<Vec<_>>();
        let ready = self.client.poll_readable(&additional, Some(timeout))?;

        if ready.first().is_some_and(|flags| {
            flags.intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "PipeWire native-protocol socket disconnected",
            ));
        }
        if ready
            .first()
            .is_some_and(|flags| flags.contains(PollFlags::IN))
        {
            self.dispatch_socket_messages()?;
        }
        if ready
            .get(1)
            .is_some_and(|flags| flags.contains(PollFlags::IN))
        {
            self.drain_transport_eventfd()?;
        }
        Ok(())
    }

    fn dispatch_socket_messages(&mut self) -> io::Result<()> {
        while let Some(message) = self.client.receive_timeout(Duration::ZERO)? {
            if message.header.id == self.client_node.object().id {
                let event = self.client_node.decode_event(message)?;
                self.handle_client_node_event(event)?;
            } else if message.header.id == 2 {
                if message.header.opcode == 1 {
                    let id = decode_registry_global_remove(&message.payload)?;
                    if id == self.target_global_id {
                        return Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            "Portal PipeWire capture source disappeared",
                        ));
                    }
                }
                self.client.handle_registry_message(&message)?;
            }
        }
        Ok(())
    }

    fn handle_client_node_event(&mut self, event: ClientNodeEvent) -> io::Result<()> {
        match event {
            ClientNodeEvent::Transport {
                read_fd, write_fd, ..
            } => {
                self.transport_read = Some(File::from(read_fd));
                self.transport_write = write_fd.map(File::from);
            }
            ClientNodeEvent::SetActivation {
                signal_fd,
                memory_id,
                offset,
                size,
                ..
            } => {
                if let Some(signal_fd) = signal_fd {
                    self.activation_signals.push(PeerActivation {
                        memory_id,
                        offset: u64::from(offset),
                        size,
                        signal_fd: File::from(signal_fd),
                    });
                }
            }
            command @ ClientNodeEvent::Command(_) => match command.node_command()? {
                NodeCommand::Start => self.streaming = true,
                NodeCommand::Pause | NodeCommand::Suspend => self.streaming = false,
                NodeCommand::Enable
                | NodeCommand::Disable
                | NodeCommand::Flush
                | NodeCommand::Drain
                | NodeCommand::Marker
                | NodeCommand::ParamBegin
                | NodeCommand::ParamEnd
                | NodeCommand::RequestProcess
                | NodeCommand::User => {}
            },
            ClientNodeEvent::PortSetParam {
                direction,
                port_id,
                id,
                param,
                ..
            } => {
                if direction == self.port_direction && port_id <= 1 {
                    self.apply_port_param(port_id, id, param.data())?;
                }
            }
            ClientNodeEvent::PortUseBuffers(buffers) => self.install_buffers(buffers)?,
            ClientNodeEvent::PortSetIo {
                direction,
                port_id,
                id,
                memory_id,
                offset,
                size,
                ..
            } => {
                if direction == self.port_direction && port_id <= 1 && id == SPA_IO_BUFFERS {
                    if size == 0 {
                        return Ok(());
                    }
                    if size < 8 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("PipeWire SPA_IO_Buffers area is too small: {size} bytes"),
                        ));
                    }
                    let io = Some((memory_id, offset, size));
                    if port_id == PORT_ID {
                        self.io = io;
                    } else {
                        self.second_io = io;
                    }
                }
            }
            ClientNodeEvent::RemovePort {
                direction,
                port_id: PORT_ID,
            } if direction == self.port_direction => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "PipeWire removed the local stream port",
                ));
            }
            ClientNodeEvent::AddPort {
                direction,
                port_id,
                properties,
            } if direction == self.port_direction => self.handle_add_port(port_id, properties)?,
            ClientNodeEvent::SetIo { .. }
            | ClientNodeEvent::SetParam { .. }
            | ClientNodeEvent::Event(_)
            | ClientNodeEvent::AddPort { .. }
            | ClientNodeEvent::RemovePort { .. }
            | ClientNodeEvent::PortSetMixInfo { .. } => {}
        }
        Ok(())
    }

    fn handle_add_port(
        &mut self,
        port_id: u32,
        properties: Vec<(String, String)>,
    ) -> io::Result<()> {
        if self.audio_port_layout != AudioPortLayout::SplitStereo || port_id != 1 {
            return Ok(());
        }
        let mut info = self.port_info.clone();
        info.properties = properties;
        info.change_mask = u64::from(PORT_CHANGE_FLAGS | PORT_CHANGE_PROPS | PORT_CHANGE_PARAMS);
        let params = [
            self.audio_enum_format
                .as_ref()
                .ok_or_else(|| invalid_data("Missing audio EnumFormat"))?
                .clone(),
            self.audio_io_param
                .as_ref()
                .ok_or_else(|| invalid_data("Missing audio IO param"))?
                .clone(),
            self.audio_busy_param
                .as_ref()
                .ok_or_else(|| invalid_data("Missing audio busy metadata"))?
                .clone(),
        ];
        self.client_node.update_port(
            &mut self.client,
            self.port_direction,
            port_id,
            &info,
            &params,
        )
    }

    fn apply_port_param(&mut self, port_id: u32, id: u32, param: &[u8]) -> io::Result<()> {
        let mut params = vec![param.to_vec()];
        if id == SPA_PARAM_FORMAT {
            let pod = RawPod::wrap(param).map_err(pod_error("Parse negotiated SPA Format"))?;
            if pod.type_() == Type::None {
                // PipeWire sends an empty SPA None POD when clearing a prior
                // format during renegotiation; acknowledge it without decoding.
                self.format = None;
                self.audio_format = None;
            } else {
                match self.media {
                    StreamMedia::Video => {
                        let negotiated = decode_video_format(&pod)?;
                        params.push(encode_video_buffers_param(negotiated)?);
                        self.format = Some(negotiated);
                    }
                    StreamMedia::Audio => {
                        let negotiated = crate::audio::decode_audio_format_with_defaults(
                            &pod,
                            self.audio_default_format,
                        )?;
                        params.push(encode_audio_buffers_param(negotiated)?);
                        self.audio_format = Some(negotiated);
                    }
                }
            }
        }

        let mut info = self.port_info.clone();
        if port_id == 1 {
            for (key, value) in &mut info.properties {
                if key == "audio.channel" {
                    *value = "FR".into();
                }
                if key == "port.name" {
                    *value = if self.port_direction == PortDirection::Input {
                        "input_1"
                    } else {
                        "output_1"
                    }
                    .into();
                }
                if key == "port.alias" {
                    *value = value.replace(
                        if self.port_direction == PortDirection::Input {
                            "input_0"
                        } else {
                            "output_0"
                        },
                        if self.port_direction == PortDirection::Input {
                            "input_1"
                        } else {
                            "output_1"
                        },
                    );
                }
            }
        }
        if let Some(item) = info.params.iter_mut().find(|item| item.id == id) {
            item.flags |= SPA_PARAM_INFO_READ;
        }
        if id == SPA_PARAM_FORMAT {
            if let Some(item) = info
                .params
                .iter_mut()
                .find(|item| item.id == SPA_PARAM_BUFFERS)
            {
                item.flags |= SPA_PARAM_INFO_READ;
            }
        }
        info.change_mask = u64::from(PORT_CHANGE_PARAMS);
        self.client_node.update_port(
            &mut self.client,
            self.port_direction,
            port_id,
            &info,
            &params,
        )
    }

    fn install_buffers(&mut self, buffers: PortUseBuffers) -> io::Result<()> {
        if buffers.direction != self.port_direction
            || buffers.port_id > u32::from(self.audio_port_layout == AudioPortLayout::SplitStereo)
        {
            return Err(invalid_data(
                "PipeWire supplied buffers for an unexpected port",
            ));
        }
        if buffers.flags & SPA_NODE_BUFFERS_FLAG_ALLOC != 0 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire requested client-allocated screen buffers",
            ));
        }
        if buffers.buffers.is_empty() {
            if buffers.port_id == PORT_ID {
                self.port_buffers.clear();
            } else {
                self.second_port_buffers.clear();
            }
            return Ok(());
        }
        if buffers.buffers.iter().any(|buffer| buffer.data.is_empty()) {
            return Err(invalid_data("PipeWire screen buffer has no data planes"));
        }
        if buffers.port_id == PORT_ID {
            self.port_buffers = buffers.buffers;
        } else {
            self.second_port_buffers = buffers.buffers;
        }
        Ok(())
    }

    fn drain_transport_eventfd(&mut self) -> io::Result<()> {
        if let Some(fd) = self.transport_read.as_mut() {
            let mut counter = [0_u8; 8];
            fd.read_exact(&mut counter)?;
        }
        Ok(())
    }

    fn trigger_peer_activations(&self) -> io::Result<()> {
        for activation in &self.activation_signals {
            self.client.trigger_activation(
                activation.memory_id,
                activation.offset,
                activation.size,
                activation.signal_fd.as_fd(),
            )?;
        }
        Ok(())
    }

    fn dequeue_audio_frame(&mut self) -> io::Result<Option<AudioFrame>> {
        if !self.streaming || self.audio_format.is_none() || self.port_buffers.is_empty() {
            return Ok(None);
        }
        if self.audio_port_layout == AudioPortLayout::Single {
            let Some(data) = dequeue_audio_port(&self.client, &self.port_buffers, self.io)? else {
                return Ok(None);
            };
            self.trigger_peer_activations()?;
            let format = self
                .audio_format
                .ok_or_else(|| invalid_data("PipeWire audio format was not negotiated"))?;
            return Ok(Some(AudioFrame { data, format }));
        }
        if self.second_port_buffers.is_empty() {
            return Ok(None);
        }
        if self.pending_left.is_none() {
            self.pending_left = dequeue_audio_port(&self.client, &self.port_buffers, self.io)?;
            if self.pending_left.is_some() {
                self.trigger_peer_activations()?;
            }
        }
        if self.pending_right.is_none() {
            self.pending_right =
                dequeue_audio_port(&self.client, &self.second_port_buffers, self.second_io)?;
            if self.pending_right.is_some() {
                self.trigger_peer_activations()?;
            }
        }
        if self.pending_left.is_none() || self.pending_right.is_none() {
            return Ok(None);
        }
        let left = self
            .pending_left
            .take()
            .ok_or_else(|| invalid_data("Missing left PipeWire audio buffer"))?;
        let right = self
            .pending_right
            .take()
            .ok_or_else(|| invalid_data("Missing right PipeWire audio buffer"))?;
        let sample_count = (left.len() / 4).min(right.len() / 4);
        let mut data = Vec::with_capacity(sample_count * 8);
        for index in 0..sample_count {
            let offset = index * 4;
            data.extend_from_slice(&left[offset..offset + 4]);
            data.extend_from_slice(&right[offset..offset + 4]);
        }
        let format = self
            .audio_format
            .ok_or_else(|| invalid_data("PipeWire audio format was not negotiated"))?;
        let format = NegotiatedAudioFormat {
            sample_format: AudioSampleFormat::F32Le,
            channels: 2,
            ..format
        };
        Ok(Some(AudioFrame { data, format }))
    }

    fn dequeue_frame(&mut self) -> io::Result<Option<VideoFrame>> {
        if !self.streaming || self.format.is_none() || self.port_buffers.is_empty() {
            return Ok(None);
        }
        let Some((memory_id, offset, size)) = self.io else {
            return Ok(None);
        };
        let io = IoBuffers::read(&self.client, memory_id, offset, size)?;
        if io.status != IoBuffers::STATUS_HAVE_DATA {
            return Ok(None);
        }
        let index = usize::try_from(io.buffer_id)
            .map_err(|_| invalid_data("PipeWire returned a negative buffer ID"))?;
        let buffer = self
            .port_buffers
            .get(index)
            .ok_or_else(|| invalid_data("PipeWire returned an out-of-range buffer ID"))?;

        // pw_stream's input callback increments Busy before dequeue, then
        // queue_buffer decrements Busy and puts the same buffer back.
        let format = self
            .format
            .ok_or_else(|| invalid_data("PipeWire screen format was not negotiated"))?;
        buffer.acquire(&self.client)?;
        // Prefer a mapped view so the raw shared-memory frame is copied only
        // once, directly into the packed output frame. Keep the buffer acquired
        // until that copy finishes; PipeWire may reuse it as soon as it is queued.
        let frame_result = match buffer.map_plane(&self.client, 0) {
            Ok(plane) => pack_mapped_video_plane(&plane, format),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => buffer
                .read_plane(&self.client, 0)
                .and_then(|plane| pack_video_plane(&plane, format)),
            Err(error) => Err(error),
        };
        let release_result = buffer.release(&self.client);
        let queue_result = IoBuffers::write(
            &self.client,
            memory_id,
            offset,
            IoBuffers::STATUS_NEED_DATA,
            io.buffer_id,
        );

        let frame = frame_result?;
        release_result?;
        queue_result?;
        self.trigger_peer_activations()?;
        Ok(Some(frame))
    }
}

impl Drop for PipeWireStream {
    fn drop(&mut self) {
        let _ = self.disconnect();
    }
}

fn dequeue_audio_port(
    client: &PipeWireClient,
    buffers: &[PipeWireBuffer],
    io_location: Option<(u32, u32, u32)>,
) -> io::Result<Option<Vec<u8>>> {
    let Some((memory_id, offset, size)) = io_location else {
        return Ok(None);
    };
    let io = IoBuffers::read(client, memory_id, offset, size)?;
    if io.status != IoBuffers::STATUS_HAVE_DATA {
        return Ok(None);
    }
    let index = usize::try_from(io.buffer_id)
        .map_err(|_| invalid_data("PipeWire returned a negative buffer ID"))?;
    let buffer = buffers
        .get(index)
        .ok_or_else(|| invalid_data("PipeWire returned an out-of-range buffer ID"))?;
    buffer.acquire(client)?;
    let frame_result = match buffer.map_plane(client, 0) {
        Ok(plane) => {
            let (first, second) = plane.data.as_slices();
            let mut bytes = Vec::with_capacity(plane.data.len());
            bytes.extend_from_slice(first);
            if let Some(second) = second {
                bytes.extend_from_slice(second);
            }
            Ok(bytes)
        }
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            buffer.read_plane(client, 0).map(|plane| plane.bytes)
        }
        Err(error) => Err(error),
    };
    let release_result = buffer.release(client);
    let queue_result = IoBuffers::write(
        client,
        memory_id,
        offset,
        IoBuffers::STATUS_NEED_DATA,
        io.buffer_id,
    );
    let data = frame_result?;
    release_result?;
    queue_result?;
    Ok(Some(data))
}

fn resolve_target_global<F>(
    client: &mut PipeWireClient,
    target_node_id: u32,
    cancelled: &mut F,
) -> io::Result<u32>
where
    F: FnMut() -> bool,
{
    if target_node_id == PIPEWIRE_ID_ANY {
        return Ok(0);
    }
    let deadline = Instant::now() + NODE_WAIT_TIMEOUT;
    loop {
        if cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "PipeWire stream setup was cancelled",
            ));
        }
        if let Some(global) = client.wait_for_global(target_node_id, Duration::from_millis(50))? {
            if global.interface != INTERFACE_NODE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("PipeWire global {target_node_id} is not a Node"),
                ));
            }
            return Ok(global.id);
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("PipeWire node {target_node_id} was not advertised"),
            ));
        }
    }
}

fn graph_link_properties(
    direction: PortDirection,
    target_node: u32,
    target_port: u32,
    stream_node: u32,
    stream_port: u32,
) -> [(String, String); 4] {
    let (output_node, output_port, input_node, input_port) = match direction {
        PortDirection::Input => (target_node, target_port, stream_node, stream_port),
        PortDirection::Output => (stream_node, stream_port, target_node, target_port),
    };
    [
        ("link.output.node".into(), output_node.to_string()),
        ("link.output.port".into(), output_port.to_string()),
        ("link.input.node".into(), input_node.to_string()),
        ("link.input.port".into(), input_port.to_string()),
    ]
}

fn stream_info(
    target_node_id: u32,
    options: &StreamOptions,
    media: StreamMedia,
    port_direction: PortDirection,
    split_audio_ports: bool,
) -> (NodeInfo, PortInfo) {
    let port_count = if split_audio_ports { PORT_COUNT } else { 1 };
    let node_info = NodeInfo {
        max_input_ports: if port_direction == PortDirection::Input {
            port_count
        } else {
            0
        },
        max_output_ports: if port_direction == PortDirection::Output {
            port_count
        } else {
            0
        },
        change_mask: u64::from(NODE_CHANGE_FLAGS | NODE_CHANGE_PROPS | NODE_CHANGE_PARAMS),
        flags: u64::from(if port_direction == PortDirection::Output {
            NODE_FLAG_RT
        } else {
            NODE_FLAG_RT | NODE_FLAG_ASYNC
        }),
        properties: node_properties(target_node_id, options),
        params: vec![
            ParamInfo {
                id: SPA_PARAM_PROP_INFO,
                flags: 0,
            },
            ParamInfo {
                id: SPA_PARAM_PROPS,
                flags: SPA_PARAM_INFO_WRITE,
            },
            ParamInfo {
                id: SPA_PARAM_ENUM_FORMAT,
                flags: 0,
            },
            ParamInfo {
                id: SPA_PARAM_FORMAT,
                flags: SPA_PARAM_INFO_WRITE,
            },
            ParamInfo {
                id: SPA_PARAM_PROCESS_LATENCY,
                flags: SPA_PARAM_INFO_READ | SPA_PARAM_INFO_WRITE,
            },
        ],
    };
    let port_info = PortInfo {
        change_mask: u64::from(PORT_CHANGE_FLAGS | PORT_CHANGE_PROPS | PORT_CHANGE_PARAMS),
        flags: 0,
        rate: (0, 0),
        properties: {
            let mut properties = vec![
                ("port.group".into(), "stream.0".into()),
                (
                    "port.name".into(),
                    if port_direction == PortDirection::Input {
                        "input_0"
                    } else {
                        "output_0"
                    }
                    .into(),
                ),
                (
                    "port.direction".into(),
                    if port_direction == PortDirection::Input {
                        "in"
                    } else {
                        "out"
                    }
                    .into(),
                ),
                (
                    "port.alias".into(),
                    format!(
                        "{}:{}_0",
                        options.name,
                        if port_direction == PortDirection::Input {
                            "input"
                        } else {
                            "output"
                        }
                    ),
                ),
            ];
            if media == StreamMedia::Audio {
                let channel = options
                    .node_properties
                    .iter()
                    .find(|(key, _)| key == "audio.position")
                    .map_or("FL", |(_, position)| {
                        if position.contains("MONO") {
                            "MONO"
                        } else {
                            "FL"
                        }
                    });
                properties.push(("audio.channel".into(), channel.into()));
            }
            properties
        },
        params: initial_port_params(),
    };
    (node_info, port_info)
}

fn node_properties(target_node_id: u32, options: &StreamOptions) -> Vec<(String, String)> {
    let defaults = vec![
        ("media.type".into(), "Video".into()),
        ("media.category".into(), "Capture".into()),
        ("media.role".into(), "Screen".into()),
        ("node.name".into(), options.name.clone()),
        ("node.autoconnect".into(), "true".into()),
        ("node.loop.class".into(), "main".into()),
        ("node.async".into(), "true".into()),
        ("media.class".into(), "Stream/Input/Video".into()),
    ];
    let mut properties = defaults;
    if target_node_id != PIPEWIRE_ID_ANY {
        properties.push(("node.target".into(), target_node_id.to_string()));
    }
    for (key, value) in &options.node_properties {
        if let Some((_, existing)) = properties.iter_mut().find(|(name, _)| name == key) {
            existing.clone_from(value);
        } else {
            properties.push((key.clone(), value.clone()));
        }
    }
    properties
}

fn initial_port_params() -> Vec<ParamInfo> {
    vec![
        ParamInfo {
            id: SPA_PARAM_ENUM_FORMAT,
            flags: SPA_PARAM_INFO_READ,
        },
        ParamInfo {
            id: SPA_PARAM_META,
            flags: SPA_PARAM_INFO_READ,
        },
        ParamInfo {
            id: SPA_PARAM_IO,
            flags: SPA_PARAM_INFO_READ,
        },
        ParamInfo {
            id: SPA_PARAM_FORMAT,
            flags: SPA_PARAM_INFO_WRITE,
        },
        ParamInfo {
            id: SPA_PARAM_BUFFERS,
            flags: 0,
        },
        ParamInfo {
            id: SPA_PARAM_LATENCY,
            flags: SPA_PARAM_INFO_WRITE,
        },
        ParamInfo {
            id: SPA_PARAM_TAG,
            flags: SPA_PARAM_INFO_WRITE,
        },
        ParamInfo {
            id: SPA_PARAM_CAPABILITY,
            flags: 0,
        },
        ParamInfo {
            id: SPA_PARAM_PEER_CAPABILITY,
            flags: SPA_PARAM_INFO_WRITE,
        },
    ]
}

fn pack_video_plane(plane: &BufferPlane, format: NegotiatedVideoFormat) -> io::Result<VideoFrame> {
    pack_slices(plane.bytes.as_slice(), None, plane.stride, format)
}

fn pack_mapped_video_plane(
    plane: &MappedBufferPlane<'_>,
    format: NegotiatedVideoFormat,
) -> io::Result<VideoFrame> {
    let (first, second) = plane.data.as_slices();
    pack_slices(first, second, plane.stride, format)
}

fn pack_slices(
    first: &[u8],
    second: Option<&[u8]>,
    signed_stride: i32,
    format: NegotiatedVideoFormat,
) -> io::Result<VideoFrame> {
    let width = usize::try_from(format.width)
        .map_err(|_| invalid_data("Negotiated video width overflow"))?;
    let height = usize::try_from(format.height)
        .map_err(|_| invalid_data("Negotiated video height overflow"))?;
    if width < 2 || height < 2 || width > 4096 || height > 4096 {
        return Err(invalid_data(
            "Negotiated video dimensions are outside capture limits",
        ));
    }
    let bytes_per_pixel = usize::try_from(format.pixel_format.bytes_per_pixel())
        .map_err(|_| invalid_data("Video pixel size overflow"))?;
    let row_bytes = width
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| invalid_data("Video row size overflow"))?;
    let stride = usize::try_from(signed_stride.unsigned_abs())
        .map_err(|_| invalid_data("PipeWire video stride overflow"))?;
    if stride < row_bytes {
        return Err(invalid_data(
            "PipeWire video stride is smaller than one pixel row",
        ));
    }
    let total_len = first
        .len()
        .checked_add(second.map_or(0, <[u8]>::len))
        .ok_or_else(|| invalid_data("PipeWire video plane size overflow"))?;
    let required = stride
        .checked_mul(height - 1)
        .and_then(|rows| rows.checked_add(row_bytes))
        .ok_or_else(|| invalid_data("PipeWire video frame size overflow"))?;
    if total_len < required {
        return Err(invalid_data(
            "PipeWire video buffer is shorter than the negotiated frame",
        ));
    }
    let capacity = row_bytes
        .checked_mul(height)
        .ok_or_else(|| invalid_data("Packed video frame size overflow"))?;
    let mut packed = Vec::with_capacity(capacity);
    for row in 0..height {
        let source_row = if signed_stride < 0 {
            height - row - 1
        } else {
            row
        };
        let start = source_row * stride;
        let end = start + row_bytes;
        if end <= first.len() {
            packed.extend_from_slice(&first[start..end]);
        } else {
            let split = first.len();
            if start < split {
                packed.extend_from_slice(&first[start..]);
                let tail_len = end - split;
                let tail = second
                    .ok_or_else(|| invalid_data("Wrapped PipeWire plane has no second slice"))?;
                if tail_len > tail.len() {
                    return Err(invalid_data(
                        "Wrapped PipeWire row exceeds its second slice",
                    ));
                }
                packed.extend_from_slice(&tail[..tail_len]);
            } else {
                let tail = second
                    .ok_or_else(|| invalid_data("Wrapped PipeWire plane has no second slice"))?;
                let tail_start = start - split;
                let tail_end = end - split;
                if tail_end > tail.len() {
                    return Err(invalid_data("PipeWire video row exceeds mapped plane"));
                }
                packed.extend_from_slice(&tail[tail_start..tail_end]);
            }
        }
    }
    Ok(VideoFrame {
        data: packed,
        width: format.width,
        height: format.height,
        stride: u32::try_from(row_bytes).map_err(|_| invalid_data("Packed stride overflow"))?,
        format,
    })
}

fn pop_int_field(
    parser: &mut pipewire_native_spa::pod::parser::Parser<'_>,
) -> Result<i32, pipewire_native_spa::pod::Error> {
    parser.pop_int()
}

fn decode_registry_global_remove(payload: &[u8]) -> io::Result<u32> {
    pipewire_native_spa::pod::parser::Parser::new(payload)
        .pop_struct(pop_int_field)
        .map(|value| value.0.cast_unsigned())
        .map_err(pod_error("Decode Registry.GlobalRemove"))
}

fn pod_error(context: &'static str) -> impl FnOnce(pipewire_native_spa::pod::Error) -> io::Error {
    move |error| io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {error:?}"))
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// `pw_stream`-style name for the portal-backed raw-video stream.
pub type Stream = PipeWireStream;
/// Audio-capable native `PipeWire` stream type (use `connect_audio` to open).
pub type AudioStream = PipeWireStream;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "The protocol integration fixture intentionally exercises the full native handshake."
    )]
    fn captures_frame_and_disconnects_through_the_portal_native_protocol() {
        use crate::format::{encode_negotiated_video_format, NegotiatedVideoFormat, VideoFormat};
        use crate::node::IoBuffers;
        use crate::remote::PipeWireRemote;
        use crate::transport::{
            read_eventfd, write_eventfd, MessageHeader, NativeMessage, NativeTransport,
        };
        use pipewire_native_spa::pod::builder::Builder;
        use pipewire_native_spa::pod::parser::Parser;
        use pipewire_native_spa::pod::types::Id;
        use pipewire_native_spa::pod::RawPod;
        use rustix::event::{eventfd, EventfdFlags};
        use rustix::fs::{memfd_create, MemfdFlags};
        use std::fs::File;
        use std::os::fd::{AsFd, OwnedFd};
        use std::os::unix::fs::FileExt;
        use std::os::unix::net::UnixStream;
        use std::thread;

        fn send_done(server: &mut NativeTransport, request: &NativeMessage) {
            let mut parser = Parser::new(&request.payload);
            let (id, seq) = parser
                .pop_struct(|fields| Ok((fields.pop_int()?, fields.pop_int()?)))
                .unwrap()
                .0;
            let mut buffer = [0_u8; 512];
            let payload = Builder::new(&mut buffer)
                .push_struct(|fields| fields.push_int(id).push_int(seq))
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 1,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload,
                    fds: Vec::new(),
                })
                .unwrap();
        }

        let (client_socket, server_socket) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut server = NativeTransport::from_owned_fd(OwnedFd::from(server_socket));
            let hello = server.receive().unwrap();
            assert_eq!((hello.header.id, hello.header.opcode), (0, 1));
            let properties = server.receive().unwrap();
            assert_eq!((properties.header.id, properties.header.opcode), (1, 2));

            let get_registry = server.receive().unwrap();
            assert_eq!((get_registry.header.id, get_registry.header.opcode), (0, 5));
            let sync = server.receive().unwrap();
            assert_eq!((sync.header.id, sync.header.opcode), (0, 2));

            let mut global_buffer = [0_u8; 512];
            let global = Builder::new(&mut global_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(42)
                        .push_int(7)
                        .push_string("PipeWire:Interface:Node")
                        .push_int(3)
                        .push_struct(|pairs| {
                            pairs
                                .push_int(1)
                                .push_string("node.name")
                                .push_string("test.source")
                        })
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 2,
                        opcode: 0,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: global,
                    fds: Vec::new(),
                })
                .unwrap();
            send_done(&mut server, &sync);

            let create = server.receive().unwrap();
            assert_eq!((create.header.id, create.header.opcode), (0, 6));
            let update = server.receive().unwrap();
            assert_eq!((update.header.id, update.header.opcode), (3, 2));

            let memfd = memfd_create("pw-client-activation", MemfdFlags::CLOEXEC).unwrap();
            let file = File::from(memfd);
            file.set_len(4096).unwrap();
            let mut activation = [0_u8; 512];
            activation[4..8].copy_from_slice(&1_u32.to_ne_bytes());
            activation[16..20].copy_from_slice(&1_u32.to_ne_bytes());
            file.write_at(&activation, 0).unwrap();
            let activation_mem = OwnedFd::from(file.try_clone().unwrap());
            let mut add_mem_buffer = [0_u8; 512];
            let add_mem = Builder::new(&mut add_mem_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(7)
                        .push_id(Id::<u32>(2))
                        .push_fd(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 6,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: add_mem,
                    fds: vec![activation_mem],
                })
                .unwrap();

            let activation_signal = File::from(eventfd(0, EventfdFlags::CLOEXEC).unwrap());
            let activation_signal_fd = OwnedFd::from(activation_signal.try_clone().unwrap());
            let mut activation_buffer = [0_u8; 512];
            let activation_event = Builder::new(&mut activation_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(42)
                        .push_fd(0)
                        .push_int(7)
                        .push_int(0)
                        .push_int(64)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 10,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: activation_event,
                    fds: vec![activation_signal_fd],
                })
                .unwrap();

            let read_signal = File::from(eventfd(0, EventfdFlags::CLOEXEC).unwrap());
            let write_signal = File::from(eventfd(0, EventfdFlags::CLOEXEC).unwrap());
            let read_signal_fd = OwnedFd::from(read_signal.try_clone().unwrap());
            let write_signal_fd = OwnedFd::from(write_signal.try_clone().unwrap());
            let mut transport_buffer = [0_u8; 512];
            let transport_event = Builder::new(&mut transport_buffer)
                .push_struct(|fields| {
                    fields
                        .push_fd(0)
                        .push_fd(1)
                        .push_int(7)
                        .push_int(0)
                        .push_int(64)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 0,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: transport_event,
                    fds: vec![read_signal_fd, write_signal_fd],
                })
                .unwrap();

            let active = server.receive().unwrap();
            assert_eq!((active.header.id, active.header.opcode), (3, 4));
            let port_update = server.receive().unwrap();
            assert_eq!((port_update.header.id, port_update.header.opcode), (3, 3));

            let video_file =
                File::from(memfd_create("pw-client-video", MemfdFlags::CLOEXEC).unwrap());
            video_file.set_len(4096).unwrap();
            let mut video_bytes = [0_u8; 4096];
            video_bytes[8..12].copy_from_slice(&0_u32.to_ne_bytes());
            video_bytes[12..16].copy_from_slice(&16_u32.to_ne_bytes());
            video_bytes[16..20].copy_from_slice(&8_i32.to_ne_bytes());
            video_bytes[20..24].copy_from_slice(&0_u32.to_ne_bytes());
            video_bytes[128..144]
                .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
            video_file.write_at(&video_bytes, 0).unwrap();
            let mut add_video_buffer = [0_u8; 512];
            let add_video = Builder::new(&mut add_video_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(8)
                        .push_id(Id::<u32>(2))
                        .push_fd(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 6,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: add_video,
                    fds: vec![OwnedFd::from(video_file.try_clone().unwrap())],
                })
                .unwrap();

            let io_file = File::from(memfd_create("pw-client-io", MemfdFlags::CLOEXEC).unwrap());
            io_file.set_len(4096).unwrap();
            let mut io_bytes = [0_u8; 8];
            io_bytes[..4].copy_from_slice(&IoBuffers::STATUS_HAVE_DATA.to_ne_bytes());
            io_bytes[4..].copy_from_slice(&0_u32.to_ne_bytes());
            io_file.write_at(&io_bytes, 0).unwrap();
            let mut add_io_buffer = [0_u8; 512];
            let add_io = Builder::new(&mut add_io_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(9)
                        .push_id(Id::<u32>(2))
                        .push_fd(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 6,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: add_io,
                    fds: vec![OwnedFd::from(io_file.try_clone().unwrap())],
                })
                .unwrap();

            let negotiated = NegotiatedVideoFormat {
                pixel_format: VideoFormat::Bgra,
                width: 2,
                height: 2,
            };
            let format_pod = encode_negotiated_video_format(negotiated).unwrap();
            let format_pod = RawPod::wrap(&format_pod).unwrap();
            let mut format_buffer = [0_u8; 512];
            let format_event = Builder::new(&mut format_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(0)
                        .push_int(0)
                        .push_id(Id::<u32>(4))
                        .push_int(0)
                        .push_pod(&format_pod)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 7,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: format_event,
                    fds: Vec::new(),
                })
                .unwrap();
            let format_ack = server.receive().unwrap();
            assert_eq!((format_ack.header.id, format_ack.header.opcode), (3, 3));

            let mut buffers_buffer = [0_u8; 512];
            let buffers_event = Builder::new(&mut buffers_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(0)
                        .push_int(0)
                        .push_int(0)
                        .push_int(0)
                        .push_int(1)
                        .push_int(8)
                        .push_int(0)
                        .push_int(24)
                        .push_int(1)
                        .push_id(Id::<u32>(7))
                        .push_int(8)
                        .push_int(1)
                        .push_id(Id::<u32>(4))
                        .push_int(8)
                        .push_int(0)
                        .push_int(128)
                        .push_int(64)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 8,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: buffers_event,
                    fds: Vec::new(),
                })
                .unwrap();

            let mut io_buffer = [0_u8; 512];
            let io_event = Builder::new(&mut io_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(0)
                        .push_int(0)
                        .push_int(0)
                        .push_id(Id::<u32>(1))
                        .push_int(9)
                        .push_int(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 9,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: io_event,
                    fds: Vec::new(),
                })
                .unwrap();

            let command_pod = [
                8_u32.to_ne_bytes(),
                15_u32.to_ne_bytes(),
                0x0003_0002_u32.to_ne_bytes(),
                2_u32.to_ne_bytes(),
            ]
            .concat();
            let command_pod = RawPod::wrap(&command_pod).unwrap();
            let mut command_buffer = [0_u8; 512];
            let command_event = Builder::new(&mut command_buffer)
                .push_struct(|fields| fields.push_pod(&command_pod))
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 4,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: command_event,
                    fds: Vec::new(),
                })
                .unwrap();
            write_eventfd(read_signal.as_fd(), 1).unwrap();
            assert_eq!(read_eventfd(activation_signal.as_fd()).unwrap(), 1);

            let inactive = server.receive().unwrap();
            assert_eq!((inactive.header.id, inactive.header.opcode), (3, 4));
            let destroy = server.receive().unwrap();
            assert_eq!((destroy.header.id, destroy.header.opcode), (0, 7));
            let sync = server.receive().unwrap();
            assert_eq!((sync.header.id, sync.header.opcode), (0, 2));
            send_done(&mut server, &sync);
        });

        let remote = PipeWireRemote::from_portal(OwnedFd::from(client_socket), 42);
        let mut stream = PipeWireStream::connect(remote).unwrap();
        assert!(!stream.is_streaming());
        let frame = stream.next_frame(Duration::from_secs(2)).unwrap().unwrap();
        assert!(stream.is_streaming());
        assert_eq!((frame.width, frame.height, frame.stride), (2, 2, 8));
        assert_eq!(
            frame.data,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(
            stream.port_buffers[0].busy_count(&stream.client).unwrap(),
            Some(0)
        );
        let (io_memory_id, io_offset, io_size) = stream.io.unwrap();
        let io_state = IoBuffers::read(&stream.client, io_memory_id, io_offset, io_size).unwrap();
        assert_eq!(io_state.status, IoBuffers::STATUS_NEED_DATA);
        stream.disconnect().unwrap();
        server.join().unwrap();
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "The protocol integration fixture intentionally exercises the full native handshake."
    )]
    fn captures_audio_frame_and_disconnects_through_the_portal_native_protocol() {
        use crate::audio::{
            encode_negotiated_audio_format, AudioSampleFormat, NegotiatedAudioFormat,
        };
        use crate::node::IoBuffers;
        use crate::remote::PipeWireRemote;
        use crate::transport::{
            read_eventfd, write_eventfd, MessageHeader, NativeMessage, NativeTransport,
        };
        use pipewire_native_spa::pod::builder::Builder;
        use pipewire_native_spa::pod::parser::Parser;
        use pipewire_native_spa::pod::types::Id;
        use pipewire_native_spa::pod::RawPod;
        use rustix::event::{eventfd, EventfdFlags};
        use rustix::fs::{memfd_create, MemfdFlags};
        use std::fs::File;
        use std::os::fd::{AsFd, OwnedFd};
        use std::os::unix::fs::FileExt;
        use std::os::unix::net::UnixStream;
        use std::thread;

        fn send_done(server: &mut NativeTransport, request: &NativeMessage) {
            let mut parser = Parser::new(&request.payload);
            let (id, seq) = parser
                .pop_struct(|fields| Ok((fields.pop_int()?, fields.pop_int()?)))
                .unwrap()
                .0;
            let mut buffer = [0_u8; 512];
            let payload = Builder::new(&mut buffer)
                .push_struct(|fields| fields.push_int(id).push_int(seq))
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 1,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload,
                    fds: Vec::new(),
                })
                .unwrap();
        }

        let (client_socket, server_socket) = UnixStream::pair().unwrap();
        let server = thread::spawn(move || {
            let mut server = NativeTransport::from_owned_fd(OwnedFd::from(server_socket));
            let hello = server.receive().unwrap();
            assert_eq!((hello.header.id, hello.header.opcode), (0, 1));
            let properties = server.receive().unwrap();
            assert_eq!((properties.header.id, properties.header.opcode), (1, 2));

            let get_registry = server.receive().unwrap();
            assert_eq!((get_registry.header.id, get_registry.header.opcode), (0, 5));
            let sync = server.receive().unwrap();
            assert_eq!((sync.header.id, sync.header.opcode), (0, 2));

            let mut global_buffer = [0_u8; 512];
            let global = Builder::new(&mut global_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(42)
                        .push_int(7)
                        .push_string("PipeWire:Interface:Node")
                        .push_int(3)
                        .push_struct(|pairs| {
                            pairs
                                .push_int(1)
                                .push_string("node.name")
                                .push_string("test.source")
                        })
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 2,
                        opcode: 0,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: global,
                    fds: Vec::new(),
                })
                .unwrap();
            send_done(&mut server, &sync);

            let create = server.receive().unwrap();
            assert_eq!((create.header.id, create.header.opcode), (0, 6));
            let update = server.receive().unwrap();
            assert_eq!((update.header.id, update.header.opcode), (3, 2));

            let memfd = memfd_create("pw-client-activation", MemfdFlags::CLOEXEC).unwrap();
            let file = File::from(memfd);
            file.set_len(4096).unwrap();
            let mut activation = [0_u8; 512];
            activation[4..8].copy_from_slice(&1_u32.to_ne_bytes());
            activation[16..20].copy_from_slice(&1_u32.to_ne_bytes());
            file.write_at(&activation, 0).unwrap();
            let activation_mem = OwnedFd::from(file.try_clone().unwrap());
            let mut add_mem_buffer = [0_u8; 512];
            let add_mem = Builder::new(&mut add_mem_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(7)
                        .push_id(Id::<u32>(2))
                        .push_fd(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 6,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: add_mem,
                    fds: vec![activation_mem],
                })
                .unwrap();

            let activation_signal = File::from(eventfd(0, EventfdFlags::CLOEXEC).unwrap());
            let activation_signal_fd = OwnedFd::from(activation_signal.try_clone().unwrap());
            let mut activation_buffer = [0_u8; 512];
            let activation_event = Builder::new(&mut activation_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(42)
                        .push_fd(0)
                        .push_int(7)
                        .push_int(0)
                        .push_int(64)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 10,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: activation_event,
                    fds: vec![activation_signal_fd],
                })
                .unwrap();

            let read_signal = File::from(eventfd(0, EventfdFlags::CLOEXEC).unwrap());
            let write_signal = File::from(eventfd(0, EventfdFlags::CLOEXEC).unwrap());
            let read_signal_fd = OwnedFd::from(read_signal.try_clone().unwrap());
            let write_signal_fd = OwnedFd::from(write_signal.try_clone().unwrap());
            let mut transport_buffer = [0_u8; 512];
            let transport_event = Builder::new(&mut transport_buffer)
                .push_struct(|fields| {
                    fields
                        .push_fd(0)
                        .push_fd(1)
                        .push_int(7)
                        .push_int(0)
                        .push_int(64)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 0,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: transport_event,
                    fds: vec![read_signal_fd, write_signal_fd],
                })
                .unwrap();

            let active = server.receive().unwrap();
            assert_eq!((active.header.id, active.header.opcode), (3, 4));
            let port_update = server.receive().unwrap();
            assert_eq!((port_update.header.id, port_update.header.opcode), (3, 3));

            let audio_file =
                File::from(memfd_create("pw-client-video", MemfdFlags::CLOEXEC).unwrap());
            audio_file.set_len(4096).unwrap();
            let mut audio_bytes = [0_u8; 4096];
            audio_bytes[8..12].copy_from_slice(&0_u32.to_ne_bytes());
            audio_bytes[12..16].copy_from_slice(&16_u32.to_ne_bytes());
            audio_bytes[16..20].copy_from_slice(&4_i32.to_ne_bytes());
            audio_bytes[20..24].copy_from_slice(&0_u32.to_ne_bytes());
            audio_bytes[128..144]
                .copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
            audio_file.write_at(&audio_bytes, 0).unwrap();
            let mut add_audio_buffer = [0_u8; 512];
            let add_audio = Builder::new(&mut add_audio_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(8)
                        .push_id(Id::<u32>(2))
                        .push_fd(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 6,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: add_audio,
                    fds: vec![OwnedFd::from(audio_file.try_clone().unwrap())],
                })
                .unwrap();

            let io_file = File::from(memfd_create("pw-client-io", MemfdFlags::CLOEXEC).unwrap());
            io_file.set_len(4096).unwrap();
            let mut io_bytes = [0_u8; 8];
            io_bytes[..4].copy_from_slice(&IoBuffers::STATUS_HAVE_DATA.to_ne_bytes());
            io_bytes[4..].copy_from_slice(&0_u32.to_ne_bytes());
            io_file.write_at(&io_bytes, 0).unwrap();
            let mut add_io_buffer = [0_u8; 512];
            let add_io = Builder::new(&mut add_io_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(9)
                        .push_id(Id::<u32>(2))
                        .push_fd(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 0,
                        opcode: 6,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: add_io,
                    fds: vec![OwnedFd::from(io_file.try_clone().unwrap())],
                })
                .unwrap();

            let negotiated = NegotiatedAudioFormat {
                sample_format: AudioSampleFormat::S16Le,
                rate: 48_000,
                channels: 2,
            };
            let format_pod = encode_negotiated_audio_format(negotiated).unwrap();
            let format_pod = RawPod::wrap(&format_pod).unwrap();
            let mut format_buffer = [0_u8; 512];
            let format_event = Builder::new(&mut format_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(0)
                        .push_int(0)
                        .push_id(Id::<u32>(4))
                        .push_int(0)
                        .push_pod(&format_pod)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 7,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: format_event,
                    fds: Vec::new(),
                })
                .unwrap();
            let format_ack = server.receive().unwrap();
            assert_eq!((format_ack.header.id, format_ack.header.opcode), (3, 3));

            let mut buffers_buffer = [0_u8; 512];
            let buffers_event = Builder::new(&mut buffers_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(0)
                        .push_int(0)
                        .push_int(0)
                        .push_int(0)
                        .push_int(1)
                        .push_int(8)
                        .push_int(0)
                        .push_int(24)
                        .push_int(1)
                        .push_id(Id::<u32>(7))
                        .push_int(8)
                        .push_int(1)
                        .push_id(Id::<u32>(4))
                        .push_int(8)
                        .push_int(0)
                        .push_int(128)
                        .push_int(64)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 8,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: buffers_event,
                    fds: Vec::new(),
                })
                .unwrap();

            let mut io_buffer = [0_u8; 512];
            let io_event = Builder::new(&mut io_buffer)
                .push_struct(|fields| {
                    fields
                        .push_int(0)
                        .push_int(0)
                        .push_int(0)
                        .push_id(Id::<u32>(1))
                        .push_int(9)
                        .push_int(0)
                        .push_int(8)
                })
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 9,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: io_event,
                    fds: Vec::new(),
                })
                .unwrap();

            let command_pod = [
                8_u32.to_ne_bytes(),
                15_u32.to_ne_bytes(),
                0x0003_0002_u32.to_ne_bytes(),
                2_u32.to_ne_bytes(),
            ]
            .concat();
            let command_pod = RawPod::wrap(&command_pod).unwrap();
            let mut command_buffer = [0_u8; 512];
            let command_event = Builder::new(&mut command_buffer)
                .push_struct(|fields| fields.push_pod(&command_pod))
                .build()
                .unwrap()
                .to_vec();
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: 3,
                        opcode: 4,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: command_event,
                    fds: Vec::new(),
                })
                .unwrap();
            write_eventfd(read_signal.as_fd(), 1).unwrap();
            assert_eq!(read_eventfd(activation_signal.as_fd()).unwrap(), 1);

            let inactive = server.receive().unwrap();
            assert_eq!((inactive.header.id, inactive.header.opcode), (3, 4));
            let destroy = server.receive().unwrap();
            assert_eq!((destroy.header.id, destroy.header.opcode), (0, 7));
            let sync = server.receive().unwrap();
            assert_eq!((sync.header.id, sync.header.opcode), (0, 2));
            send_done(&mut server, &sync);
        });

        let remote = PipeWireRemote::from_portal(OwnedFd::from(client_socket), 42);
        let mut stream = PipeWireStream::connect_audio(remote).unwrap();
        assert!(!stream.is_streaming());
        let frame = stream
            .next_audio_frame(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(stream.is_streaming());
        assert_eq!(
            frame.format,
            NegotiatedAudioFormat {
                sample_format: AudioSampleFormat::S16Le,
                rate: 48_000,
                channels: 2,
            }
        );
        assert_eq!(
            frame.data,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(
            stream.port_buffers[0].busy_count(&stream.client).unwrap(),
            Some(0)
        );
        let (io_memory_id, io_offset, io_size) = stream.io.unwrap();
        let io_state = IoBuffers::read(&stream.client, io_memory_id, io_offset, io_size).unwrap();
        assert_eq!(io_state.status, IoBuffers::STATUS_NEED_DATA);
        stream.disconnect().unwrap();
        server.join().unwrap();
    }

    #[test]
    fn node_and_port_properties_match_pw_stream_screen_capture() {
        let (node, port) = stream_info(
            42,
            &StreamOptions::default(),
            StreamMedia::Video,
            PortDirection::Input,
            false,
        );
        assert_eq!(node.max_input_ports, 1);
        assert_eq!(node.max_output_ports, 0);
        assert_eq!(node.flags, 65);
        assert!(node
            .properties
            .contains(&("node.name".into(), "PipeWire Stream".into())));
        assert_eq!(
            node.properties
                .iter()
                .find(|(key, _)| key == "node.target")
                .map(|(_, value)| value.as_str()),
            Some("42")
        );
        assert_eq!(port.flags, 0);
        assert_eq!(port.params.len(), 9);
    }

    #[test]
    fn audio_playback_node_exposes_output_port_and_synchronous_processing() {
        let options = StreamOptions {
            name: "ZaloCall Playback".into(),
            node_properties: vec![
                ("media.type".into(), "Audio".into()),
                ("media.category".into(), "Playback".into()),
                ("media.class".into(), "Stream/Output/Audio".into()),
                ("audio.position".into(), "[ MONO ]".into()),
            ],
            ..StreamOptions::default()
        };
        let (node, port) = stream_info(
            PIPEWIRE_ID_ANY,
            &options,
            StreamMedia::Audio,
            PortDirection::Output,
            false,
        );
        assert_eq!(node.max_input_ports, 0);
        assert_eq!(node.max_output_ports, 1);
        assert_eq!(node.flags, u64::from(NODE_FLAG_RT));
        assert_eq!(
            port.properties
                .iter()
                .find(|(key, _)| key == "port.direction")
                .map(|(_, value)| value.as_str()),
            Some("out")
        );
        assert_eq!(
            port.properties
                .iter()
                .find(|(key, _)| key == "port.name")
                .map(|(_, value)| value.as_str()),
            Some("output_0")
        );
        assert_eq!(
            node.properties
                .iter()
                .find(|(key, _)| key == "media.class")
                .map(|(_, value)| value.as_str()),
            Some("Stream/Output/Audio")
        );
        assert_eq!(
            port.properties
                .iter()
                .find(|(key, _)| key == "audio.channel")
                .map(|(_, value)| value.as_str()),
            Some("MONO")
        );
    }

    #[test]
    fn bgrx_frame_conversion_sets_opaque_alpha() {
        let frame = VideoFrame {
            data: vec![1, 2, 3, 0],
            width: 1,
            height: 1,
            stride: 4,
            format: NegotiatedVideoFormat {
                pixel_format: VideoFormat::Bgrx,
                width: 1,
                height: 1,
            },
        };
        assert_eq!(frame.into_bgra(), vec![1, 2, 3, u8::MAX]);
    }

    #[test]
    fn packed_plane_reads_rows_across_a_circular_mapping_boundary() {
        let format = NegotiatedVideoFormat {
            pixel_format: crate::format::VideoFormat::Bgra,
            width: 2,
            height: 2,
        };
        let first = [1, 2, 3, 4, 5, 6, 7, 8, 99, 99];
        let second = [9, 10, 11, 12, 13, 14, 15, 16];
        let frame = pack_slices(&first, Some(&second), 10, format).unwrap();
        assert_eq!(frame.data, (1..=16).collect::<Vec<u8>>());
    }

    #[test]
    fn negative_stride_reverses_source_rows() {
        let format = NegotiatedVideoFormat {
            pixel_format: crate::format::VideoFormat::Bgra,
            width: 2,
            height: 2,
        };
        let plane = BufferPlane {
            memory_id: 0,
            bytes: vec![9, 10, 11, 12, 13, 14, 15, 16, 1, 2, 3, 4, 5, 6, 7, 8],
            stride: -8,
            flags: 0,
        };
        let frame = pack_video_plane(&plane, format).unwrap();
        assert_eq!(
            frame.data,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn stream_options_are_generic_and_override_default_node_properties() {
        let options = StreamOptions {
            name: "Test Capture".into(),
            node_properties: vec![("node.description".into(), "test stream".into())],
            ..StreamOptions::default()
        };
        let properties = node_properties(42, &options);
        assert!(properties.contains(&("node.name".into(), "Test Capture".into())));
        assert!(properties.contains(&("node.target".into(), "42".into())));
        assert!(properties.contains(&("node.description".into(), "test stream".into())));
    }

    #[test]
    fn frame_packing_preserves_stride_padding_and_pixel_bytes() {
        let format = NegotiatedVideoFormat {
            pixel_format: crate::format::VideoFormat::Bgra,
            width: 2,
            height: 2,
        };
        let plane = BufferPlane {
            memory_id: 0,
            bytes: vec![
                1, 2, 3, 4, 5, 6, 7, 8, 99, 99, 9, 10, 11, 12, 13, 14, 15, 16,
            ],
            stride: 10,
            flags: 0,
        };
        let frame = pack_video_plane(&plane, format).unwrap();
        assert_eq!(
            frame.data,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
        assert_eq!(frame.stride, 8);
        assert_eq!(frame.width, 2);
        assert_eq!(frame.height, 2);
    }
}
