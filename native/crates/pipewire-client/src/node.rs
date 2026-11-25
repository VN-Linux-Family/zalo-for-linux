//! Native `PipeWire` `ClientNode` proxy and SPA node/port event types.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::Ordering;

use pipewire_native_spa::pod::{builder::Builder, parser::Parser, types::Id, RawPodOwned};

use crate::{
    client::{PipeWireClient, PipeWireObject},
    transport::NativeMessage,
};

const CLIENT_NODE_INTERFACE: &str = "PipeWire:Interface:ClientNode";
const CLIENT_NODE_VERSION: u32 = crate::build_info::CLIENT_NODE_PROTOCOL_VERSION;
const METHOD_GET_NODE: u8 = 1;
const METHOD_UPDATE: u8 = 2;
const METHOD_PORT_UPDATE: u8 = 3;
const METHOD_SET_ACTIVE: u8 = 4;
const METHOD_EVENT: u8 = 5;
const METHOD_PORT_BUFFERS: u8 = 6;
const EVENT_TRANSPORT: u8 = 0;
const EVENT_SET_PARAM: u8 = 1;
const EVENT_SET_IO: u8 = 2;
const EVENT_EVENT: u8 = 3;
const EVENT_COMMAND: u8 = 4;
const EVENT_ADD_PORT: u8 = 5;
const EVENT_REMOVE_PORT: u8 = 6;
const EVENT_PORT_SET_PARAM: u8 = 7;
const EVENT_PORT_USE_BUFFERS: u8 = 8;
const EVENT_PORT_SET_IO: u8 = 9;
const EVENT_SET_ACTIVATION: u8 = 10;
const EVENT_PORT_SET_MIX_INFO: u8 = 11;

pub const NODE_CHANGE_FLAGS: u32 = 1 << 0;
pub const NODE_CHANGE_PROPS: u32 = 1 << 1;
pub const NODE_CHANGE_PARAMS: u32 = 1 << 2;
pub const NODE_FLAG_RT: u32 = 1 << 0;
pub const NODE_FLAG_ASYNC: u32 = 1 << 6;

pub const PORT_CHANGE_FLAGS: u32 = 1 << 0;
pub const PORT_CHANGE_RATE: u32 = 1 << 1;
pub const PORT_CHANGE_PROPS: u32 = 1 << 2;
pub const PORT_CHANGE_PARAMS: u32 = 1 << 3;

pub const CLIENT_NODE_UPDATE_PARAMS: u32 = 1 << 0;
pub const CLIENT_NODE_UPDATE_INFO: u32 = 1 << 1;
pub const CLIENT_NODE_PORT_UPDATE_PARAMS: u32 = 1 << 0;
pub const CLIENT_NODE_PORT_UPDATE_INFO: u32 = 1 << 1;

const MAX_ITEMS: i32 = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PortDirection {
    Input = 0,
    Output = 1,
}

impl TryFrom<i32> for PortDirection {
    type Error = pipewire_native_spa::pod::Error;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Input),
            1 => Ok(Self::Output),
            _ => Err(pipewire_native_spa::pod::Error::Invalid(
                "Invalid `PipeWire` port direction".into(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParamInfo {
    pub id: u32,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NodeInfo {
    pub max_input_ports: u32,
    pub max_output_ports: u32,
    pub change_mask: u64,
    pub flags: u64,
    pub properties: Vec<(String, String)>,
    pub params: Vec<ParamInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortInfo {
    pub change_mask: u64,
    pub flags: u64,
    pub rate: (u32, u32),
    pub properties: Vec<(String, String)>,
    pub params: Vec<ParamInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferMeta {
    pub type_id: u32,
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferData {
    pub type_id: u32,
    /// Memory ID for `MemId`, or byte offset for `MemPtr`.
    pub data: u32,
    pub flags: u32,
    pub map_offset: u32,
    pub max_size: u32,
}

pub struct PipeWireBuffer {
    pub memory_id: u32,
    pub offset: u32,
    pub size: u32,
    pub metas: Vec<BufferMeta>,
    pub data: Vec<BufferData>,
    dma_buf_mappings: Vec<Option<mmap_io::MemoryMappedFile>>,
}

impl std::fmt::Debug for PipeWireBuffer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PipeWireBuffer")
            .field("memory_id", &self.memory_id)
            .field("offset", &self.offset)
            .field("size", &self.size)
            .field("metas", &self.metas)
            .field("data", &self.data)
            .field(
                "dma_buf_count",
                &self
                    .dma_buf_mappings
                    .iter()
                    .filter(|mapping| mapping.is_some())
                    .count(),
            )
            .finish()
    }
}

#[derive(Debug)]
pub struct PortBufferFd {
    pub type_id: u32,
    pub fd: OwnedFd,
    pub flags: u32,
    pub map_offset: u32,
    pub max_size: u32,
}

/// The SPA chunk metadata for one data plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BufferChunk {
    pub offset: u32,
    pub size: u32,
    pub stride: i32,
    pub flags: u32,
}

/// A copied data plane, read without mapping shared memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferPlane {
    pub memory_id: u32,
    pub bytes: Vec<u8>,
    pub stride: i32,
    pub flags: u32,
}

/// Zero-copy view of a buffer plane, possibly split at a circular-buffer wrap.
pub enum BufferPlaneView<'a> {
    Contiguous(mmap_io::MappedSlice<'a>),
    Wrapped {
        first: mmap_io::MappedSlice<'a>,
        second: mmap_io::MappedSlice<'a>,
    },
}

impl BufferPlaneView<'_> {
    #[must_use]
    pub fn as_slices(&self) -> (&[u8], Option<&[u8]>) {
        match self {
            Self::Contiguous(slice) => (slice, None),
            Self::Wrapped { first, second } => (first, Some(second)),
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        let (first, second) = self.as_slices();
        first.len() + second.map_or(0, <[u8]>::len)
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A mapped, zero-copy SPA data plane.
pub struct MappedBufferPlane<'a> {
    pub memory_id: u32,
    pub stride: i32,
    pub flags: u32,
    pub data: BufferPlaneView<'a>,
}

struct ResolvedPlane {
    memory_id: u32,
    base_offset: u64,
    max_size: u32,
    chunk: BufferChunk,
}

impl PipeWireBuffer {
    /// Read one SPA data plane into an owned buffer.
    ///
    /// This uses the safe mapped read path when possible and falls back to
    /// positional reads for memory objects that cannot be mapped.
    pub fn read_plane(
        &self,
        client: &PipeWireClient,
        plane_index: usize,
    ) -> io::Result<BufferPlane> {
        let resolved = self.resolve_plane(client, plane_index)?;
        if let Some(mapping) = self
            .dma_buf_mappings
            .get(plane_index)
            .and_then(Option::as_ref)
        {
            let start = resolved.chunk.offset % resolved.max_size;
            let size = resolved.chunk.size.min(resolved.max_size);
            let first_len = size.min(resolved.max_size - start);
            let mut bytes = mapping
                .as_slice(
                    resolved.base_offset + u64::from(start),
                    u64::from(first_len),
                )
                .map_err(io::Error::other)?
                .to_vec();
            let remaining = size - first_len;
            if remaining > 0 {
                bytes.extend_from_slice(
                    &mapping
                        .as_slice(resolved.base_offset, u64::from(remaining))
                        .map_err(io::Error::other)?,
                );
            }
            return Ok(BufferPlane {
                memory_id: resolved.memory_id,
                bytes,
                stride: resolved.chunk.stride,
                flags: resolved.chunk.flags,
            });
        }
        let start = resolved.chunk.offset % resolved.max_size;
        let memory = client
            .memory(resolved.memory_id)
            .ok_or_else(|| invalid_data("Data plane references unknown memory"))?;
        let size = resolved.chunk.size.min(resolved.max_size);
        let first_len = size.min(resolved.max_size - start);
        let mut bytes =
            memory.read_at(resolved.base_offset + u64::from(start), first_len as usize)?;
        let remaining = size - first_len;
        if remaining > 0 {
            bytes.extend(memory.read_at(resolved.base_offset, remaining as usize)?);
        }
        Ok(BufferPlane {
            memory_id: resolved.memory_id,
            bytes,
            stride: resolved.chunk.stride,
            flags: resolved.chunk.flags,
        })
    }

    /// Borrow one SPA data plane directly from its shared-memory mapping.
    ///
    /// The returned view keeps the mapping read-locked for its lifetime and
    /// avoids a frame-sized copy. Wrapped chunks are represented by two slices.
    pub fn map_plane<'a>(
        &'a self,
        client: &'a PipeWireClient,
        plane_index: usize,
    ) -> io::Result<MappedBufferPlane<'a>> {
        let resolved = self.resolve_plane(client, plane_index)?;
        let start = resolved.chunk.offset % resolved.max_size;
        if let Some(mapping) = self
            .dma_buf_mappings
            .get(plane_index)
            .and_then(Option::as_ref)
        {
            let size = resolved.chunk.size.min(resolved.max_size);
            let first_len = size.min(resolved.max_size - start);
            let first = mapping
                .as_slice(
                    resolved.base_offset + u64::from(start),
                    u64::from(first_len),
                )
                .map_err(io::Error::other)?;
            let remaining = size - first_len;
            let data = if remaining == 0 {
                BufferPlaneView::Contiguous(first)
            } else {
                BufferPlaneView::Wrapped {
                    first,
                    second: mapping
                        .as_slice(resolved.base_offset, u64::from(remaining))
                        .map_err(io::Error::other)?,
                }
            };
            return Ok(MappedBufferPlane {
                memory_id: resolved.memory_id,
                stride: resolved.chunk.stride,
                flags: resolved.chunk.flags,
                data,
            });
        }
        let memory = client
            .memory(resolved.memory_id)
            .ok_or_else(|| invalid_data("Data plane references unknown memory"))?;
        let size = resolved.chunk.size.min(resolved.max_size);
        let first_len = size.min(resolved.max_size - start);
        let first =
            memory.map_slice_at(resolved.base_offset + u64::from(start), first_len as usize)?;
        let remaining = size - first_len;
        let data = if remaining == 0 {
            BufferPlaneView::Contiguous(first)
        } else {
            BufferPlaneView::Wrapped {
                first,
                second: memory.map_slice_at(resolved.base_offset, remaining as usize)?,
            }
        };
        Ok(MappedBufferPlane {
            memory_id: resolved.memory_id,
            stride: resolved.chunk.stride,
            flags: resolved.chunk.flags,
            data,
        })
    }

    fn resolve_plane(
        &self,
        client: &PipeWireClient,
        plane_index: usize,
    ) -> io::Result<ResolvedPlane> {
        let data = self
            .data
            .get(plane_index)
            .ok_or_else(|| invalid_data("Data plane index is out of range"))?;
        let chunk_table_offset = self.chunk_table_offset()?;
        let plane_offset = plane_index
            .checked_mul(16)
            .ok_or_else(|| invalid_data("Chunk offset overflow"))?;
        let chunk_offset = chunk_table_offset
            .checked_add(plane_offset as u64)
            .ok_or_else(|| invalid_data("Chunk offset overflow"))?;
        let metadata_memory = client
            .memory(self.memory_id)
            .ok_or_else(|| invalid_data("Buffer references unknown metadata memory"))?;
        let chunk = decode_buffer_chunk(&metadata_memory.read_at(chunk_offset, 16)?)?;
        if data.max_size == 0 {
            return Err(invalid_data("Data plane has zero max_size"));
        }
        let (memory_id, base_offset) = match data.type_id {
            SPA_DATA_MEM_ID => (data.data, u64::from(data.map_offset)),
            SPA_DATA_MEM_PTR => (self.memory_id, u64::from(data.data)),
            SPA_DATA_MEM_FD | SPA_DATA_DMA_BUF => (self.memory_id, u64::from(data.map_offset)),
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("Unsupported buffer memory type {other}"),
                ));
            }
        };
        Ok(ResolvedPlane {
            memory_id,
            base_offset,
            max_size: data.max_size,
            chunk,
        })
    }

    fn chunk_table_offset(&self) -> io::Result<u64> {
        let buffer_start = u64::from(self.offset);
        let buffer_end = buffer_start
            .checked_add(u64::from(self.size))
            .ok_or_else(|| invalid_data("PipeWire metadata end overflow"))?;
        let meta_bytes = self.metas.iter().try_fold(0_u64, |offset, meta| {
            let aligned_size = u64::from(meta.size).div_ceil(8) * 8;
            offset
                .checked_add(aligned_size)
                .ok_or_else(|| invalid_data("PipeWire metadata size overflow"))
        })?;
        let chunk_table_offset = buffer_start
            .checked_add(meta_bytes)
            .ok_or_else(|| invalid_data("PipeWire chunk table offset overflow"))?;
        let chunk_bytes = self
            .data
            .len()
            .checked_mul(16)
            .ok_or_else(|| invalid_data("PipeWire chunk table size overflow"))?;
        if chunk_table_offset
            .checked_add(chunk_bytes as u64)
            .is_none_or(|end| end > buffer_end)
        {
            return Err(invalid_data(
                "SPA chunk table exceeds buffer metadata block",
            ));
        }
        Ok(chunk_table_offset)
    }

    fn busy_count_offset(&self) -> io::Result<Option<u64>> {
        let mut offset = u64::from(self.offset);
        let metadata_end = offset
            .checked_add(u64::from(self.size))
            .ok_or_else(|| invalid_data("PipeWire metadata end overflow"))?;
        for meta in &self.metas {
            let aligned_size = u64::from(meta.size).div_ceil(8) * 8;
            let meta_end = offset
                .checked_add(aligned_size)
                .ok_or_else(|| invalid_data("PipeWire metadata offset overflow"))?;
            if meta_end > metadata_end {
                return Err(invalid_data("SPA metadata exceeds buffer metadata block"));
            }
            if meta.type_id == SPA_META_BUSY {
                if meta.size < 8 {
                    return Err(invalid_data("SPA_META_Busy is smaller than its ABI struct"));
                }
                return Ok(Some(offset + 4));
            }
            offset = meta_end;
        }
        Ok(None)
    }

    /// Read the optional `SPA_META_Busy` counter for this buffer.
    pub fn busy_count(&self, client: &PipeWireClient) -> io::Result<Option<u32>> {
        let Some(offset) = self.busy_count_offset()? else {
            return Ok(None);
        };
        let memory = client
            .memory(self.memory_id)
            .ok_or_else(|| invalid_data("PipeWire buffer references unknown metadata memory"))?;
        Ok(Some(memory.load_u32(offset, Ordering::Acquire)?))
    }

    /// Acquire an output buffer when the local client takes ownership, matching `spa_meta_busy` semantics.
    pub fn acquire(&self, client: &PipeWireClient) -> io::Result<()> {
        let Some(offset) = self.busy_count_offset()? else {
            return Ok(());
        };
        let memory = client
            .memory(self.memory_id)
            .ok_or_else(|| invalid_data("PipeWire buffer references unknown metadata memory"))?;
        memory.increment_u32(offset, Ordering::AcqRel)?;
        Ok(())
    }

    /// Release a consumed input buffer by decrementing its Busy metadata.
    ///
    /// Call only after the buffer has been consumed and before setting the
    /// port IO status back to `SPA_STATUS_NEED_DATA`.
    pub fn release(&self, client: &PipeWireClient) -> io::Result<()> {
        let Some(offset) = self.busy_count_offset()? else {
            return Ok(());
        };
        let memory = client
            .memory(self.memory_id)
            .ok_or_else(|| invalid_data("PipeWire buffer references unknown metadata memory"))?;
        memory.decrement_u32(offset, Ordering::AcqRel)?;
        Ok(())
    }
}

/// `SPA_IO_Buffers` status and selected buffer ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoBuffers {
    pub status: i32,
    pub buffer_id: u32,
}

impl IoBuffers {
    pub const STATUS_OK: i32 = 0;
    pub const STATUS_NEED_DATA: i32 = 1;
    pub const STATUS_HAVE_DATA: i32 = 2;
    pub const STATUS_STOPPED: i32 = 4;
    pub const STATUS_DRAINED: i32 = 8;

    pub fn read(
        client: &PipeWireClient,
        memory_id: u32,
        offset: u32,
        size: u32,
    ) -> io::Result<Self> {
        if size < 8 {
            return Err(invalid_data(
                "`SPA_IO_Buffers` area is smaller than 8 bytes",
            ));
        }
        let memory = client
            .memory(memory_id)
            .ok_or_else(|| invalid_data("`SPA_IO_Buffers` references unknown memory"))?;
        let status = memory.load_u32(u64::from(offset), Ordering::Acquire)?;
        let buffer_id = memory.load_u32(u64::from(offset) + 4, Ordering::Relaxed)?;
        Ok(Self {
            status: status.cast_signed(),
            buffer_id,
        })
    }

    pub fn write(
        client: &PipeWireClient,
        memory_id: u32,
        offset: u32,
        status: i32,
        buffer_id: u32,
    ) -> io::Result<()> {
        let memory = client
            .memory(memory_id)
            .ok_or_else(|| invalid_data("`SPA_IO_Buffers` references unknown memory"))?;
        memory.store_u32(u64::from(offset) + 4, buffer_id, Ordering::Relaxed)?;
        memory.store_u32(u64::from(offset), status.cast_unsigned(), Ordering::Release)
    }
}

const SPA_DATA_MEM_PTR: u32 = 1;
const SPA_DATA_MEM_FD: u32 = 2;
const SPA_DATA_DMA_BUF: u32 = 3;
const SPA_DATA_MEM_ID: u32 = 4;
const SPA_META_BUSY: u32 = 7;

fn decode_buffer_chunk(bytes: &[u8]) -> io::Result<BufferChunk> {
    if bytes.len() != 16 {
        return Err(invalid_data("Invalid SPA chunk size"));
    }
    Ok(BufferChunk {
        offset: u32::from_ne_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA chunk offset"))?,
        ),
        size: u32::from_ne_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA chunk size"))?,
        ),
        stride: i32::from_ne_bytes(
            bytes[8..12]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA chunk stride"))?,
        ),
        flags: u32::from_ne_bytes(
            bytes[12..16]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA chunk flags"))?,
        ),
    })
}

#[derive(Debug)]
pub struct PortUseBuffers {
    pub direction: PortDirection,
    pub port_id: u32,
    pub mix_id: u32,
    pub flags: u32,
    pub buffers: Vec<PipeWireBuffer>,
}

/// Commands defined by SPA's node command object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeCommand {
    Suspend,
    Pause,
    Start,
    Enable,
    Disable,
    Flush,
    Drain,
    Marker,
    ParamBegin,
    ParamEnd,
    RequestProcess,
    User,
}

impl TryFrom<u32> for NodeCommand {
    type Error = io::Error;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Suspend),
            1 => Ok(Self::Pause),
            2 => Ok(Self::Start),
            3 => Ok(Self::Enable),
            4 => Ok(Self::Disable),
            5 => Ok(Self::Flush),
            6 => Ok(Self::Drain),
            7 => Ok(Self::Marker),
            8 => Ok(Self::ParamBegin),
            9 => Ok(Self::ParamEnd),
            10 => Ok(Self::RequestProcess),
            11 => Ok(Self::User),
            _ => Err(invalid_data("Unknown SPA node command")),
        }
    }
}

const SPA_TYPE_OBJECT: u32 = 15;
const SPA_TYPE_COMMAND_NODE: u32 = 0x30002;

#[derive(Debug)]
pub enum ClientNodeEvent {
    Transport {
        read_fd: OwnedFd,
        write_fd: Option<OwnedFd>,
        memory_id: u32,
        offset: u32,
        size: u32,
    },
    SetParam {
        id: u32,
        flags: u32,
        param: RawPodOwned,
    },
    SetIo {
        id: u32,
        memory_id: u32,
        offset: u32,
        size: u32,
    },
    Event(RawPodOwned),
    Command(RawPodOwned),
    AddPort {
        direction: PortDirection,
        port_id: u32,
        properties: Vec<(String, String)>,
    },
    RemovePort {
        direction: PortDirection,
        port_id: u32,
    },
    PortSetParam {
        direction: PortDirection,
        port_id: u32,
        id: u32,
        flags: u32,
        param: RawPodOwned,
    },
    PortUseBuffers(PortUseBuffers),
    PortSetIo {
        direction: PortDirection,
        port_id: u32,
        mix_id: u32,
        id: u32,
        memory_id: u32,
        offset: u32,
        size: u32,
    },
    SetActivation {
        node_id: u32,
        signal_fd: Option<OwnedFd>,
        memory_id: u32,
        offset: u32,
        size: u32,
    },
    PortSetMixInfo {
        direction: PortDirection,
        port_id: u32,
        mix_id: u32,
        peer_id: u32,
        properties: Vec<(String, String)>,
    },
}

impl ClientNodeEvent {
    /// Drain the `ClientNode` transport eventfd after it becomes readable.
    pub fn drain_transport_signal(&self) -> io::Result<u64> {
        match self {
            Self::Transport { read_fd, .. } => crate::transport::read_eventfd(read_fd.as_fd()),
            _ => Err(invalid_input(
                "ClientNode event does not contain a transport read FD",
            )),
        }
    }

    /// Complete peer activation after this node has finished processing.
    pub fn trigger_peer(&self, client: &PipeWireClient) -> io::Result<bool> {
        match self {
            Self::SetActivation {
                signal_fd,
                memory_id,
                offset,
                size,
                ..
            } => match signal_fd {
                Some(signal_fd) => client.trigger_activation(
                    *memory_id,
                    u64::from(*offset),
                    *size,
                    signal_fd.as_fd(),
                ),
                None => Ok(false),
            },
            _ => Err(invalid_input(
                "ClientNode event does not contain a peer activation record",
            )),
        }
    }

    /// Decode a raw SPA node command object into its command ID.
    pub fn node_command(&self) -> io::Result<NodeCommand> {
        let Self::Command(pod) = self else {
            return Err(invalid_input("ClientNode event is not a command"));
        };
        let bytes = pod.data();
        if bytes.len() < 16 {
            return Err(invalid_data("SPA node command POD is truncated"));
        }
        let pod_size = u32::from_ne_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA node command POD size"))?,
        ) as usize;
        let pod_type = u32::from_ne_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA node command POD type"))?,
        );
        let object_type = u32::from_ne_bytes(
            bytes[8..12]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA node command object type"))?,
        );
        let command_id = u32::from_ne_bytes(
            bytes[12..16]
                .try_into()
                .map_err(|_| invalid_data("Invalid SPA node command ID"))?,
        );
        if pod_type != SPA_TYPE_OBJECT || pod_size < 8 || object_type != SPA_TYPE_COMMAND_NODE {
            return Err(invalid_data("POD is not a SPA node command object"));
        }
        if 8_usize
            .checked_add(pod_size)
            .is_none_or(|end| end > bytes.len())
        {
            return Err(invalid_data("SPA node command POD length is invalid"));
        }
        NodeCommand::try_from(command_id)
    }
}

/// Client-side proxy for a `PipeWire` `ClientNode`.
#[derive(Debug, Clone)]
pub struct ClientNode {
    object: PipeWireObject,
}

impl ClientNode {
    /// Create a `ClientNode` with the native `PipeWire` client-node factory.
    pub fn create(
        client: &mut PipeWireClient,
        properties: &[(String, String)],
    ) -> io::Result<Self> {
        let object = client.create_object(
            "client-node",
            CLIENT_NODE_INTERFACE,
            CLIENT_NODE_VERSION,
            properties,
        )?;
        Ok(Self { object })
    }

    #[must_use]
    pub fn object(&self) -> &PipeWireObject {
        &self.object
    }

    /// Destroy this `ClientNode` proxy through the Core object.
    pub fn destroy(self, client: &mut PipeWireClient) -> io::Result<()> {
        client.destroy_object(&self.object)
    }

    /// Request a server-side Node proxy for state and registry correlation.
    pub fn get_node(&self, client: &mut PipeWireClient, version: u32) -> io::Result<u32> {
        let new_id = client.allocate_object_id()?;
        let mut buffer = [0_u8; 64];
        let payload = Builder::new(&mut buffer)
            .push_struct(|fields| {
                fields
                    .push_int(version.cast_signed())
                    .push_int(new_id.cast_signed())
            })
            .build()
            .map_err(pod_error("Encode ClientNode.GetNode"))?;
        client.send_message(self.object.id, METHOD_GET_NODE, payload.to_vec())?;
        Ok(new_id)
    }

    /// Bind the server-side Node object associated with this `ClientNode`.
    pub fn get_node_with_id(
        &self,
        client: &mut PipeWireClient,
        version: u32,
        new_id: u32,
    ) -> io::Result<()> {
        let mut buffer = [0_u8; 64];
        let payload = Builder::new(&mut buffer)
            .push_struct(|fields| {
                fields
                    .push_int(version.cast_signed())
                    .push_int(new_id.cast_signed())
            })
            .build()
            .map_err(pod_error("Encode ClientNode.GetNode"))?;
        client.send_message(self.object.id, METHOD_GET_NODE, payload.to_vec())
    }

    /// Send an SPA node event to the server-side `ClientNode`.
    pub fn send_event(&self, client: &mut PipeWireClient, event: &RawPodOwned) -> io::Result<()> {
        let mut buffer = vec![0_u8; event.total_size() + 32];
        let payload = Builder::new(&mut buffer)
            .push_struct(|fields| fields.push_pod(event))
            .build()
            .map_err(pod_error("Encode ClientNode.Event"))?;
        client.send_message(self.object.id, METHOD_EVENT, payload.to_vec())
    }

    /// Report client-allocated buffer planes to the remote `ClientNode`.
    pub fn port_buffers(
        &self,
        client: &mut PipeWireClient,
        direction: PortDirection,
        port_id: u32,
        mix_id: u32,
        buffers: &[Vec<PortBufferFd>],
    ) -> io::Result<()> {
        let buffer_count = checked_count(buffers.len(), "ClientNode buffers")?;
        let data_counts = buffers
            .iter()
            .map(|buffer| checked_count(buffer.len(), "ClientNode buffer planes"))
            .collect::<io::Result<Vec<_>>>()?;
        let fd_count = buffers.iter().try_fold(0_usize, |count, buffer| {
            count.checked_add(buffer.len()).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "ClientNode FD count overflow")
            })
        })?;
        if fd_count > 64 {
            return Err(invalid_input(
                "ClientNode.PortBuffers exceeds the 64-FD limit",
            ));
        }
        let fds = buffers
            .iter()
            .flat_map(|buffer| {
                buffer
                    .iter()
                    .map(|data| data.fd.as_fd().try_clone_to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let capacity = fd_count
            .checked_mul(80)
            .and_then(|size| size.checked_add(buffers.len() * 16 + 128))
            .ok_or_else(|| invalid_input("ClientNode.PortBuffers payload size overflow"))?;
        let mut storage = vec![0_u8; capacity];
        let mut fd_index = 0_i32;
        let payload = Builder::new(&mut storage)
            .push_struct(|mut fields| {
                fields = fields
                    .push_int((direction as u32).cast_signed())
                    .push_int(port_id.cast_signed())
                    .push_int(mix_id.cast_signed())
                    .push_int(buffer_count);
                for (buffer, data_count) in buffers.iter().zip(data_counts) {
                    fields = fields.push_struct(|mut buffer_fields| {
                        buffer_fields = buffer_fields.push_int(data_count);
                        for data in buffer {
                            buffer_fields = buffer_fields
                                .push_id(Id(data.type_id))
                                .push_fd(fd_index)
                                .push_int(data.flags.cast_signed())
                                .push_int(data.map_offset.cast_signed())
                                .push_int(data.max_size.cast_signed());
                            fd_index += 1;
                        }
                        buffer_fields
                    });
                }
                fields
            })
            .build()
            .map_err(pod_error("Encode ClientNode.PortBuffers"))?;
        client.send_protocol_message_with_fds(
            self.object.id,
            METHOD_PORT_BUFFERS,
            payload.to_vec(),
            fds,
        )
    }

    /// Publish node info and optional SPA params using ClientNode.Update.
    pub fn update(
        &self,
        client: &mut PipeWireClient,
        info: &NodeInfo,
        params: &[Vec<u8>],
    ) -> io::Result<()> {
        let payload = encode_node_update(info, params)?;
        client.send_message(self.object.id, METHOD_UPDATE, payload)
    }

    /// Publish information and optional SPA params for one node port.
    pub fn update_port(
        &self,
        client: &mut PipeWireClient,
        direction: PortDirection,
        port_id: u32,
        info: &PortInfo,
        params: &[Vec<u8>],
    ) -> io::Result<()> {
        let payload = encode_port_update(direction, port_id, info, params)?;
        client.send_message(self.object.id, METHOD_PORT_UPDATE, payload)
    }

    /// Acknowledge an incoming `PortSetParam` with the exact SPA POD received.
    pub fn acknowledge_port_param(
        &self,
        client: &mut PipeWireClient,
        direction: PortDirection,
        port_id: u32,
        param: &RawPodOwned,
    ) -> io::Result<()> {
        let mut buffer = vec![0_u8; param.total_size() + 128];
        let encoded = Builder::new(&mut buffer)
            .push_struct(|fields| {
                fields
                    .push_int(direction as i32)
                    .push_int(port_id.cast_signed())
                    .push_int(CLIENT_NODE_PORT_UPDATE_PARAMS.cast_signed())
                    .push_int(1)
                    .push_pod(param)
                    .push_struct(|info| info)
            })
            .build()
            .map_err(pod_error("Encode ClientNode.PortUpdate acknowledgement"))?;
        client.send_message(self.object.id, METHOD_PORT_UPDATE, encoded.to_vec())
    }

    pub fn set_active(&self, client: &mut PipeWireClient, active: bool) -> io::Result<()> {
        let mut buffer = [0_u8; 64];
        let encoded = Builder::new(&mut buffer)
            .push_struct(|fields| fields.push_bool(active))
            .build()
            .map_err(pod_error("Encode ClientNode.SetActive"))?;
        client.send_message(self.object.id, METHOD_SET_ACTIVE, encoded.to_vec())
    }

    /// Decode one event received on this `ClientNode` proxy.
    /// Decode one event received on this `ClientNode` proxy.
    pub fn decode_event(&self, message: NativeMessage) -> io::Result<ClientNodeEvent> {
        if message.header.id != self.object.id {
            return Err(invalid_data(
                "`PipeWire` message belongs to a different ClientNode",
            ));
        }
        let NativeMessage {
            header,
            payload,
            fds,
        } = message;
        let opcode = header.opcode;
        let result = match opcode {
            EVENT_TRANSPORT => decode_transport_event(&payload, fds),
            EVENT_SET_PARAM => decode_set_param_event(&payload),
            EVENT_SET_IO => decode_set_io_event(&payload),
            EVENT_EVENT => decode_pod_event(&payload, true),
            EVENT_COMMAND => decode_pod_event(&payload, false),
            EVENT_ADD_PORT => decode_add_port_event(&payload),
            EVENT_REMOVE_PORT => decode_remove_port_event(&payload),
            EVENT_PORT_SET_PARAM => decode_port_set_param_event(&payload),
            EVENT_PORT_USE_BUFFERS => {
                decode_port_use_buffers(&payload, fds).map(ClientNodeEvent::PortUseBuffers)
            }
            EVENT_PORT_SET_IO => decode_port_set_io_event(&payload),
            EVENT_SET_ACTIVATION => decode_activation_event(&payload, fds),
            EVENT_PORT_SET_MIX_INFO => decode_port_mix_info_event(&payload),
            opcode => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Unsupported ClientNode event opcode {opcode}"),
            )),
        };
        result.map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("ClientNode event opcode {opcode}: {error}"),
            )
        })
    }
}

fn decode_transport_event(payload: &[u8], fds: Vec<OwnedFd>) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (read_index, write_index, memory_id, offset, size) =
        pop_event_struct(&mut parser, |fields| {
            Ok((
                fields.pop_fd()?.0,
                fields.pop_fd()?.0,
                fields.pop_int()? as u32,
                fields.pop_int()? as u32,
                fields.pop_int()? as u32,
            ))
        })?;
    let mut fds = fds.into_iter().map(Some).collect::<Vec<_>>();
    Ok(ClientNodeEvent::Transport {
        read_fd: take_fd(&mut fds, read_index)?,
        write_fd: take_optional_fd(&mut fds, write_index)?,
        memory_id,
        offset,
        size,
    })
}

fn decode_set_param_event(payload: &[u8]) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (id, flags, param) = pop_event_struct(&mut parser, |fields| {
        let id = fields.pop_id::<u32>()?.0;
        let flags = fields.pop_int()? as u32;
        let pod = fields.pop_raw_pod()?;
        Ok((id, flags, RawPodOwned::wrap(pod.data().to_vec())?))
    })?;
    Ok(ClientNodeEvent::SetParam { id, flags, param })
}

fn decode_set_io_event(payload: &[u8]) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (id, memory_id, offset, size) = pop_event_struct(&mut parser, |fields| {
        Ok((
            fields.pop_id::<u32>()?.0,
            fields.pop_int()? as u32,
            fields.pop_int()? as u32,
            fields.pop_int()? as u32,
        ))
    })?;
    Ok(ClientNodeEvent::SetIo {
        id,
        memory_id,
        offset,
        size,
    })
}

fn decode_pod_event(payload: &[u8], is_event: bool) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let pod = pop_event_struct(&mut parser, |fields| {
        let pod = fields.pop_raw_pod()?;
        RawPodOwned::wrap(pod.data().to_vec())
    })?;
    Ok(if is_event {
        ClientNodeEvent::Event(pod)
    } else {
        ClientNodeEvent::Command(pod)
    })
}

fn decode_add_port_event(payload: &[u8]) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (direction, port_id, properties) = pop_event_struct(&mut parser, |fields| {
        let direction = PortDirection::try_from(fields.pop_int()?)?;
        let port_id = fields.pop_int()? as u32;
        let properties = pop_properties(fields)?;
        Ok((direction, port_id, properties))
    })?;
    Ok(ClientNodeEvent::AddPort {
        direction,
        port_id,
        properties,
    })
}

fn decode_remove_port_event(payload: &[u8]) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (direction, port_id) = pop_event_struct(&mut parser, |fields| {
        Ok((
            PortDirection::try_from(fields.pop_int()?)?,
            fields.pop_int()? as u32,
        ))
    })?;
    Ok(ClientNodeEvent::RemovePort { direction, port_id })
}

fn decode_port_set_param_event(payload: &[u8]) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (direction, port_id, id, flags, param) = pop_event_struct(&mut parser, |fields| {
        let direction = PortDirection::try_from(fields.pop_int()?)?;
        let port_id = fields.pop_int()? as u32;
        let id = fields.pop_id::<u32>()?.0;
        let flags = fields.pop_int()? as u32;
        let pod = fields.pop_raw_pod()?;
        Ok((
            direction,
            port_id,
            id,
            flags,
            RawPodOwned::wrap(pod.data().to_vec())?,
        ))
    })?;
    Ok(ClientNodeEvent::PortSetParam {
        direction,
        port_id,
        id,
        flags,
        param,
    })
}

fn decode_port_set_io_event(payload: &[u8]) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let values = pop_event_struct(&mut parser, |fields| {
        Ok((
            PortDirection::try_from(fields.pop_int()?)?,
            fields.pop_int()? as u32,
            fields.pop_int()? as u32,
            fields.pop_id::<u32>()?.0,
            fields.pop_int()? as u32,
            fields.pop_int()? as u32,
            fields.pop_int()? as u32,
        ))
    })?;
    Ok(ClientNodeEvent::PortSetIo {
        direction: values.0,
        port_id: values.1,
        mix_id: values.2,
        id: values.3,
        memory_id: values.4,
        offset: values.5,
        size: values.6,
    })
}

fn decode_activation_event(payload: &[u8], fds: Vec<OwnedFd>) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (node_id, fd_index, memory_id, offset, size) = pop_event_struct(&mut parser, |fields| {
        Ok((
            fields.pop_int()? as u32,
            fields.pop_fd()?.0,
            fields.pop_int()? as u32,
            fields.pop_int()? as u32,
            fields.pop_int()? as u32,
        ))
    })?;
    let mut fds = fds.into_iter().map(Some).collect::<Vec<_>>();
    Ok(ClientNodeEvent::SetActivation {
        node_id,
        signal_fd: take_optional_fd(&mut fds, fd_index)?,
        memory_id,
        offset,
        size,
    })
}

fn decode_port_mix_info_event(payload: &[u8]) -> io::Result<ClientNodeEvent> {
    let mut parser = Parser::new(payload);
    let (direction, port_id, mix_id, peer_id, properties) =
        pop_event_struct(&mut parser, |fields| {
            Ok((
                PortDirection::try_from(fields.pop_int()?)?,
                fields.pop_int()? as u32,
                fields.pop_int()? as u32,
                fields.pop_int()? as u32,
                pop_properties(fields)?,
            ))
        })?;
    Ok(ClientNodeEvent::PortSetMixInfo {
        direction,
        port_id,
        mix_id,
        peer_id,
        properties,
    })
}

fn pop_event_struct<T>(
    parser: &mut Parser<'_>,
    decode: impl FnOnce(&mut Parser<'_>) -> Result<T, pipewire_native_spa::pod::Error>,
) -> io::Result<T> {
    parser
        .pop_struct(|fields| {
            let value = decode(fields)?;
            if fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "Unexpected trailing ClientNode event fields".into(),
                ));
            }
            Ok(value)
        })
        .map(|value| value.0)
        .map_err(pod_error("Decode `PipeWire` ClientNode event"))
}

pub fn decode_port_use_buffers(payload: &[u8], fds: Vec<OwnedFd>) -> io::Result<PortUseBuffers> {
    let mut fds = fds.into_iter().map(Some).collect::<Vec<_>>();
    let mut parser = Parser::new(payload);
    parser
        .pop_struct(|fields| {
            let direction = PortDirection::try_from(fields.pop_int()?)?;
            let port_id = fields.pop_int()? as u32;
            let mix_id = fields.pop_int()? as u32;
            let flags = fields.pop_int()? as u32;
            let buffer_count = bounded_pod_count(fields.pop_int()?)?;
            let mut buffers = Vec::with_capacity(buffer_count);
            for _ in 0..buffer_count {
                let memory_id = fields.pop_int()? as u32;
                let offset = fields.pop_int()? as u32;
                let size = fields.pop_int()? as u32;
                let meta_count = bounded_pod_count(fields.pop_int()?)?;
                let mut metas = Vec::with_capacity(meta_count);
                for _ in 0..meta_count {
                    metas.push(BufferMeta {
                        type_id: fields.pop_id::<u32>()?.0,
                        size: fields.pop_int()? as u32,
                    });
                }
                let data_count = bounded_pod_count(fields.pop_int()?)?;
                let mut data = Vec::with_capacity(data_count);
                let mut dma_buf_mappings = Vec::with_capacity(data_count);
                for _ in 0..data_count {
                    let type_id = fields.pop_id::<u32>()?.0;
                    let data_value = if type_id == SPA_DATA_MEM_FD || type_id == SPA_DATA_DMA_BUF {
                        let fd_index = fields.pop_int()?;
                        let fd = take_fd(&mut fds, fd_index).map_err(|error| {
                            pipewire_native_spa::pod::Error::Invalid(error.to_string())
                        })?;
                        let file = std::fs::File::from(fd);
                        let mapping = mmap_io::MemoryMappedFile::from_file(
                            file,
                            mmap_io::MmapMode::ReadOnly,
                            "PipeWire DMA-BUF",
                        )
                        .map_err(|error| {
                            pipewire_native_spa::pod::Error::Invalid(error.to_string())
                        })?;
                        dma_buf_mappings.push(Some(mapping));
                        0
                    } else {
                        dma_buf_mappings.push(None);
                        fields.pop_int()? as u32
                    };
                    data.push(BufferData {
                        type_id,
                        data: data_value,
                        flags: fields.pop_int()?.cast_unsigned(),
                        map_offset: fields.pop_int()? as u32,
                        max_size: fields.pop_int()? as u32,
                    });
                }
                buffers.push(PipeWireBuffer {
                    memory_id,
                    offset,
                    size,
                    metas,
                    data,
                    dma_buf_mappings,
                });
            }
            if fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "Unexpected trailing PortUseBuffers fields".into(),
                ));
            }
            Ok(PortUseBuffers {
                direction,
                port_id,
                mix_id,
                flags,
                buffers,
            })
        })
        .map(|value| value.0)
        .map_err(pod_error("Decode ClientNode.PortUseBuffers"))
}

fn encode_node_update(info: &NodeInfo, params: &[Vec<u8>]) -> io::Result<Vec<u8>> {
    let raw_params = raw_params(params)?;
    let count = checked_count(raw_params.len(), "node params")?;
    let property_count = checked_count(info.properties.len(), "node properties")?;
    let param_info_count = checked_count(info.params.len(), "node param info")?;
    let mut buffer = vec![0_u8; payload_capacity(params, &info.properties)];
    let encoded = Builder::new(&mut buffer)
        .push_struct(|fields| {
            let mut fields = fields
                .push_int(
                    (if params.is_empty() {
                        0
                    } else {
                        CLIENT_NODE_UPDATE_PARAMS
                    } | CLIENT_NODE_UPDATE_INFO)
                        .cast_signed(),
                )
                .push_int(count);
            for param in &raw_params {
                fields = fields.push_pod(param);
            }
            fields.push_struct(|node| {
                let node = node
                    .push_int(info.max_input_ports.cast_signed())
                    .push_int(info.max_output_ports.cast_signed())
                    .push_long(info.change_mask.cast_signed())
                    .push_long(info.flags.cast_signed());
                let node = push_properties(node, &info.properties, property_count);
                push_param_info(node, &info.params, param_info_count)
            })
        })
        .build()
        .map_err(pod_error("Encode ClientNode.Update"))?;
    Ok(encoded.to_vec())
}

fn encode_port_update(
    direction: PortDirection,
    port_id: u32,
    info: &PortInfo,
    params: &[Vec<u8>],
) -> io::Result<Vec<u8>> {
    let raw_params = raw_params(params)?;
    let count = checked_count(raw_params.len(), "port params")?;
    let property_count = checked_count(info.properties.len(), "port properties")?;
    let param_info_count = checked_count(info.params.len(), "port param info")?;
    let mut buffer = vec![0_u8; payload_capacity(params, &info.properties)];
    let encoded = Builder::new(&mut buffer)
        .push_struct(|fields| {
            let mut fields = fields
                .push_int(direction as i32)
                .push_int(port_id.cast_signed())
                .push_int(
                    (if params.is_empty() {
                        0
                    } else {
                        CLIENT_NODE_PORT_UPDATE_PARAMS
                    } | CLIENT_NODE_PORT_UPDATE_INFO)
                        .cast_signed(),
                )
                .push_int(count);
            for param in &raw_params {
                fields = fields.push_pod(param);
            }
            fields.push_struct(|port| {
                let port = port
                    .push_long(info.change_mask.cast_signed())
                    .push_long(info.flags.cast_signed())
                    .push_int(info.rate.0.cast_signed())
                    .push_int(info.rate.1.cast_signed());
                let port = push_properties(port, &info.properties, property_count);
                push_param_info(port, &info.params, param_info_count)
            })
        })
        .build()
        .map_err(pod_error("Encode ClientNode.PortUpdate"))?;
    Ok(encoded.to_vec())
}

fn raw_params(params: &[Vec<u8>]) -> io::Result<Vec<RawPodOwned>> {
    params
        .iter()
        .map(|bytes| RawPodOwned::wrap(bytes.clone()).map_err(pod_error("Parse SPA parameter POD")))
        .collect()
}

fn push_properties<'a>(
    fields: pipewire_native_spa::pod::builder::StructBuilder<'a>,
    properties: &[(String, String)],
    count: i32,
) -> pipewire_native_spa::pod::builder::StructBuilder<'a> {
    let mut fields = fields.push_int(count);
    for (key, value) in properties {
        fields = fields.push_string(key).push_string(value);
    }
    fields
}

fn push_param_info<'a>(
    fields: pipewire_native_spa::pod::builder::StructBuilder<'a>,
    params: &[ParamInfo],
    count: i32,
) -> pipewire_native_spa::pod::builder::StructBuilder<'a> {
    let mut fields = fields.push_int(count);
    for param in params {
        fields = fields
            .push_id(Id(param.id))
            .push_int(param.flags.cast_signed());
    }
    fields
}

fn pop_properties(
    fields: &mut Parser<'_>,
) -> Result<Vec<(String, String)>, pipewire_native_spa::pod::Error> {
    fields
        .pop_struct(|pairs| {
            let count = bounded_pod_count(pairs.pop_int()?)?;
            let mut properties = Vec::with_capacity(count);
            for _ in 0..count {
                properties.push((pairs.pop_string()?, pairs.pop_string()?));
            }
            if pairs.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "Trailing property fields".into(),
                ));
            }
            Ok(properties)
        })
        .map(|value| value.0)
}

fn take_optional_fd(fds: &mut [Option<OwnedFd>], index: i32) -> io::Result<Option<OwnedFd>> {
    if index == -1 {
        return Ok(None);
    }
    take_fd(fds, index).map(Some)
}

fn take_fd(fds: &mut [Option<OwnedFd>], index: i32) -> io::Result<OwnedFd> {
    let index = usize::try_from(index)
        .map_err(|_| invalid_data("Negative FD index in ClientNode event"))?;
    fds.get_mut(index)
        .and_then(Option::take)
        .ok_or_else(|| invalid_data("ClientNode event references an absent or duplicate FD"))
}

fn bounded_pod_count(count: i32) -> Result<usize, pipewire_native_spa::pod::Error> {
    if !(0..=MAX_ITEMS).contains(&count) {
        return Err(pipewire_native_spa::pod::Error::Invalid(
            "Invalid `PipeWire` property count".into(),
        ));
    }
    Ok(count as usize)
}

fn checked_count(count: usize, kind: &str) -> io::Result<i32> {
    i32::try_from(count).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Too many `PipeWire` {kind}"),
        )
    })
}

fn payload_capacity(params: &[Vec<u8>], properties: &[(String, String)]) -> usize {
    1024 + params.iter().map(Vec::len).sum::<usize>()
        + properties
            .iter()
            .map(|(key, value)| key.len() + value.len() + 32)
            .sum::<usize>()
}

fn pod_error(context: &'static str) -> impl FnOnce(pipewire_native_spa::pod::Error) -> io::Error {
    move |error| io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {error:?}"))
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire_native_spa::pod::builder::Builder;

    #[test]
    fn imports_dma_buf_fd_from_port_use_buffers() {
        use std::os::fd::OwnedFd;

        let mut payload = [0_u8; 512];
        let encoded = Builder::new(&mut payload)
            .push_struct(|fields| {
                fields
                    .push_int(PortDirection::Input as i32)
                    .push_int(0)
                    .push_int(0)
                    .push_int(0)
                    .push_int(1)
                    .push_int(7)
                    .push_int(0)
                    .push_int(64)
                    .push_int(0)
                    .push_int(1)
                    .push_id(Id(SPA_DATA_DMA_BUF))
                    .push_int(0)
                    .push_int(0)
                    .push_int(0)
                    .push_int(64)
            })
            .build()
            .unwrap()
            .to_vec();
        let fd = create_memfd("pw-dmabuf-test", &[]);
        let buffers = decode_port_use_buffers(&encoded, vec![OwnedFd::from(fd)]).unwrap();
        assert_eq!(buffers.buffers.len(), 1);
        assert_eq!(buffers.buffers[0].data[0].type_id, SPA_DATA_DMA_BUF);
        assert!(buffers.buffers[0].dma_buf_mappings[0].is_some());
    }

    #[test]
    fn decodes_port_use_buffers_without_abi_pointer_casts() {
        let mut buffer = [0_u8; 1024];
        let payload = Builder::new(&mut buffer)
            .push_struct(|fields| {
                fields
                    .push_int(PortDirection::Input as i32)
                    .push_int(0)
                    .push_int(0)
                    .push_int(0)
                    .push_int(1)
                    .push_int(7)
                    .push_int(64)
                    .push_int(4096)
                    .push_int(1)
                    .push_id(Id(7_u32))
                    .push_int(8)
                    .push_int(1)
                    .push_id(Id(1_u32))
                    .push_int(0)
                    .push_int(1)
                    .push_int(0)
                    .push_int(4096)
            })
            .build()
            .unwrap()
            .to_vec();

        let buffers = decode_port_use_buffers(&payload, Vec::new()).unwrap();
        assert_eq!(buffers.direction, PortDirection::Input);
        assert_eq!(buffers.buffers.len(), 1);
        assert_eq!(buffers.buffers[0].memory_id, 7);
        assert_eq!(buffers.buffers[0].metas[0].type_id, 7);
        assert_eq!(buffers.buffers[0].data[0].max_size, 4096);
    }

    #[test]
    fn port_buffers_sends_plane_fd_indices_and_rights() {
        use std::fs::File;

        let (mut client, mut server) = connect_test_client(1);
        let node = ClientNode {
            object: PipeWireObject {
                id: 3,
                interface: CLIENT_NODE_INTERFACE.to_owned(),
                version: CLIENT_NODE_VERSION,
            },
        };
        let buffers = vec![vec![
            PortBufferFd {
                type_id: SPA_DATA_MEM_ID,
                fd: File::open("/dev/null").unwrap().into(),
                flags: 3,
                map_offset: 0,
                max_size: 4096,
            },
            PortBufferFd {
                type_id: SPA_DATA_MEM_ID,
                fd: File::open("/dev/null").unwrap().into(),
                flags: 3,
                map_offset: 4096,
                max_size: 4096,
            },
        ]];
        node.port_buffers(&mut client, PortDirection::Input, 0, 0, &buffers)
            .unwrap();

        let message = server.receive().unwrap();
        assert_eq!(message.header.id, node.object().id);
        assert_eq!(message.header.opcode, METHOD_PORT_BUFFERS);
        assert_eq!(message.fds.len(), 2);
        let mut parser = Parser::new(&message.payload);
        let fields = parser
            .pop_struct(|fields| {
                let direction = fields.pop_int()?;
                let port_id = fields.pop_int()?;
                let mix_id = fields.pop_int()?;
                let count = fields.pop_int()?;
                let planes = fields.pop_struct(|buffer| {
                    let count = buffer.pop_int()?;
                    let mut planes = Vec::new();
                    for _ in 0..count {
                        planes.push((
                            buffer.pop_id::<u32>()?.0,
                            buffer.pop_fd()?.0,
                            buffer.pop_int()?,
                            buffer.pop_int()?,
                            buffer.pop_int()?,
                        ));
                    }
                    Ok(planes)
                })?;
                Ok((direction, port_id, mix_id, count, planes.0))
            })
            .unwrap()
            .0;
        assert_eq!(fields.0, PortDirection::Input as i32);
        assert_eq!(fields.1, 0);
        assert_eq!(fields.2, 0);
        assert_eq!(fields.3, 1);
        assert_eq!(fields.4.len(), 2);
        assert_eq!(fields.4[0].1, 0);
        assert_eq!(fields.4[1].1, 1);
    }

    #[test]
    fn reads_memid_and_memptr_planes_and_releases_busy_metadata() {
        let (mut client, mut server) = connect_test_client(11);
        let mut metadata = [0_u8; 24];
        metadata[0..4].copy_from_slice(&4_u32.to_ne_bytes()); // spa_meta_busy.flags
        metadata[4..8].copy_from_slice(&1_u32.to_ne_bytes()); // spa_meta_busy.count
        metadata[8..12].copy_from_slice(&4_u32.to_ne_bytes()); // spa_chunk.offset
        metadata[12..16].copy_from_slice(&8_u32.to_ne_bytes()); // spa_chunk.size
        metadata[16..20].copy_from_slice(&4_i32.to_ne_bytes()); // spa_chunk.stride
        metadata[20..24].copy_from_slice(&0_u32.to_ne_bytes()); // spa_chunk.flags
        let mut ptr_chunk = [0_u8; 16];
        ptr_chunk[0..4].copy_from_slice(&28_u32.to_ne_bytes());
        ptr_chunk[4..8].copy_from_slice(&8_u32.to_ne_bytes());
        ptr_chunk[8..12].copy_from_slice(&4_i32.to_ne_bytes());

        let metadata_file = create_memfd(
            "pw-buffer-meta",
            &[
                (0, &metadata),
                (256, &ptr_chunk),
                (512, b"abcd"),
                (540, b"WXYZ"),
            ],
        );
        let data_file = create_memfd("pw-buffer-data", &[(132, b"abcdefgh")]);
        add_memory(&mut client, &mut server, 7, metadata_file);
        add_memory(&mut client, &mut server, 8, data_file);

        let buffer = PipeWireBuffer {
            memory_id: 7,
            offset: 0,
            size: 24,
            metas: vec![BufferMeta {
                type_id: SPA_META_BUSY,
                size: 8,
            }],
            data: vec![BufferData {
                type_id: SPA_DATA_MEM_ID,
                data: 8,
                flags: 1,
                map_offset: 128,
                max_size: 32,
            }],
            dma_buf_mappings: vec![None],
        };
        let plane = buffer.read_plane(&client, 0).unwrap();
        assert_eq!(plane.bytes, b"abcdefgh");
        assert_eq!(plane.stride, 4);
        let mapped = buffer.map_plane(&client, 0).unwrap();
        assert_eq!(mapped.data.as_slices(), (b"abcdefgh".as_slice(), None));
        drop(mapped);

        assert_eq!(buffer.busy_count(&client).unwrap(), Some(1));
        buffer.acquire(&client).unwrap();
        assert_eq!(buffer.busy_count(&client).unwrap(), Some(2));
        buffer.release(&client).unwrap();
        assert_eq!(buffer.busy_count(&client).unwrap(), Some(1));
        buffer.release(&client).unwrap();
        assert_eq!(buffer.busy_count(&client).unwrap(), Some(0));

        let memptr_buffer = PipeWireBuffer {
            memory_id: 7,
            offset: 256,
            size: 16,
            metas: Vec::new(),
            data: vec![BufferData {
                type_id: SPA_DATA_MEM_PTR,
                data: 512,
                flags: 1,
                map_offset: 0,
                max_size: 32,
            }],
            dma_buf_mappings: vec![None],
        };
        let mapped = memptr_buffer.map_plane(&client, 0).unwrap();
        assert_eq!(
            mapped.data.as_slices(),
            (b"WXYZ".as_slice(), Some(b"abcd".as_slice()))
        );
        drop(mapped);
        assert_eq!(
            memptr_buffer.read_plane(&client, 0).unwrap().bytes,
            b"WXYZabcd"
        );
    }

    #[test]
    fn reads_and_updates_spa_io_buffers_safely() {
        let (mut client, mut server) = connect_test_client(1);
        let mut io = [0_u8; 8];
        io[..4].copy_from_slice(&IoBuffers::STATUS_HAVE_DATA.to_ne_bytes());
        io[4..].copy_from_slice(&3_u32.to_ne_bytes());
        add_memory(
            &mut client,
            &mut server,
            5,
            create_memfd("pw-io", &[(16, &io)]),
        );

        let state = IoBuffers::read(&client, 5, 16, 8).unwrap();
        assert_eq!(
            state,
            IoBuffers {
                status: IoBuffers::STATUS_HAVE_DATA,
                buffer_id: 3
            }
        );
        IoBuffers::write(&client, 5, 16, IoBuffers::STATUS_NEED_DATA, 3).unwrap();
        assert_eq!(
            IoBuffers::read(&client, 5, 16, 8).unwrap().status,
            IoBuffers::STATUS_NEED_DATA
        );
    }

    fn connect_test_client(node_id: u32) -> (PipeWireClient, crate::transport::NativeTransport) {
        use std::os::fd::OwnedFd;
        use std::os::unix::net::UnixStream;

        let (remote_fd, peer_fd) = UnixStream::pair().unwrap();
        let remote = crate::remote::PipeWireRemote::from_portal(OwnedFd::from(remote_fd), node_id);
        let client = PipeWireClient::connect(remote).unwrap();
        let mut server = crate::transport::NativeTransport::from_owned_fd(OwnedFd::from(peer_fd));
        let _hello = server.receive().unwrap();
        let _properties = server.receive().unwrap();
        (client, server)
    }

    fn create_memfd(name: &str, writes: &[(u64, &[u8])]) -> std::fs::File {
        use rustix::fs::{memfd_create, MemfdFlags};
        use std::fs::File;
        use std::os::unix::fs::FileExt;

        let fd = memfd_create(name, MemfdFlags::CLOEXEC).unwrap();
        let file = File::from(fd);
        file.set_len(4096).unwrap();
        for (offset, bytes) in writes {
            file.write_at(bytes, *offset).unwrap();
        }
        file
    }

    fn add_memory(
        client: &mut PipeWireClient,
        server: &mut crate::transport::NativeTransport,
        memory_id: u32,
        file: std::fs::File,
    ) {
        let mut payload_buffer = [0_u8; 128];
        let payload = Builder::new(&mut payload_buffer)
            .push_struct(|fields| {
                fields
                    .push_int(memory_id.cast_signed())
                    .push_id(Id(2_u32))
                    .push_fd(0)
                    .push_int(11)
            })
            .build()
            .unwrap()
            .to_vec();
        server
            .send(&NativeMessage {
                header: crate::transport::MessageHeader {
                    id: 0,
                    opcode: 6,
                    size: 0,
                    seq: 0,
                    n_fds: 0,
                },
                payload,
                fds: vec![file.into()],
            })
            .unwrap();
        let _add_mem = client.receive().unwrap();
    }

    #[test]
    fn creates_client_node_and_encodes_node_update() {
        let (mut client, mut server) = connect_test_client(11);
        let properties = vec![
            ("node.name".to_owned(), "test.capture".to_owned()),
            ("media.type".to_owned(), "Video".to_owned()),
        ];
        let node = ClientNode::create(&mut client, &properties).unwrap();
        assert_eq!(node.object().id, 3);
        assert_eq!(node.object().version, CLIENT_NODE_VERSION);

        let create = server.receive().unwrap();
        assert_eq!(create.header.id, 0);
        assert_eq!(create.header.opcode, 6);
        let mut parser = Parser::new(&create.payload);
        let create_fields = parser
            .pop_struct(|fields| {
                let factory = fields.pop_string()?;
                let interface = fields.pop_string()?;
                let version = fields.pop_int()?;
                let properties = fields.pop_struct(pop_string_pairs)?.0;
                let new_id = fields.pop_int()?;
                Ok((factory, interface, version, properties, new_id))
            })
            .unwrap()
            .0;
        assert_eq!(create_fields.0, "client-node");
        assert_eq!(create_fields.1, CLIENT_NODE_INTERFACE);
        assert_eq!(create_fields.2, CLIENT_NODE_VERSION.cast_signed());
        assert_eq!(create_fields.3, properties);
        assert_eq!(create_fields.4, node.object().id.cast_signed());

        let node_info = NodeInfo {
            max_input_ports: 1,
            max_output_ports: 0,
            change_mask: u64::from(NODE_CHANGE_FLAGS | NODE_CHANGE_PROPS | NODE_CHANGE_PARAMS),
            flags: u64::from(NODE_FLAG_ASYNC),
            properties: vec![("media.type".to_owned(), "Video".to_owned())],
            params: vec![ParamInfo { id: 3, flags: 0 }],
        };
        let caps =
            crate::format::encode_video_format_caps(&crate::format::VideoFormatCaps::default())
                .unwrap();
        node.update(&mut client, &node_info, &[caps]).unwrap();

        let update = server.receive().unwrap();
        assert_eq!(update.header.id, node.object().id);
        assert_eq!(update.header.opcode, METHOD_UPDATE);
        let mut parser = Parser::new(&update.payload);
        let fields = parser
            .pop_struct(|fields| {
                let change_mask = fields.pop_int()?;
                let count = fields.pop_int()?;
                let _format = fields.pop_raw_pod()?;
                let info = fields.pop_struct(|info| {
                    let max_input = info.pop_int()?;
                    let max_output = info.pop_int()?;
                    let change_mask = info.pop_long()?;
                    let flags = info.pop_long()?;
                    let properties = pop_string_pairs(info)?;
                    let params = pop_param_infos(info)?;
                    Ok((
                        max_input,
                        max_output,
                        change_mask,
                        flags,
                        properties,
                        params,
                    ))
                })?;
                Ok((change_mask, count, info.0))
            })
            .unwrap()
            .0;
        assert_eq!(
            fields.0,
            (CLIENT_NODE_UPDATE_PARAMS | CLIENT_NODE_UPDATE_INFO).cast_signed()
        );
        assert_eq!(fields.1, 1);
        assert_eq!(fields.2 .0, 1);
        assert_eq!(fields.2 .1, 0);
        assert_eq!(fields.2 .4, node_info.properties);
        assert_eq!(fields.2 .5, node_info.params);
    }

    #[test]
    fn encodes_video_port_update_with_io_and_busy_metadata() {
        use crate::format::{
            encode_buffers_io_param, encode_busy_meta_param, encode_video_buffers_param,
            encode_video_format_caps, NegotiatedVideoFormat, VideoFormat, VideoFormatCaps,
        };

        let (mut client, mut server) = connect_test_client(11);
        let node = ClientNode {
            object: PipeWireObject {
                id: 3,
                interface: CLIENT_NODE_INTERFACE.to_owned(),
                version: CLIENT_NODE_VERSION,
            },
        };
        let negotiated = NegotiatedVideoFormat {
            pixel_format: VideoFormat::Bgra,
            width: 1280,
            height: 720,
        };
        let params = vec![
            encode_video_format_caps(&VideoFormatCaps::default()).unwrap(),
            encode_video_buffers_param(negotiated).unwrap(),
            encode_buffers_io_param().unwrap(),
            encode_busy_meta_param().unwrap(),
        ];
        let info = PortInfo {
            change_mask: u64::from(
                PORT_CHANGE_FLAGS | PORT_CHANGE_RATE | PORT_CHANGE_PROPS | PORT_CHANGE_PARAMS,
            ),
            flags: 0,
            rate: (1, 1),
            properties: Vec::new(),
            params: vec![
                ParamInfo { id: 3, flags: 0 },
                ParamInfo { id: 5, flags: 0 },
                ParamInfo { id: 6, flags: 0 },
                ParamInfo { id: 7, flags: 0 },
            ],
        };
        node.update_port(&mut client, PortDirection::Input, 0, &info, &params)
            .unwrap();

        let message = server.receive().unwrap();
        assert_eq!(message.header.id, node.object().id);
        assert_eq!(message.header.opcode, METHOD_PORT_UPDATE);
        let mut parser = Parser::new(&message.payload);
        let fields = parser
            .pop_struct(|fields| {
                let direction = fields.pop_int()?;
                let port_id = fields.pop_int()?;
                let change_mask = fields.pop_int()?;
                let count = fields.pop_int()?;
                for _ in 0..count {
                    let _param = fields.pop_raw_pod()?;
                }
                let info = fields.pop_struct(|info| {
                    let change_mask = info.pop_long()?;
                    let flags = info.pop_long()?;
                    let rate = (info.pop_int()?, info.pop_int()?);
                    let properties = pop_string_pairs(info)?;
                    let params = pop_param_infos(info)?;
                    Ok((change_mask, flags, rate, properties, params))
                })?;
                Ok((direction, port_id, change_mask, count, info.0))
            })
            .unwrap()
            .0;
        assert_eq!(fields.0, PortDirection::Input as i32);
        assert_eq!(fields.1, 0);
        assert_eq!(
            fields.2,
            (CLIENT_NODE_PORT_UPDATE_PARAMS | CLIENT_NODE_PORT_UPDATE_INFO).cast_signed()
        );
        assert_eq!(fields.3, 4);
        assert_eq!(fields.4 .2, (1, 1));
        assert_eq!(fields.4 .4, info.params);
    }

    fn pop_string_pairs(
        fields: &mut Parser<'_>,
    ) -> Result<Vec<(String, String)>, pipewire_native_spa::pod::Error> {
        let count = usize::try_from(fields.pop_int()?).map_err(|_| {
            pipewire_native_spa::pod::Error::Invalid("Negative property count".into())
        })?;
        let mut pairs = Vec::with_capacity(count);
        for _ in 0..count {
            pairs.push((fields.pop_string()?, fields.pop_string()?));
        }
        Ok(pairs)
    }

    fn pop_param_infos(
        fields: &mut Parser<'_>,
    ) -> Result<Vec<ParamInfo>, pipewire_native_spa::pod::Error> {
        let count = usize::try_from(fields.pop_int()?)
            .map_err(|_| pipewire_native_spa::pod::Error::Invalid("Negative param count".into()))?;
        let mut params = Vec::with_capacity(count);
        for _ in 0..count {
            params.push(ParamInfo {
                id: fields.pop_id::<u32>()?.0,
                flags: fields.pop_int()?.cast_unsigned(),
            });
        }
        Ok(params)
    }

    #[test]
    fn decodes_transport_and_activation_descriptors() {
        use rustix::event::{eventfd, EventfdFlags};
        use std::os::fd::{AsRawFd, OwnedFd};
        use std::os::unix::net::UnixStream;

        let node = ClientNode {
            object: PipeWireObject {
                id: 3,
                interface: CLIENT_NODE_INTERFACE.to_owned(),
                version: CLIENT_NODE_VERSION,
            },
        };
        let (read_fd, _read_peer) = UnixStream::pair().unwrap();
        let (write_fd, _write_peer) = UnixStream::pair().unwrap();
        let expected_read = read_fd.as_raw_fd();
        let expected_write = write_fd.as_raw_fd();
        let mut payload_buffer = [0_u8; 128];
        let payload = Builder::new(&mut payload_buffer)
            .push_struct(|fields| {
                fields
                    .push_fd(1)
                    .push_fd(0)
                    .push_int(7)
                    .push_int(16)
                    .push_int(64)
            })
            .build()
            .unwrap()
            .to_vec();
        let transport = node
            .decode_event(NativeMessage {
                header: crate::transport::MessageHeader {
                    id: 3,
                    opcode: EVENT_TRANSPORT,
                    size: 0,
                    seq: 0,
                    n_fds: 0,
                },
                payload,
                fds: vec![OwnedFd::from(read_fd), OwnedFd::from(write_fd)],
            })
            .unwrap();
        let ClientNodeEvent::Transport {
            read_fd,
            write_fd,
            memory_id,
            offset,
            size,
        } = transport
        else {
            panic!("expected transport event");
        };
        assert_eq!(read_fd.as_raw_fd(), expected_write);
        assert_eq!(write_fd.unwrap().as_raw_fd(), expected_read);
        assert_eq!((memory_id, offset, size), (7, 16, 64));

        let signal = eventfd(0, EventfdFlags::CLOEXEC).unwrap();
        let peer_event = ClientNodeEvent::SetActivation {
            node_id: 22,
            signal_fd: Some(signal),
            memory_id: 4,
            offset: 8,
            size: 64,
        };
        let ClientNodeEvent::SetActivation {
            memory_id,
            offset,
            size,
            signal_fd,
            ..
        } = peer_event
        else {
            unreachable!();
        };
        assert_eq!((memory_id, offset, size), (4, 8, 64));
        assert!(signal_fd.unwrap().as_raw_fd() >= 0);
    }

    #[test]
    fn decodes_activation_event_without_optional_signal_fd() {
        let mut payload_buffer = [0_u8; 128];
        let payload = Builder::new(&mut payload_buffer)
            .push_struct(|fields| {
                fields
                    .push_int(22)
                    .push_fd(-1)
                    .push_int(4)
                    .push_int(8)
                    .push_int(64)
            })
            .build()
            .unwrap()
            .to_vec();

        let ClientNodeEvent::SetActivation {
            node_id,
            signal_fd,
            memory_id,
            offset,
            size,
        } = decode_activation_event(&payload, Vec::new()).unwrap()
        else {
            panic!("expected activation event");
        };

        assert_eq!((node_id, memory_id, offset, size), (22, 4, 8, 64));
        assert!(signal_fd.is_none());
    }

    #[test]
    fn decodes_transport_with_an_unavailable_optional_write_fd() {
        use std::os::fd::{AsRawFd, OwnedFd};
        use std::os::unix::net::UnixStream;

        let (read_fd, _peer) = UnixStream::pair().unwrap();
        let expected_read = read_fd.as_raw_fd();
        let mut payload_buffer = [0_u8; 128];
        let payload = Builder::new(&mut payload_buffer)
            .push_struct(|fields| {
                fields
                    .push_fd(0)
                    .push_fd(-1)
                    .push_int(7)
                    .push_int(16)
                    .push_int(64)
            })
            .build()
            .unwrap()
            .to_vec();

        let ClientNodeEvent::Transport {
            read_fd,
            write_fd,
            memory_id,
            offset,
            size,
        } = decode_transport_event(&payload, vec![OwnedFd::from(read_fd)]).unwrap()
        else {
            panic!("expected transport event");
        };

        assert_eq!(read_fd.as_raw_fd(), expected_read);
        assert!(write_fd.is_none());
        assert_eq!((memory_id, offset, size), (7, 16, 64));
    }

    #[test]
    fn decodes_spa_node_start_command() {
        let mut bytes = [0_u8; 16];
        bytes[0..4].copy_from_slice(&8_u32.to_ne_bytes());
        bytes[4..8].copy_from_slice(&SPA_TYPE_OBJECT.to_ne_bytes());
        bytes[8..12].copy_from_slice(&SPA_TYPE_COMMAND_NODE.to_ne_bytes());
        bytes[12..16].copy_from_slice(&2_u32.to_ne_bytes());
        let pod = RawPodOwned::wrap(bytes.to_vec()).unwrap();
        let event = ClientNodeEvent::Command(pod);
        assert_eq!(event.node_command().unwrap(), NodeCommand::Start);
    }

    #[test]
    fn rejects_oversized_port_buffer_lists() {
        let mut buffer = [0_u8; 128];
        let payload = Builder::new(&mut buffer)
            .push_struct(|fields| {
                fields
                    .push_int(PortDirection::Input as i32)
                    .push_int(0)
                    .push_int(0)
                    .push_int(0)
                    .push_int(MAX_ITEMS + 1)
            })
            .build()
            .unwrap()
            .to_vec();
        assert_eq!(
            decode_port_use_buffers(&payload, Vec::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
