//! Owned `PipeWire` remote endpoints supplied by desktop portals.

use std::os::fd::OwnedFd;

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
    /// Construct an endpoint from the FD and node ID returned by a portal.
    #[must_use]
    pub const fn from_portal(fd: OwnedFd, node_id: u32) -> Self {
        Self { fd, node_id }
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
