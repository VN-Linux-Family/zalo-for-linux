//! Native `PipeWire` camera capture backend.

use super::{
    bgra_to_yuyv, encode_camera_frame, uyvy_to_yuyv, CameraCapture, CameraFrameContext,
    EncodedCameraFrame,
};
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Profile, UsageType,
};
use pipewire_client::{
    client::PipeWireClient,
    format::{VideoFormat, VideoFormatCaps, VideoSizeRange},
    remote::{PipeWireRemote, PIPEWIRE_ID_ANY},
    PipeWireStream, StreamOptions,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

impl CameraCapture {
    /// Capture a camera node directly from the user's native `PipeWire` socket.
    pub fn start_pipewire_direct(
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self, String> {
        Self::start_pipewire_direct_selected(None, width, height, fps, bitrate)
    }

    /// Capture the exact V4L2 camera selected by the user through `PipeWire`.
    pub(super) fn start_pipewire_direct_selected(
        device_path: Option<&str>,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self, String> {
        let remote = PipeWireRemote::connect_default(0)
            .map_err(|error| format!("Connect to PipeWire camera remote: {error}"))?;
        let mut client = PipeWireClient::connect(remote)
            .map_err(|error| format!("Initialize PipeWire camera registry: {error}"))?;
        let globals = client
            .list_globals()
            .map_err(|error| format!("Enumerate PipeWire camera nodes: {error}"))?;
        let selected = globals
            .iter()
            .find(|global| {
                global.interface == "PipeWire:Interface:Node"
                    && global.property("media.role") == Some("Camera")
                    && device_path.is_none_or(|path| camera_global_matches_device(global, path))
            })
            .ok_or_else(|| match device_path {
                Some(path) => {
                    format!("PipeWire has no camera node matching selected device '{path}'")
                }
                None => "PipeWire has no camera-role node".to_owned(),
            })?;
        let node_id = selected.id;
        let target_object = selected
            .target_object()
            .map_or_else(|| node_id.to_string(), str::to_owned);
        drop(client);
        let remote = PipeWireRemote::connect_default(PIPEWIRE_ID_ANY)
            .map_err(|error| format!("Connect PipeWire camera node: {error}"))?;
        Self::start_pipewire_remote(remote, Some(target_object), width, height, fps, bitrate)
    }

    pub(super) fn start_pipewire_remote(
        remote: PipeWireRemote,
        target_object: Option<String>,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self, String> {
        let width = width.clamp(160, 1920) & !1;
        let height = height.clamp(120, 1080) & !1;
        let caps = VideoFormatCaps {
            default: VideoFormat::Bgra,
            alternatives: vec![
                VideoFormat::Bgra,
                VideoFormat::Bgrx,
                VideoFormat::Yuy2,
                VideoFormat::Uyvy,
            ],
            size: VideoSizeRange {
                default: (width, height),
                min: (160, 120),
                max: (1920, 1080),
            },
        };
        let mut node_properties = vec![
            ("media.type".into(), "Video".into()),
            ("media.category".into(), "Capture".into()),
            ("media.role".into(), "Camera".into()),
            ("media.class".into(), "Stream/Input/Video".into()),
        ];
        if let Some(target_object) = target_object {
            node_properties.push(("target.object".into(), target_object));
        } else {
            // The portal remote is already restricted to the granted camera.
            // Let the camera-role policy connect this stream; there is no
            // camera source node exposed in the portal client's registry.
            node_properties.push(("node.autoconnect".into(), "true".into()));
        }
        let options = StreamOptions {
            name: "ZaloCall Camera".into(),
            node_properties,
            client_properties: vec![("application.name".into(), "ZaloCall".into())],
            auto_link: false,
            target_node_id: None,
        };
        let stream = PipeWireStream::connect_with_options(remote, &caps, &options)
            .map_err(|error| format!("Open PipeWire camera stream: {error}"))?;
        let context = CameraFrameContext {
            config: EncoderConfig::new()
                .bitrate(BitRate::from_bps(bitrate.clamp(80_000, 2_000_000)))
                .max_frame_rate(FrameRate::from_hz(f32::from(
                    u8::try_from(fps.clamp(5, 30)).unwrap_or(30),
                )))
                .usage_type(UsageType::CameraVideoRealTime)
                .profile(Profile::Baseline)
                .intra_frame_period(IntraFramePeriod::from_num_frames(fps.clamp(5, 30) * 2)),
            actual_width: width as usize,
            actual_height: height as usize,
            stride: width as usize * 2,
            frame_width: width,
            frame_height: height,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let force_keyframe = Arc::new(AtomicBool::new(false));
        let worker_force_keyframe = force_keyframe.clone();
        let (tx, frames) = sync_channel(2);
        let worker = spawn_pipewire_camera_worker(
            stream,
            context,
            worker_stop,
            worker_force_keyframe,
            tx,
            width,
            height,
        )?;
        Ok(Self {
            stop,
            force_keyframe,
            frames,
            worker: Mutex::new(Some(worker)),
        })
    }
}

fn spawn_pipewire_camera_worker(
    mut stream: PipeWireStream,
    context: CameraFrameContext,
    worker_stop: Arc<AtomicBool>,
    worker_force_keyframe: Arc<AtomicBool>,
    tx: SyncSender<EncodedCameraFrame>,
    width: u32,
    height: u32,
) -> Result<JoinHandle<()>, String> {
    thread::Builder::new()
        .name("zcall-pipewire-camera".into())
        .spawn(move || {
            let mut encoder: Option<Encoder> = None;
            while !worker_stop.load(Ordering::Acquire) {
                match stream.next_frame(Duration::from_millis(50)) {
                    Ok(Some(frame)) => {
                        let frame_width = frame.width as usize;
                        let frame_height = frame.height as usize;
                        let frame_stride = frame.stride as usize;
                        let yuyv = match frame.format.pixel_format {
                            VideoFormat::Yuy2 => Some(frame.data),
                            VideoFormat::Uyvy => {
                                uyvy_to_yuyv(&frame.data, frame_width, frame_height, frame_stride)
                            }
                            VideoFormat::Bgra | VideoFormat::Bgrx => bgra_to_yuyv(
                                &frame.into_bgra(),
                                frame_width,
                                frame_height,
                                frame_stride,
                            ),
                        };
                        let Some(yuyv) = yuyv else {
                            crate::zcall_log!(
                                "[zcall-camera] Dropped malformed PipeWire camera frame"
                            );
                            continue;
                        };
                        let frame_context = CameraFrameContext {
                            actual_width: frame_width,
                            actual_height: frame_height,
                            stride: frame_width * 2,
                            frame_width: u32::try_from(frame_width).unwrap_or(width),
                            frame_height: u32::try_from(frame_height).unwrap_or(height),
                            ..context
                        };
                        if !encode_camera_frame(
                            &yuyv,
                            &mut encoder,
                            &worker_force_keyframe,
                            &frame_context,
                            &tx,
                        ) {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if !worker_stop.load(Ordering::Acquire) {
                            crate::zcall_log!("[zcall-camera] PipeWire stream failed: {error}");
                        }
                        break;
                    }
                }
            }
        })
        .map_err(|error| format!("Start PipeWire camera worker: {error}"))
}

fn camera_global_matches_device(
    global: &pipewire_client::PipeWireGlobal,
    device_path: &str,
) -> bool {
    ["api.v4l2.path", "object.path", "device.name", "node.name"]
        .into_iter()
        .filter_map(|key| global.property(key))
        .any(|value| value == device_path || value == format!("v4l2:{device_path}"))
}

#[cfg(test)]
mod selection_tests {
    use super::camera_global_matches_device;
    use pipewire_client::PipeWireGlobal;

    #[test]
    fn selected_v4l2_path_matches_pipewire_camera_node() {
        let global = PipeWireGlobal {
            id: 86,
            permissions: 0,
            interface: "PipeWire:Interface:Node".into(),
            version: 3,
            properties: vec![
                ("media.role".into(), "Camera".into()),
                ("api.v4l2.path".into(), "/dev/video0".into()),
                ("object.path".into(), "v4l2:/dev/video0".into()),
            ],
        };
        assert!(camera_global_matches_device(&global, "/dev/video0"));
        assert!(!camera_global_matches_device(&global, "/dev/video2"));
    }
}
