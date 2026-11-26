//! Safe transport primitives for the `PipeWire` native protocol.
//!
//! The transport preserves the complete protocol frame, including ancillary
//! file descriptors, across nonblocking reads and cancellation timeouts.

use std::collections::VecDeque;
use std::io::{self, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::net::{
    recvmsg, sendmsg, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags,
};

const HEADER_SIZE: usize = 16;
const MAX_PAYLOAD_SIZE: usize = crate::build_info::MAX_MESSAGE_SIZE;
const MAX_FDS: usize = 64;
const RECEIVE_CHUNK_SIZE: usize = 32 * 1024;
const CONTROL_SPACE: usize = rustix::cmsg_space!(ScmRights(MAX_FDS));

fn read_u32(bytes: &[u8; HEADER_SIZE], offset: usize) -> u32 {
    u32::from_ne_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageHeader {
    pub id: u32,
    pub opcode: u8,
    pub size: u32,
    pub seq: u32,
    pub n_fds: u32,
}

impl MessageHeader {
    pub fn encode(self) -> io::Result<[u8; HEADER_SIZE]> {
        if self.size as usize > MAX_PAYLOAD_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PipeWire message payload exceeds the 24-bit wire limit",
            ));
        }
        if self.n_fds as usize > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PipeWire message carries too many file descriptors",
            ));
        }

        let opcode_size = (u32::from(self.opcode) << 24) | self.size;
        let mut bytes = [0; HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.id.to_ne_bytes());
        bytes[4..8].copy_from_slice(&opcode_size.to_ne_bytes());
        bytes[8..12].copy_from_slice(&self.seq.to_ne_bytes());
        bytes[12..16].copy_from_slice(&self.n_fds.to_ne_bytes());
        Ok(bytes)
    }

    pub fn decode(bytes: [u8; HEADER_SIZE]) -> io::Result<Self> {
        let id = read_u32(&bytes, 0);
        let opcode_size = read_u32(&bytes, 4);
        let seq = read_u32(&bytes, 8);
        let n_fds = read_u32(&bytes, 12);
        let size = opcode_size & 0x00ff_ffff;

        if n_fds as usize > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "PipeWire message declares too many file descriptors",
            ));
        }

        Ok(Self {
            id,
            opcode: (opcode_size >> 24) as u8,
            size,
            seq,
            n_fds,
        })
    }
}

#[derive(Debug)]
pub struct NativeMessage {
    pub header: MessageHeader,
    /// Complete payload, including an optional protocol footer POD.
    pub payload: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

pub struct NativeTransport {
    stream: UnixStream,
    incoming_bytes: Vec<u8>,
    receive_buffer: Vec<u8>,
    received_fds: VecDeque<OwnedFd>,
}

impl AsFd for NativeTransport {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }
}

impl NativeTransport {
    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self::from_unix_stream(UnixStream::from(fd))
    }

    fn from_unix_stream(stream: UnixStream) -> Self {
        Self {
            stream,
            incoming_bytes: Vec::new(),
            receive_buffer: vec![0_u8; RECEIVE_CHUNK_SIZE],
            received_fds: VecDeque::new(),
        }
    }

    pub fn into_owned_fd(self) -> OwnedFd {
        self.stream.into()
    }

    pub fn send(&mut self, message: &NativeMessage) -> io::Result<()> {
        if message.payload.len() > MAX_PAYLOAD_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PipeWire message payload exceeds the 24-bit wire limit",
            ));
        }
        if message.fds.len() > MAX_FDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PipeWire message carries too many file descriptors",
            ));
        }

        let mut header = message.header;
        header.size = u32::try_from(message.payload.len()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "PipeWire payload size overflow",
            )
        })?;
        header.n_fds = u32::try_from(message.fds.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "PipeWire FD count overflow")
        })?;

        let mut packet = Vec::with_capacity(HEADER_SIZE + message.payload.len());
        packet.extend_from_slice(&header.encode()?);
        packet.extend_from_slice(&message.payload);

        let sent = if message.fds.is_empty() {
            self.stream.write(&packet)?
        } else {
            let borrowed_fds: Vec<BorrowedFd<'_>> = message.fds.iter().map(AsFd::as_fd).collect();
            let mut control_space = [MaybeUninit::uninit(); CONTROL_SPACE];
            let mut control = SendAncillaryBuffer::new(&mut control_space);
            if !control.push(SendAncillaryMessage::ScmRights(&borrowed_fds)) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Could not encode PipeWire SCM_RIGHTS control message",
                ));
            }
            sendmsg(
                &self.stream,
                &[IoSlice::new(&packet)],
                &mut control,
                SendFlags::empty(),
            )
            .map_err(errno_to_io)?
        };

        if sent == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "PipeWire socket accepted zero bytes",
            ));
        }
        self.stream.write_all(&packet[sent..])
    }

    /// Receive one complete message without blocking.
    ///
    /// `PipeWire` may flush several protocol messages in one `sendmsg` call and
    /// attach all of their `SCM_RIGHTS` descriptors to the first byte of that
    /// batch. Keep bytes and descriptors as independent ordered streams, then
    /// consume exactly the descriptor count declared by each message header.
    pub fn try_receive(&mut self) -> io::Result<Option<NativeMessage>> {
        const MAX_QUEUED_FDS: usize = 1024;

        loop {
            if let Some(message) = self.take_message()? {
                return Ok(Some(message));
            }

            // Reuse one receive buffer instead of allocating 32 KiB per read.
            let mut control_space = [MaybeUninit::uninit(); CONTROL_SPACE];
            let mut control = RecvAncillaryBuffer::new(&mut control_space);
            let mut iov = [IoSliceMut::new(&mut self.receive_buffer)];
            let result = match recvmsg(
                &self.stream,
                &mut iov,
                &mut control,
                RecvFlags::CMSG_CLOEXEC | RecvFlags::DONTWAIT,
            ) {
                Ok(result) => result,
                Err(error) if error == rustix::io::Errno::AGAIN => return Ok(None),
                Err(error) => return Err(errno_to_io(error)),
            };

            for ancillary in control.drain() {
                if let RecvAncillaryMessage::ScmRights(fds) = ancillary {
                    self.received_fds.extend(fds);
                }
            }
            if result.flags.contains(ReturnFlags::CTRUNC) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "PipeWire ancillary data exceeded the receive buffer",
                ));
            }
            if self.received_fds.len() > MAX_QUEUED_FDS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "PipeWire connection queued too many file descriptors",
                ));
            }
            if result.bytes == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "PipeWire socket closed during a protocol message",
                ));
            }

            self.incoming_bytes
                .extend_from_slice(&self.receive_buffer[..result.bytes]);
        }
    }

    fn take_message(&mut self) -> io::Result<Option<NativeMessage>> {
        if self.incoming_bytes.len() < HEADER_SIZE {
            return Ok(None);
        }

        let header_bytes: [u8; HEADER_SIZE] = self.incoming_bytes[..HEADER_SIZE]
            .try_into()
            .map_err(|_| io::Error::other("PipeWire message header length changed"))?;
        let header = MessageHeader::decode(header_bytes)?;
        let frame_size = HEADER_SIZE
            .checked_add(header.size as usize)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "PipeWire frame size overflow")
            })?;

        if self.incoming_bytes.len() < frame_size || self.received_fds.len() < header.n_fds as usize
        {
            return Ok(None);
        }

        let payload = self.incoming_bytes[HEADER_SIZE..frame_size].to_vec();
        self.incoming_bytes.drain(..frame_size);
        let fds = self.received_fds.drain(..header.n_fds as usize).collect();

        Ok(Some(NativeMessage {
            header,
            payload,
            fds,
        }))
    }

    /// Wait up to timeout for a complete message. Partial reads are preserved.
    pub fn receive_timeout(&mut self, timeout: Duration) -> io::Result<Option<NativeMessage>> {
        if let Some(message) = self.try_receive()? {
            return Ok(Some(message));
        }

        let mut descriptors = [PollFd::new(&self.stream, PollFlags::IN)];
        let timeout = Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        rustix::event::poll(&mut descriptors, Some(&timeout)).map_err(errno_to_io)?;
        self.try_receive()
    }

    pub fn receive(&mut self) -> io::Result<NativeMessage> {
        loop {
            if let Some(message) = self.try_receive()? {
                return Ok(message);
            }
            let mut descriptors = [PollFd::new(&self.stream, PollFlags::IN)];
            rustix::event::poll(&mut descriptors, None).map_err(errno_to_io)?;
        }
    }
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

/// Read and clear an eventfd counter using safe descriptor I/O.
pub fn read_eventfd(fd: BorrowedFd<'_>) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    let count = rustix::io::read(fd, &mut bytes)?;
    if count != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Short read from PipeWire eventfd",
        ));
    }
    Ok(u64::from_ne_bytes(bytes))
}

/// Signal an eventfd using safe descriptor I/O.
pub fn write_eventfd(fd: BorrowedFd<'_>, value: u64) -> io::Result<()> {
    let bytes = value.to_ne_bytes();
    let count = rustix::io::write(fd, &bytes)?;
    if count != bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "Short write to PipeWire eventfd",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsFd, AsRawFd};

    #[test]
    fn message_header_round_trips_wire_fields() {
        let header = MessageHeader {
            id: 0x1020_3040,
            opcode: 0x12,
            size: 0x00ab_cdef,
            seq: 0x5060_7080,
            n_fds: 3,
        };
        assert_eq!(
            MessageHeader::decode(header.encode().unwrap()).unwrap(),
            header
        );
    }

    #[test]
    fn rejects_payload_larger_than_wire_limit() {
        let header = MessageHeader {
            id: 0,
            opcode: 1,
            size: (MAX_PAYLOAD_SIZE + 1) as u32,
            seq: 0,
            n_fds: 0,
        };
        assert_eq!(
            header.encode().unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn transfers_file_descriptors_without_application_unsafe() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let (passed_fd, _peer) = UnixStream::pair().unwrap();
        let passed_fd: OwnedFd = passed_fd.into();
        let mut sender = NativeTransport::from_unix_stream(sender);
        let mut receiver = NativeTransport::from_unix_stream(receiver);
        let message = NativeMessage {
            header: MessageHeader {
                id: 0,
                opcode: 1,
                size: 0,
                seq: 7,
                n_fds: 0,
            },
            payload: b"pipewire".to_vec(),
            fds: vec![passed_fd],
        };

        sender.send(&message).unwrap();
        let received = receiver.receive().unwrap();
        assert_eq!(received.payload, b"pipewire");
        assert_eq!(received.header.seq, 7);
        assert_eq!(received.fds.len(), 1);
        assert!(received.fds[0].as_fd().as_raw_fd() >= 0);
    }

    #[test]
    fn assigns_batched_file_descriptors_to_their_protocol_messages() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let mut receiver = NativeTransport::from_unix_stream(receiver);
        let mut passed_fds = Vec::new();
        let mut peers = Vec::new();
        for _ in 0..4 {
            let (fd, peer) = UnixStream::pair().unwrap();
            passed_fds.push(OwnedFd::from(fd));
            peers.push(peer);
        }
        let borrowed_fds = passed_fds.iter().map(AsFd::as_fd).collect::<Vec<_>>();

        let first_header = MessageHeader {
            id: 10,
            opcode: 1,
            size: 5,
            seq: 0,
            n_fds: 1,
        }
        .encode()
        .unwrap();
        let second_header = MessageHeader {
            id: 11,
            opcode: 2,
            size: 6,
            seq: 1,
            n_fds: 3,
        }
        .encode()
        .unwrap();
        let mut packet = Vec::new();
        packet.extend_from_slice(&first_header);
        packet.extend_from_slice(b"first");
        packet.extend_from_slice(&second_header);
        packet.extend_from_slice(b"second");

        let mut control_space = [MaybeUninit::uninit(); CONTROL_SPACE];
        let mut control = SendAncillaryBuffer::new(&mut control_space);
        assert!(control.push(SendAncillaryMessage::ScmRights(&borrowed_fds)));
        let sent = sendmsg(
            &sender,
            &[IoSlice::new(&packet)],
            &mut control,
            SendFlags::empty(),
        )
        .unwrap();
        assert_eq!(sent, packet.len());

        let first = receiver.receive().unwrap();
        assert_eq!(first.header.id, 10);
        assert_eq!(first.payload, b"first");
        assert_eq!(first.fds.len(), 1);

        let second = receiver.receive().unwrap();
        assert_eq!(second.header.id, 11);
        assert_eq!(second.payload, b"second");
        assert_eq!(second.fds.len(), 3);
    }

    #[test]
    fn timeout_preserves_a_partial_frame() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let mut receiver = NativeTransport::from_unix_stream(receiver);
        let header = MessageHeader {
            id: 9,
            opcode: 3,
            size: 4,
            seq: 11,
            n_fds: 0,
        }
        .encode()
        .unwrap();

        (&sender).write_all(&header[..8]).unwrap();
        assert!(receiver
            .receive_timeout(Duration::from_millis(5))
            .unwrap()
            .is_none());
        (&sender).write_all(&header[8..]).unwrap();
        (&sender).write_all(b"data").unwrap();

        let message = receiver
            .receive_timeout(Duration::from_millis(50))
            .unwrap()
            .unwrap();
        assert_eq!(message.header.id, 9);
        assert_eq!(message.header.opcode, 3);
        assert_eq!(message.payload, b"data");
    }
}
