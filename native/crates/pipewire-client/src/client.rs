//! High-level `PipeWire` client connection setup.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec};

use mmap_io::{MemoryMappedFile, MmapMode};
use std::os::unix::fs::FileExt;
use std::sync::atomic::Ordering;

use pipewire_native_spa::pod::builder::Builder;

use crate::remote::PipeWireRemote;
use crate::transport::{MessageHeader, NativeMessage, NativeTransport};

const CORE_OBJECT_ID: u32 = 0;
const REGISTRY_OBJECT_ID: u32 = 2;
const CORE_HELLO_OPCODE: u8 = 1;
const CORE_SYNC_OPCODE: u8 = 2;
const CORE_ERROR_OPCODE: u8 = 3;
const CORE_DONE_OPCODE: u8 = 1;
const CORE_GET_REGISTRY_OPCODE: u8 = 5;
const CORE_PROTOCOL_VERSION: i32 = crate::build_info::CORE_PROTOCOL_VERSION.cast_signed();
const REGISTRY_VERSION: i32 = crate::build_info::REGISTRY_PROTOCOL_VERSION.cast_signed();
const CLIENT_OBJECT_ID: u32 = 1;
const CLIENT_UPDATE_PROPERTIES_OPCODE: u8 = 2;
const CORE_PONG_OPCODE: u8 = 3;
const CORE_CREATE_OBJECT_OPCODE: u8 = 6;
const CORE_DESTROY_OPCODE: u8 = 7;
const CORE_ADD_MEM_OPCODE: u8 = 6;
const CORE_REMOVE_MEM_OPCODE: u8 = 7;
const CORE_PING_EVENT_OPCODE: u8 = 2;
const REGISTRY_BIND_OPCODE: u8 = 1;
const REGISTRY_GLOBAL_REMOVE_EVENT_OPCODE: u8 = 1;
const ASYNC_SEQ_BIT: u32 = 1 << 30;
const ASYNC_SEQ_MASK: u32 = ASYNC_SEQ_BIT - 1;
const MAX_GLOBAL_PROPERTIES: i32 = 4096;
const SPA_DATA_MEM_FD: u32 = 2;
const SPA_DATA_FLAG_MAPPABLE: u32 = 1 << 3;

/// A global object advertised by the `PipeWire` remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeWireGlobal {
    pub id: u32,
    pub permissions: u32,
    pub interface: String,
    pub version: u32,
    pub properties: Vec<(String, String)>,
}

impl PipeWireGlobal {
    /// Look up one registry property without allocating a temporary map.
    #[must_use]
    pub fn property(&self, key: &str) -> Option<&str> {
        self.properties
            .iter()
            .find_map(|(name, value)| (name == key).then_some(value.as_str()))
    }

    /// Return the stable target selector used by `PipeWire` stream properties.
    #[must_use]
    pub fn target_object(&self) -> Option<&str> {
        self.property("object.serial")
            .or_else(|| self.property("node.name"))
    }
}

/// An object allocated in the remote `PipeWire` connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeWireObject {
    pub id: u32,
    pub interface: String,
    pub version: u32,
}

/// A memory object supplied by the `PipeWire` core through Core.AddMem.
pub struct PipeWireMemory {
    file: File,
    mapping: Option<MemoryMappedFile>,
    pub memory_type: u32,
    pub flags: u32,
}

impl PipeWireMemory {
    fn new(file: OwnedFd, memory_type: u32, flags: u32) -> io::Result<Self> {
        let file = File::from(file);
        let mapping = if (memory_type == SPA_DATA_MEM_FD || flags & SPA_DATA_FLAG_MAPPABLE != 0)
            && file.metadata()?.len() > 0
        {
            Some(
                MemoryMappedFile::from_file(
                    file.try_clone()?,
                    MmapMode::ReadWrite,
                    "PipeWire shared memory",
                )
                .map_err(io::Error::other)?,
            )
        } else {
            None
        };
        Ok(Self {
            file,
            mapping,
            memory_type,
            flags,
        })
    }

    /// Load a shared 32-bit field atomically from a mapped SPA region.
    pub fn load_u32(&self, offset: u64, ordering: Ordering) -> io::Result<u32> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire shared memory does not support an atomic read-write mapping",
            )
        })?;
        let value = mapping.atomic_u32(offset).map_err(io::Error::other)?;
        Ok(value.load(ordering))
    }

    /// Store a shared 32-bit field atomically in a mapped SPA region.
    pub fn store_u32(&self, offset: u64, value: u32, ordering: Ordering) -> io::Result<()> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire shared memory does not support an atomic read-write mapping",
            )
        })?;
        let target = mapping.atomic_u32(offset).map_err(io::Error::other)?;
        target.store(value, ordering);
        Ok(())
    }

    /// Store a shared 64-bit timestamp atomically.
    pub fn store_u64(&self, offset: u64, value: u64, ordering: Ordering) -> io::Result<()> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire shared memory does not support an atomic read-write mapping",
            )
        })?;
        let target = mapping.atomic_u64(offset).map_err(io::Error::other)?;
        target.store(value, ordering);
        Ok(())
    }

    /// Compare and replace a shared 32-bit value atomically.
    pub fn compare_exchange_u32(
        &self,
        offset: u64,
        current: u32,
        new: u32,
        success: Ordering,
        failure: Ordering,
    ) -> io::Result<bool> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire shared memory does not support an atomic read-write mapping",
            )
        })?;
        let target = mapping.atomic_u32(offset).map_err(io::Error::other)?;
        Ok(target
            .compare_exchange(current, new, success, failure)
            .is_ok())
    }

    /// Increment a shared counter without racing the `PipeWire` server.
    pub fn increment_u32(&self, offset: u64, ordering: Ordering) -> io::Result<u32> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire shared memory does not support an atomic read-write mapping",
            )
        })?;
        let target = mapping.atomic_u32(offset).map_err(io::Error::other)?;
        Ok(target.fetch_add(1, ordering))
    }

    /// Decrement a shared counter, returning its new value.
    pub fn decrement_u32(&self, offset: u64, ordering: Ordering) -> io::Result<u32> {
        self.decrement_if_nonzero_u32(offset, ordering)?
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "PipeWire shared metadata counter underflow",
                )
            })
    }

    /// Decrement a shared counter unless it is already zero.
    pub fn decrement_if_nonzero_u32(
        &self,
        offset: u64,
        ordering: Ordering,
    ) -> io::Result<Option<u32>> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire shared memory does not support an atomic read-write mapping",
            )
        })?;
        let target = mapping.atomic_u32(offset).map_err(io::Error::other)?;
        match target.try_update(ordering, Ordering::Relaxed, |value| value.checked_sub(1)) {
            Ok(previous) => Ok(Some(previous - 1)),
            Err(0) => Ok(None),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "PipeWire shared metadata counter update failed",
            )),
        }
    }

    /// Borrow a zero-copy view into mapped shared memory.
    pub fn map_slice_at(&self, offset: u64, length: usize) -> io::Result<mmap_io::MappedSlice<'_>> {
        let mapping = self.mapping.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "PipeWire memory is not mapped for zero-copy access",
            )
        })?;
        let length = u64::try_from(length).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Mapped slice length overflow")
        })?;
        mapping.as_slice(offset, length).map_err(io::Error::other)
    }

    /// Read bytes at an offset, using the safe shared mapping when available.
    pub fn read_at(&self, offset: u64, length: usize) -> io::Result<Vec<u8>> {
        let mut bytes = vec![0; length];
        if let Some(mapping) = &self.mapping {
            mapping
                .read_into(offset, &mut bytes)
                .map_err(io::Error::other)?;
            return Ok(bytes);
        }
        let mut read = 0;
        while read < bytes.len() {
            let count = self
                .file
                .read_at(&mut bytes[read..], offset + read as u64)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "PipeWire memory read reached EOF",
                ));
            }
            read += count;
        }
        Ok(bytes)
    }

    /// Write bytes at an offset without mapping memory or using application unsafe.
    pub fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let mut written = 0;
        while written < bytes.len() {
            let count = self
                .file
                .write_at(&bytes[written..], offset + written as u64)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "PipeWire memory write made no progress",
                ));
            }
            written += count;
        }
        Ok(())
    }
}

/// A connected `PipeWire` native-protocol client.
///
/// The client owns native protocol sequencing, proxy allocation, remote
/// memory and registry events. Media stream logic is implemented by `ClientNode`.
pub struct PipeWireClient {
    transport: NativeTransport,
    node_id: u32,
    next_seq: u32,
    next_object_id: u32,
    memory: HashMap<u32, PipeWireMemory>,
    globals: HashMap<u32, PipeWireGlobal>,
    registry_created: bool,
    registry_synced: bool,
}

impl PipeWireClient {
    /// Connect to a portal-provided remote and send the native Core.Hello request.
    pub fn connect(remote: PipeWireRemote) -> io::Result<Self> {
        Self::connect_with_properties(remote, &[])
    }

    /// Connect and publish application properties on the native Client object.
    pub fn connect_with_properties(
        remote: PipeWireRemote,
        properties: &[(String, String)],
    ) -> io::Result<Self> {
        let (fd, node_id) = remote.into_parts();
        let mut client = Self {
            transport: NativeTransport::from_owned_fd(fd),
            node_id,
            next_seq: 0,
            next_object_id: 3,
            memory: HashMap::new(),
            globals: HashMap::new(),
            registry_created: false,
            registry_synced: false,
        };
        client.send_message(
            CORE_OBJECT_ID,
            CORE_HELLO_OPCODE,
            encode_ints(&[CORE_PROTOCOL_VERSION])?,
        )?;
        client.send_message(
            CLIENT_OBJECT_ID,
            CLIENT_UPDATE_PROPERTIES_OPCODE,
            encode_properties(properties)?,
        )?;
        Ok(client)
    }

    /// Return the node ID selected by the portal.
    #[must_use]
    pub const fn node_id(&self) -> u32 {
        self.node_id
    }

    /// Enumerate globals exported by the portal's `PipeWire` remote.
    ///
    /// The Core.Sync barrier ensures all registry globals preceding the
    /// barrier have been received before this method returns.
    pub fn list_globals(&mut self) -> io::Result<Vec<PipeWireGlobal>> {
        self.request_registry()?;
        if !self.registry_synced {
            for message in self.sync()? {
                self.handle_registry_event(&message)?;
            }
            self.registry_synced = true;
        }
        Ok(self.sorted_globals())
    }

    /// Enumerate globals with a deadline and cancellation predicate.
    pub fn list_globals_with_cancellation<F>(
        &mut self,
        timeout: Duration,
        mut cancelled: F,
    ) -> io::Result<Vec<PipeWireGlobal>>
    where
        F: FnMut() -> bool,
    {
        self.request_registry()?;
        if !self.registry_synced {
            for message in self.sync_with_cancellation(timeout, &mut cancelled)? {
                self.handle_registry_event(&message)?;
            }
            self.registry_synced = true;
        }
        Ok(self.sorted_globals())
    }

    fn request_registry(&mut self) -> io::Result<()> {
        if !self.registry_created {
            self.send_message(
                CORE_OBJECT_ID,
                CORE_GET_REGISTRY_OPCODE,
                encode_ints(&[REGISTRY_VERSION, REGISTRY_OBJECT_ID.cast_signed()])?,
            )?;
            self.registry_created = true;
        }
        Ok(())
    }

    fn sorted_globals(&self) -> Vec<PipeWireGlobal> {
        let mut globals = self.globals.values().cloned().collect::<Vec<_>>();
        globals.sort_unstable_by_key(|global| global.id);
        globals
    }

    /// Wait for a registry global while continuing to dispatch native events.
    pub fn wait_for_global(
        &mut self,
        id: u32,
        timeout: Duration,
    ) -> io::Result<Option<PipeWireGlobal>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(global) = self.globals.get(&id) {
                return Ok(Some(global.clone()));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            if let Some(message) = self.receive_timeout(remaining.min(Duration::from_millis(50)))? {
                self.handle_registry_event(&message)?;
            }
        }
    }

    fn handle_registry_event(&mut self, message: &NativeMessage) -> io::Result<()> {
        if message.header.id != REGISTRY_OBJECT_ID {
            return Ok(());
        }
        match message.header.opcode {
            0 => {
                let global = decode_global(&message.payload)?;
                self.globals.insert(global.id, global);
            }
            REGISTRY_GLOBAL_REMOVE_EVENT_OPCODE => {
                self.globals.remove(&decode_single_int(&message.payload)?);
            }
            _ => {}
        }
        Ok(())
    }

    /// Execute a native Core.Sync round trip and return intervening protocol events.
    pub fn sync(&mut self) -> io::Result<Vec<NativeMessage>> {
        let sync_seq = ASYNC_SEQ_BIT | (self.next_seq & ASYNC_SEQ_MASK);
        self.send_message(
            CORE_OBJECT_ID,
            CORE_SYNC_OPCODE,
            encode_ints(&[CORE_OBJECT_ID.cast_signed(), sync_seq.cast_signed()])?,
        )?;
        let mut events = Vec::new();
        loop {
            let message = self.receive()?;
            if message.header.id == CORE_OBJECT_ID && message.header.opcode == CORE_DONE_OPCODE {
                let (id, seq) = decode_done(&message.payload)?;
                if id == CORE_OBJECT_ID && seq == sync_seq {
                    return Ok(events);
                }
            }
            events.push(message);
        }
    }

    /// Execute Core.Sync with a deadline and cancellation predicate.
    pub fn sync_with_cancellation<F>(
        &mut self,
        timeout: Duration,
        mut cancelled: F,
    ) -> io::Result<Vec<NativeMessage>>
    where
        F: FnMut() -> bool,
    {
        let sync_seq = ASYNC_SEQ_BIT | (self.next_seq & ASYNC_SEQ_MASK);
        self.send_message(
            CORE_OBJECT_ID,
            CORE_SYNC_OPCODE,
            encode_ints(&[CORE_OBJECT_ID.cast_signed(), sync_seq.cast_signed()])?,
        )?;
        let deadline = std::time::Instant::now() + timeout;
        let mut events = Vec::new();
        loop {
            if cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "PipeWire protocol synchronization was cancelled",
                ));
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Timed out waiting for PipeWire Core.Sync",
                ));
            }
            let Some(message) = self.receive_timeout(remaining.min(Duration::from_millis(50)))?
            else {
                continue;
            };
            if message.header.id == CORE_OBJECT_ID && message.header.opcode == CORE_DONE_OPCODE {
                let (id, seq) = decode_done(&message.payload)?;
                if id == CORE_OBJECT_ID && seq == sync_seq {
                    return Ok(events);
                }
            }
            events.push(message);
        }
    }

    /// Allocate a client-side object ID.
    pub fn allocate_object_id(&mut self) -> io::Result<u32> {
        let id = self.next_object_id;
        self.next_object_id = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("PipeWire proxy ID space exhausted"))?;
        Ok(id)
    }

    /// Bind a global interface exposed by the remote registry.
    pub fn bind_global(&mut self, global: &PipeWireGlobal) -> io::Result<PipeWireObject> {
        self.bind_global_version(global, global.version)
    }

    /// Bind a global using a caller-selected interface version.
    pub fn bind_global_version(
        &mut self,
        global: &PipeWireGlobal,
        version: u32,
    ) -> io::Result<PipeWireObject> {
        if version == 0 || version > global.version {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Requested PipeWire interface version is unsupported by the global",
            ));
        }
        let id = self.allocate_object_id()?;
        let payload = encode_registry_bind(global, version, id)?;
        self.send_message(REGISTRY_OBJECT_ID, REGISTRY_BIND_OPCODE, payload)?;
        Ok(PipeWireObject {
            id,
            interface: global.interface.clone(),
            version,
        })
    }

    /// Create a server object using a registered `PipeWire` factory.
    pub fn create_object(
        &mut self,
        factory: &str,
        interface: &str,
        version: u32,
        properties: &[(String, String)],
    ) -> io::Result<PipeWireObject> {
        let id = self.allocate_object_id()?;
        let payload = encode_create_object(factory, interface, version, properties, id)?;
        self.send_message(CORE_OBJECT_ID, CORE_CREATE_OBJECT_OPCODE, payload)?;
        Ok(PipeWireObject {
            id,
            interface: interface.to_owned(),
            version,
        })
    }

    /// Destroy a previously-created proxy on the remote core.
    pub fn destroy_object(&mut self, object: &PipeWireObject) -> io::Result<()> {
        self.send_message(
            CORE_OBJECT_ID,
            CORE_DESTROY_OPCODE,
            encode_ints(&[object.id.cast_signed()])?,
        )
    }

    /// Access a memory block announced through Core.AddMem.
    pub fn memory(&self, id: u32) -> Option<&PipeWireMemory> {
        self.memory.get(&id)
    }

    /// Trigger a peer using the private `PipeWire` node-activation record.
    ///
    /// The shared layout places status at +0, the first pending counter at
    /// +16, and the monotonic signal timestamp at +32. Use the CAS transition
    /// required by current `PipeWire` servers; it also works with baseline
    /// activation records while their status is `NOT_TRIGGERED`.
    pub fn trigger_activation(
        &self,
        memory_id: u32,
        offset: u64,
        size: u32,
        signal_fd: BorrowedFd<'_>,
    ) -> io::Result<bool> {
        const ACTIVATION_MIN_SIZE: u32 = 40;
        const STATUS_OFFSET: u64 = 0;
        const PENDING_OFFSET: u64 = 16;
        const SIGNAL_TIME_OFFSET: u64 = 32;
        const STATUS_NOT_TRIGGERED: u32 = 0;
        const STATUS_TRIGGERED: u32 = 1;

        if size < ACTIVATION_MIN_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "PipeWire peer activation record is truncated",
            ));
        }
        let memory = self.memory(memory_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "PipeWire peer activation references unknown memory",
            )
        })?;
        let Some(pending) =
            memory.decrement_if_nonzero_u32(offset + PENDING_OFFSET, Ordering::AcqRel)?
        else {
            return Ok(false);
        };
        if pending != 0 {
            return Ok(false);
        }

        if !memory.compare_exchange_u32(
            offset + STATUS_OFFSET,
            STATUS_NOT_TRIGGERED,
            STATUS_TRIGGERED,
            Ordering::AcqRel,
            Ordering::Acquire,
        )? {
            return Ok(false);
        }

        let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
        let signal_time = i128::from(now.tv_sec) * 1_000_000_000 + i128::from(now.tv_nsec);
        let signal_time = u64::try_from(signal_time).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "Monotonic timestamp overflow")
        })?;
        memory.store_u64(offset + SIGNAL_TIME_OFFSET, signal_time, Ordering::Release)?;
        crate::transport::write_eventfd(signal_fd, 1)?;
        Ok(true)
    }

    /// Poll the native-protocol socket and additional descriptors together.
    ///
    /// The returned flags are ordered with the protocol socket first, followed
    /// by the additional descriptors. This lets stream users wait for protocol
    /// events and `ClientNode` transport eventfds without unsafe polling wrappers.
    pub fn poll_readable(
        &self,
        additional: &[BorrowedFd<'_>],
        timeout: Option<Duration>,
    ) -> io::Result<Vec<PollFlags>> {
        let mut descriptors = Vec::with_capacity(additional.len() + 1);
        descriptors.push(PollFd::new(&self.transport, PollFlags::IN));
        descriptors.extend(
            additional
                .iter()
                .map(|fd| PollFd::from_borrowed_fd(*fd, PollFlags::IN)),
        );
        let timeout = timeout.map(|duration| Timespec {
            tv_sec: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(duration.subsec_nanos()),
        });
        rustix::event::poll(&mut descriptors, timeout.as_ref())?;
        Ok(descriptors.iter().map(PollFd::revents).collect())
    }

    /// Receive a complete protocol message with a bounded wait. Core-level
    /// ping and memory events are handled before the message is returned.
    pub fn receive_timeout(&mut self, timeout: Duration) -> io::Result<Option<NativeMessage>> {
        let Some(mut message) = self.transport.receive_timeout(timeout)? else {
            return Ok(None);
        };
        self.handle_core_event(&mut message)?;
        Ok(Some(message))
    }

    /// Receive the next complete native-protocol message.
    pub fn receive(&mut self) -> io::Result<NativeMessage> {
        loop {
            if let Some(message) = self.receive_timeout(Duration::from_millis(50))? {
                return Ok(message);
            }
        }
    }

    fn handle_core_event(&mut self, message: &mut NativeMessage) -> io::Result<()> {
        if message.header.id != CORE_OBJECT_ID {
            return Ok(());
        }
        match message.header.opcode {
            CORE_PING_EVENT_OPCODE => {
                let (id, seq) = decode_done(&message.payload)?;
                self.send_message(
                    CORE_OBJECT_ID,
                    CORE_PONG_OPCODE,
                    encode_ints(&[id.cast_signed(), seq.cast_signed()])?,
                )?;
            }
            CORE_ADD_MEM_OPCODE => {
                let mut parser = pipewire_native_spa::pod::parser::Parser::new(&message.payload);
                let (memory_id, memory_type, fd_index, flags) = parser
                    .pop_struct(|fields| {
                        let memory_id = fields.pop_int().map_err(|error| {
                            pipewire_native_spa::pod::Error::Invalid(format!(
                                "Core.AddMem memory id: {error:?}"
                            ))
                        })? as u32;
                        let memory_type = fields
                            .pop_id::<u32>()
                            .map_err(|error| {
                                pipewire_native_spa::pod::Error::Invalid(format!(
                                    "Core.AddMem memory type: {error:?}"
                                ))
                            })?
                            .0;
                        let fd_index = fields
                            .pop_fd()
                            .map_err(|error| {
                                pipewire_native_spa::pod::Error::Invalid(format!(
                                    "Core.AddMem FD index: {error:?}"
                                ))
                            })?
                            .0;
                        let flags = fields.pop_int().map_err(|error| {
                            pipewire_native_spa::pod::Error::Invalid(format!(
                                "Core.AddMem flags: {error:?}"
                            ))
                        })? as u32;
                        Ok((memory_id, memory_type, fd_index, flags))
                    })
                    .map(|value| value.0)
                    .map_err(pod_error("Decode PipeWire Core.AddMem"))?;
                let fd_index = usize::try_from(fd_index).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "Negative PipeWire FD index")
                })?;
                if fd_index >= message.fds.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "PipeWire Core.AddMem references an absent FD",
                    ));
                }
                let fd: OwnedFd = message.fds.swap_remove(fd_index);
                let memory = PipeWireMemory::new(fd, memory_type, flags)?;
                self.memory.insert(memory_id, memory);
            }
            CORE_REMOVE_MEM_OPCODE => {
                let memory_id = decode_single_int(&message.payload)?;
                self.memory.remove(&memory_id);
            }
            CORE_ERROR_OPCODE => return Err(decode_core_error(&message.payload)?),
            _ => {}
        }
        Ok(())
    }

    /// Send a native-protocol message.
    pub fn send(&mut self, message: &NativeMessage) -> io::Result<()> {
        self.transport.send(message)
    }

    pub(crate) fn send_protocol_message(
        &mut self,
        id: u32,
        opcode: u8,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        self.send_protocol_message_with_fds(id, opcode, payload, Vec::new())
    }

    pub(crate) fn send_protocol_message_with_fds(
        &mut self,
        id: u32,
        opcode: u8,
        payload: Vec<u8>,
        fds: Vec<OwnedFd>,
    ) -> io::Result<()> {
        let seq = self.next_seq;
        self.next_seq = (self.next_seq + 1) & ASYNC_SEQ_MASK;
        self.transport.send(&NativeMessage {
            header: MessageHeader {
                id,
                opcode,
                size: 0,
                seq,
                n_fds: 0,
            },
            payload,
            fds,
        })
    }

    pub(crate) fn send_message(&mut self, id: u32, opcode: u8, payload: Vec<u8>) -> io::Result<()> {
        self.send_protocol_message(id, opcode, payload)
    }
}

fn encode_ints(values: &[i32]) -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 256];
    let encoded = Builder::new(&mut buffer)
        .push_struct(|mut structure| {
            for value in values {
                structure = structure.push_int(*value);
            }
            structure
        })
        .build()
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Encode PipeWire protocol structure: {error:?}"),
            )
        })?;
    Ok(encoded.to_vec())
}

fn encode_properties(properties: &[(String, String)]) -> io::Result<Vec<u8>> {
    let count = i32::try_from(properties.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Too many PipeWire client properties",
        )
    })?;
    let mut buffer = vec![
        0_u8;
        64 + properties
            .iter()
            .map(|(key, value)| key.len() + value.len() + 32)
            .sum::<usize>()
    ];
    let encoded = Builder::new(&mut buffer)
        .push_struct(|outer| {
            outer.push_struct(|pairs| {
                let mut pairs = pairs.push_int(count);
                for (key, value) in properties {
                    pairs = pairs.push_string(key).push_string(value);
                }
                pairs
            })
        })
        .build()
        .map_err(pod_error("Encode PipeWire client properties"))?;
    Ok(encoded.to_vec())
}

fn encode_registry_bind(global: &PipeWireGlobal, version: u32, new_id: u32) -> io::Result<Vec<u8>> {
    let mut buffer = [0_u8; 1024];
    let encoded = Builder::new(&mut buffer)
        .push_struct(|fields| {
            fields
                .push_int(global.id.cast_signed())
                .push_string(&global.interface)
                .push_int(version.cast_signed())
                .push_int(new_id.cast_signed())
        })
        .build()
        .map_err(pod_error("Encode PipeWire Registry.Bind"))?;
    Ok(encoded.to_vec())
}

fn encode_create_object(
    factory: &str,
    interface: &str,
    version: u32,
    properties: &[(String, String)],
    new_id: u32,
) -> io::Result<Vec<u8>> {
    let count = i32::try_from(properties.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Too many PipeWire object properties",
        )
    })?;
    let mut buffer = vec![
        0_u8;
        4096 + properties
            .iter()
            .map(|(key, value)| key.len() + value.len() + 32)
            .sum::<usize>()
    ];
    let encoded = Builder::new(&mut buffer)
        .push_struct(|fields| {
            fields
                .push_string(factory)
                .push_string(interface)
                .push_int(version.cast_signed())
                .push_struct(|pairs| {
                    let mut pairs = pairs.push_int(count);
                    for (key, value) in properties {
                        pairs = pairs.push_string(key).push_string(value);
                    }
                    pairs
                })
                .push_int(new_id.cast_signed())
        })
        .build()
        .map_err(pod_error("Encode PipeWire Core.CreateObject"))?;
    Ok(encoded.to_vec())
}

fn pop_int(
    fields: &mut pipewire_native_spa::pod::parser::Parser<'_>,
) -> Result<i32, pipewire_native_spa::pod::Error> {
    fields.pop_int()
}

fn decode_single_int(payload: &[u8]) -> io::Result<u32> {
    pipewire_native_spa::pod::parser::Parser::new(payload)
        .pop_struct(pop_int)
        .map(|value| value.0.cast_unsigned())
        .map_err(pod_error("Decode PipeWire integer event"))
}

fn pod_error(context: &'static str) -> impl FnOnce(pipewire_native_spa::pod::Error) -> io::Error {
    move |error| io::Error::new(io::ErrorKind::InvalidData, format!("{context}: {error:?}"))
}

fn decode_global(payload: &[u8]) -> io::Result<PipeWireGlobal> {
    let mut parser = pipewire_native_spa::pod::parser::Parser::new(payload);
    let global = parser
        .pop_struct(|fields| {
            let id = fields.pop_int()?;
            let permissions = fields.pop_int()?;
            let interface = fields.pop_string()?;
            let version = fields.pop_int()?;
            let properties = fields
                .pop_struct(|pairs| {
                    let count = pairs.pop_int()?;
                    if !(0..=MAX_GLOBAL_PROPERTIES).contains(&count) {
                        return Err(pipewire_native_spa::pod::Error::Invalid(
                            "Invalid PipeWire global property count".into(),
                        ));
                    }
                    let mut properties = Vec::with_capacity(count as usize);
                    for _ in 0..count {
                        properties.push((pairs.pop_string()?, pairs.pop_string()?));
                    }
                    Ok(properties)
                })?
                .0;
            if fields.available() != 0 {
                return Err(pipewire_native_spa::pod::Error::Invalid(
                    "Unexpected trailing PipeWire registry global fields".into(),
                ));
            }
            Ok(PipeWireGlobal {
                id: id as u32,
                permissions: permissions as u32,
                interface,
                version: version as u32,
                properties,
            })
        })
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Decode PipeWire registry global: {error:?}"),
            )
        })?
        .0;
    Ok(global)
}

fn decode_done(payload: &[u8]) -> io::Result<(u32, u32)> {
    let mut parser = pipewire_native_spa::pod::parser::Parser::new(payload);
    parser
        .pop_struct(|fields| Ok((fields.pop_int()? as u32, fields.pop_int()? as u32)))
        .map(|value| value.0)
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Decode PipeWire Core.Done: {error:?}"),
            )
        })
}

fn decode_core_error(payload: &[u8]) -> io::Result<io::Error> {
    let mut parser = pipewire_native_spa::pod::parser::Parser::new(payload);
    let (object_id, seq, result, message) = parser
        .pop_struct(|fields| {
            Ok((
                fields.pop_int()?,
                fields.pop_int()?,
                fields.pop_int()?,
                fields.pop_string()?,
            ))
        })
        .map(|value| value.0)
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Decode PipeWire Core.Error: {error:?}"),
            )
        })?;
    Ok(io::Error::other(format!(
        "PipeWire core error: object={object_id}, seq={seq}, result={result}, message={message}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pipewire_native_spa::pod::parser::Parser;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::thread;

    #[test]
    fn shared_memory_counters_use_atomic_operations() {
        let fd = rustix::fs::memfd_create("pipewire-native-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let file = File::from(fd);
        file.set_len(64).unwrap();
        let memory = PipeWireMemory::new(file.into(), 0, 8).unwrap();

        memory.store_u32(8, 2, Ordering::Release).unwrap();
        assert_eq!(memory.load_u32(8, Ordering::Acquire).unwrap(), 2);
        assert_eq!(memory.decrement_u32(8, Ordering::Release).unwrap(), 1);
        assert_eq!(memory.load_u32(8, Ordering::Acquire).unwrap(), 1);
        assert_eq!(memory.decrement_u32(8, Ordering::Release).unwrap(), 0);
        assert_eq!(
            memory
                .decrement_u32(8, Ordering::Release)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn connecting_sends_core_hello_on_the_portal_remote() {
        let (remote_fd, peer) = UnixStream::pair().unwrap();
        let remote = PipeWireRemote::from_portal(OwnedFd::from(remote_fd), 42);
        let client = PipeWireClient::connect(remote).unwrap();
        let mut peer = NativeTransport::from_owned_fd(OwnedFd::from(peer));
        let hello = peer.receive().unwrap();
        let update_properties = peer.receive().unwrap();

        assert_eq!(client.node_id(), 42);
        assert_eq!(update_properties.header.id, CLIENT_OBJECT_ID);
        assert_eq!(
            update_properties.header.opcode,
            CLIENT_UPDATE_PROPERTIES_OPCODE
        );
        assert_eq!(hello.header.id, CORE_OBJECT_ID);
        assert_eq!(hello.header.opcode, CORE_HELLO_OPCODE);
        assert_eq!(hello.header.seq, 0);
        assert!(hello.fds.is_empty());
        assert_eq!(
            Parser::new(&hello.payload)
                .pop_struct(|structure| {
                    let version = structure.pop_int()?;
                    Ok(version)
                })
                .unwrap()
                .0,
            CORE_PROTOCOL_VERSION
        );
    }

    #[test]
    fn connect_publishes_client_properties() {
        use pipewire_native_spa::pod::parser::Parser;

        let (remote_fd, peer_fd) = UnixStream::pair().unwrap();
        let remote = PipeWireRemote::from_portal(OwnedFd::from(remote_fd), 9);
        let properties = vec![
            ("application.name".to_owned(), "Test App".to_owned()),
            ("application.id".to_owned(), "org.example.Test".to_owned()),
        ];
        let _client = PipeWireClient::connect_with_properties(remote, &properties).unwrap();
        let mut server = NativeTransport::from_owned_fd(OwnedFd::from(peer_fd));
        let _hello = server.receive().unwrap();
        let update = server.receive().unwrap();
        assert_eq!(update.header.id, CLIENT_OBJECT_ID);
        assert_eq!(update.header.opcode, CLIENT_UPDATE_PROPERTIES_OPCODE);
        let mut parser = Parser::new(&update.payload);
        let decoded = parser
            .pop_struct(|fields| {
                fields
                    .pop_struct(|pairs| {
                        let count = pairs.pop_int()?;
                        let mut decoded = Vec::new();
                        for _ in 0..count {
                            decoded.push((pairs.pop_string()?, pairs.pop_string()?));
                        }
                        Ok(decoded)
                    })
                    .map(|value| value.0)
            })
            .unwrap()
            .0;
        assert_eq!(decoded, properties);
    }

    #[test]
    fn bind_global_uses_the_requested_interface_version() {
        use pipewire_native_spa::pod::parser::Parser;

        let (remote_fd, peer_fd) = UnixStream::pair().unwrap();
        let remote = PipeWireRemote::from_portal(OwnedFd::from(remote_fd), 42);
        let mut client = PipeWireClient::connect(remote).unwrap();
        let mut server = NativeTransport::from_owned_fd(OwnedFd::from(peer_fd));
        let _hello = server.receive().unwrap();
        let _properties = server.receive().unwrap();

        let global = PipeWireGlobal {
            id: 42,
            permissions: 0,
            interface: "PipeWire:Interface:Node".to_owned(),
            version: 7,
            properties: vec![
                ("object.serial".to_owned(), "1234".to_owned()),
                ("node.name".to_owned(), "capture.node".to_owned()),
            ],
        };
        assert_eq!(global.target_object(), Some("1234"));
        let object = client.bind_global_version(&global, 3).unwrap();
        assert_eq!(object.version, 3);

        let message = server.receive().unwrap();
        assert_eq!(message.header.id, REGISTRY_OBJECT_ID);
        assert_eq!(message.header.opcode, REGISTRY_BIND_OPCODE);
        let mut parser = Parser::new(&message.payload);
        let fields = parser
            .pop_struct(|fields| {
                Ok((
                    fields.pop_int()?,
                    fields.pop_string()?,
                    fields.pop_int()?,
                    fields.pop_int()?,
                ))
            })
            .unwrap()
            .0;
        assert_eq!(fields, (42, global.interface, 3, object.id.cast_signed()));
    }

    #[test]
    fn registry_enumeration_can_be_cancelled_during_sync() {
        let (remote_fd, peer_fd) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || {
            let mut server = NativeTransport::from_owned_fd(OwnedFd::from(peer_fd));
            let _hello = server.receive().unwrap();
            let _properties = server.receive().unwrap();
            let registry = server.receive().unwrap();
            assert_eq!(registry.header.opcode, CORE_GET_REGISTRY_OPCODE);
            let sync = server.receive().unwrap();
            assert_eq!(sync.header.opcode, CORE_SYNC_OPCODE);
        });

        let remote = PipeWireRemote::from_portal(OwnedFd::from(remote_fd), 9);
        let mut client = PipeWireClient::connect(remote).unwrap();
        let error = client
            .list_globals_with_cancellation(Duration::from_secs(1), || true)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        server.join().unwrap();
    }

    #[test]
    fn registry_enumeration_waits_for_sync_barrier() {
        let (remote_fd, peer) = UnixStream::pair().unwrap();
        let remote = PipeWireRemote::from_portal(OwnedFd::from(remote_fd), 73);
        let server = thread::spawn(move || {
            let mut server = NativeTransport::from_owned_fd(OwnedFd::from(peer));
            let hello = server.receive().unwrap();
            assert_eq!(hello.header.opcode, CORE_HELLO_OPCODE);
            let update_properties = server.receive().unwrap();
            assert_eq!(update_properties.header.id, CLIENT_OBJECT_ID);
            assert_eq!(
                update_properties.header.opcode,
                CLIENT_UPDATE_PROPERTIES_OPCODE
            );

            let get_registry = server.receive().unwrap();
            assert_eq!(get_registry.header.id, CORE_OBJECT_ID);
            assert_eq!(get_registry.header.opcode, CORE_GET_REGISTRY_OPCODE);
            assert_eq!(
                Parser::new(&get_registry.payload)
                    .pop_struct(|fields| {
                        let version = fields.pop_int()?;
                        let id = fields.pop_int()?;
                        Ok((version, id))
                    })
                    .unwrap()
                    .0,
                (REGISTRY_VERSION, REGISTRY_OBJECT_ID.cast_signed())
            );

            let sync = server.receive().unwrap();
            assert_eq!(sync.header.opcode, CORE_SYNC_OPCODE);
            let sync_seq = Parser::new(&sync.payload)
                .pop_struct(|fields| {
                    let id = fields.pop_int()?;
                    let seq = fields.pop_int()?;
                    Ok((id, seq as u32))
                })
                .unwrap()
                .0;
            assert_eq!(sync_seq.0, CORE_OBJECT_ID.cast_signed());

            send_registry_global(&mut server, 73, "obsolete-node");
            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: REGISTRY_OBJECT_ID,
                        opcode: REGISTRY_GLOBAL_REMOVE_EVENT_OPCODE,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: encode_ints(&[73]).unwrap(),
                    fds: Vec::new(),
                })
                .unwrap();
            send_registry_global(&mut server, 74, "portal-screen");

            server
                .send(&NativeMessage {
                    header: MessageHeader {
                        id: CORE_OBJECT_ID,
                        opcode: 1,
                        size: 0,
                        seq: 0,
                        n_fds: 0,
                    },
                    payload: encode_ints(&[CORE_OBJECT_ID.cast_signed(), sync_seq.1.cast_signed()])
                        .unwrap(),
                    fds: Vec::new(),
                })
                .unwrap();
        });

        let mut client = PipeWireClient::connect(remote).unwrap();
        let globals = client.list_globals().unwrap();
        server.join().unwrap();

        assert_eq!(globals.len(), 1);
        assert_eq!(globals[0].id, 74);
        assert_eq!(globals[0].interface, "PipeWire:Interface:Node");
        assert_eq!(
            globals[0].properties,
            vec![("node.name".into(), "portal-screen".into())]
        );
    }

    fn send_registry_global(server: &mut NativeTransport, id: i32, name: &str) {
        let mut buffer = [0_u8; 512];
        let payload = Builder::new(&mut buffer)
            .push_struct(|fields| {
                fields
                    .push_int(id)
                    .push_int(0x7)
                    .push_string("PipeWire:Interface:Node")
                    .push_int(3)
                    .push_struct(|pairs| {
                        pairs.push_int(1).push_string("node.name").push_string(name)
                    })
            })
            .build()
            .unwrap()
            .to_vec();
        server
            .send(&NativeMessage {
                header: MessageHeader {
                    id: REGISTRY_OBJECT_ID,
                    opcode: 0,
                    size: 0,
                    seq: 0,
                    n_fds: 0,
                },
                payload,
                fds: Vec::new(),
            })
            .unwrap();
    }

    #[test]
    fn poll_waits_for_protocol_socket_and_transport_eventfds() {
        use rustix::event::{eventfd, EventfdFlags, PollFlags};
        use std::os::fd::AsFd;
        use std::time::Duration;

        let (remote_fd, peer_fd) = UnixStream::pair().unwrap();
        let remote = PipeWireRemote::from_portal(OwnedFd::from(remote_fd), 9);
        let client = PipeWireClient::connect(remote).unwrap();
        let mut peer = NativeTransport::from_owned_fd(OwnedFd::from(peer_fd));
        let _hello = peer.receive().unwrap();
        let _properties = peer.receive().unwrap();
        let signal = eventfd(0, EventfdFlags::CLOEXEC).unwrap();

        let idle = client
            .poll_readable(&[signal.as_fd()], Some(Duration::from_millis(1)))
            .unwrap();
        assert!(!idle[0].contains(PollFlags::IN));
        assert!(!idle[1].contains(PollFlags::IN));

        crate::transport::write_eventfd(signal.as_fd(), 1).unwrap();
        let ready = client
            .poll_readable(&[signal.as_fd()], Some(Duration::from_millis(10)))
            .unwrap();
        assert!(!ready[0].contains(PollFlags::IN));
        assert!(ready[1].contains(PollFlags::IN));
        assert_eq!(crate::transport::read_eventfd(signal.as_fd()).unwrap(), 1);
    }

    #[test]
    fn triggers_peer_activation_after_the_last_dependency() {
        use rustix::event::{eventfd, EventfdFlags};
        use rustix::fs::{memfd_create, MemfdFlags};
        use std::fs::File;
        use std::os::fd::AsFd;
        use std::os::unix::fs::FileExt;

        let (remote_fd, peer_fd) = UnixStream::pair().unwrap();
        let remote = PipeWireRemote::from_portal(OwnedFd::from(remote_fd), 9);
        let mut client = PipeWireClient::connect(remote).unwrap();
        let mut server = NativeTransport::from_owned_fd(OwnedFd::from(peer_fd));
        let _hello = server.receive().unwrap();
        let _properties = server.receive().unwrap();

        let fd = memfd_create("pw-activation", MemfdFlags::CLOEXEC).unwrap();
        let file = File::from(fd);
        file.set_len(4096).unwrap();
        let mut activation = [0_u8; 40];
        activation[4..8].copy_from_slice(&1_u32.to_ne_bytes());
        activation[16..20].copy_from_slice(&2_u32.to_ne_bytes());
        file.write_at(&activation, 0).unwrap();
        client
            .memory
            .insert(99, PipeWireMemory::new(file.into(), 2, 0).unwrap());
        let signal = eventfd(0, EventfdFlags::CLOEXEC).unwrap();

        assert!(!client
            .trigger_activation(99, 0, 40, signal.as_fd())
            .unwrap());
        assert_eq!(
            client
                .memory(99)
                .unwrap()
                .load_u32(16, Ordering::Acquire)
                .unwrap(),
            1
        );
        assert!(client
            .trigger_activation(99, 0, 40, signal.as_fd())
            .unwrap());
        let memory = client.memory(99).unwrap();
        assert_eq!(memory.load_u32(0, Ordering::Acquire).unwrap(), 1);
        assert_eq!(memory.load_u32(16, Ordering::Acquire).unwrap(), 0);
        let signal_time = u64::from_ne_bytes(memory.read_at(32, 8).unwrap().try_into().unwrap());
        assert!(signal_time > 0);
        assert_eq!(crate::transport::read_eventfd(signal.as_fd()).unwrap(), 1);
    }

    #[test]
    fn hello_payload_is_a_single_version_integer() {
        let payload = encode_ints(&[CORE_PROTOCOL_VERSION]).unwrap();
        let mut parser = Parser::new(&payload);
        let version = parser
            .pop_struct(|structure| {
                let version = structure.pop_int()?;
                if structure.available() != 0 {
                    return Err(pipewire_native_spa::pod::Error::Invalid(
                        "Unexpected trailing Core.Hello fields".into(),
                    ));
                }
                Ok(version)
            })
            .unwrap()
            .0;

        assert_eq!(version, CORE_PROTOCOL_VERSION);
        assert_eq!(parser.available(), 0);
    }
}
