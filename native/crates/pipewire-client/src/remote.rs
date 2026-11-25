//! Owned `PipeWire` remote endpoints supplied by desktop portals.

use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// `PipeWire`'s `PW_ID_ANY` target, allowing the session manager to route by properties.
pub const PIPEWIRE_ID_ANY: u32 = u32::MAX;

/// A `PipeWire` remote connection and the portal-selected node to capture.
///
/// The endpoint owns the remote file descriptor. Dropping it closes the FD;
/// transferring it into a transport preserves single-owner semantics.
#[derive(Debug)]
pub struct PipeWireRemote {
    fd: OwnedFd,
    node_id: u32,
}

impl PipeWireRemote {
    /// Connect to a local `PipeWire` native socket and target one registry node.
    pub fn connect_socket(path: impl AsRef<Path>, node_id: u32) -> io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        Ok(Self::from_fd(stream.into(), node_id))
    }

    /// Connect to the user's default `PipeWire` socket and target one registry node.
    pub fn connect_default(node_id: u32) -> io::Result<Self> {
        let runtime_dir = std::env::var_os("PIPEWIRE_RUNTIME_DIR")
            .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "PipeWire runtime directory is not configured",
                )
            })?;
        let remote = std::env::var_os("PIPEWIRE_REMOTE").unwrap_or_else(|| "pipewire-0".into());
        let remote_path = PathBuf::from(&remote);
        let socket = if remote_path.is_absolute() {
            remote_path
        } else {
            PathBuf::from(runtime_dir).join(remote_path)
        };
        Self::connect_socket(socket, node_id)
    }

    /// Construct an endpoint from a connected native-protocol FD and node ID.
    #[must_use]
    pub const fn from_fd(fd: OwnedFd, node_id: u32) -> Self {
        Self { fd, node_id }
    }

    /// Construct an endpoint from the FD and node ID returned by a portal.
    #[must_use]
    pub const fn from_portal(fd: OwnedFd, node_id: u32) -> Self {
        Self::from_fd(fd, node_id)
    }

    /// Return the portal-selected `PipeWire` node ID.
    #[must_use]
    pub const fn node_id(&self) -> u32 {
        self.node_id
    }

    /// Consume the endpoint and transfer ownership of its remote FD.
    #[must_use]
    pub fn into_parts(self) -> (OwnedFd, u32) {
        (self.fd, self.node_id)
    }
}
