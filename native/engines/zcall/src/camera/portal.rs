//! XDG Camera portal permission and remote setup.

use super::CameraCapture;
use ashpd::desktop::camera::{CameraAccessOptions, OpenPipeWireRemoteOptions};
use pipewire_client::remote::{PipeWireRemote, PIPEWIRE_ID_ANY};

impl CameraCapture {
    /// Request camera permission through XDG Camera portal and capture its node.
    pub fn start_pipewire_portal(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("Create camera portal runtime: {error}"))?;
        let remote_fd = runtime.block_on(async {
            let camera = ashpd::desktop::camera::Camera::new()
                .await
                .map_err(|error| format!("Connect to XDG Camera portal: {error}"))?;
            let request = camera
                .request_access(CameraAccessOptions::default())
                .await
                .map_err(|error| format!("Request camera access: {error}"))?;
            request
                .response()
                .map_err(|error| format!("Camera access denied: {error}"))?;
            if !camera
                .is_present()
                .await
                .map_err(|error| format!("Check camera presence: {error}"))?
            {
                return Err("XDG Camera portal reports no camera present".to_owned());
            }
            let fd = camera
                .open_pipe_wire_remote(OpenPipeWireRemoteOptions::default())
                .await
                .map_err(|error| format!("Open portal PipeWire camera remote: {error}"))?;
            Ok::<_, String>(fd)
        })?;
        let remote = PipeWireRemote::from_fd(remote_fd, PIPEWIRE_ID_ANY);
        Self::start_pipewire_remote(remote, None, width, height, fps, bitrate)
    }
}
