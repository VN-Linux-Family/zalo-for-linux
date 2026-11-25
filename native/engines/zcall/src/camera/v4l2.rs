//! V4L2 device probing and capture-format negotiation.

use super::{encode_camera_frame, CameraCapture, CameraFrameContext};
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Profile, UsageType,
};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};
use std::thread;
#[cfg(target_os = "linux")]
use v4l::buffer::Type;
#[cfg(target_os = "linux")]
use v4l::device::Device;
#[cfg(target_os = "linux")]
use v4l::format::FourCC;
#[cfg(target_os = "linux")]
use v4l::io::mmap::Stream as MmapStream;
#[cfg(target_os = "linux")]
use v4l::io::traits::CaptureStream;
#[cfg(target_os = "linux")]
use v4l::video::Capture;

#[cfg(target_os = "linux")]
pub(super) fn open_camera_device(
    device_path: Option<&str>,
    width: u32,
    height: u32,
) -> Result<(Device, String, usize, usize, usize), String> {
    // Inventory only advertises nodes with V4L2 capture + streaming capability
    // and YUYV support. Probe the saved choice first, then try the other verified
    // nodes; a metadata node or a capture node that rejects our format is not a
    // usable camera for this pipeline.
    let available = camera_device_paths();
    let mut candidates = Vec::with_capacity(available.len());
    if let Some(preferred) = device_path.filter(|path| !path.is_empty()) {
        if available.iter().any(|(path, _)| path == preferred) {
            candidates.push(preferred.to_owned());
        }
    }
    for (path, _) in available {
        if !candidates.iter().any(|candidate| candidate == &path) {
            candidates.push(path);
        }
    }

    let mut errors = Vec::new();
    for path in candidates {
        match try_open_camera_device(&path, width, height) {
            Ok((device, actual_width, actual_height, stride)) => {
                return Ok((device, path, actual_width, actual_height, stride));
            }
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Err("No usable V4L2 camera found (requires capture, streaming, and YUYV)".into())
    } else {
        Err(format!("No usable V4L2 camera: {}", errors.join("; ")))
    }
}

#[cfg(target_os = "linux")]
fn try_open_camera_device(
    path: &str,
    width: u32,
    height: u32,
) -> Result<(Device, usize, usize, usize), String> {
    let device = Device::with_path(path).map_err(|error| format!("Open {path}: {error}"))?;
    let mut format = device
        .format()
        .map_err(|error| format!("Read camera format {path}: {error}"))?;
    format.width = width.clamp(160, 1920) & !1;
    format.height = height.clamp(120, 1080) & !1;
    format.fourcc = FourCC::new(b"YUYV");
    let format = device
        .set_format(&format)
        .map_err(|error| format!("Configure camera {path}: {error}"))?;

    if format.fourcc != FourCC::new(b"YUYV") {
        return Err(format!(
            "Camera {path} did not accept YUYV (returned {:?})",
            format.fourcc
        ));
    }
    let actual_width = format.width as usize;
    let actual_height = format.height as usize;
    if actual_width < 2
        || actual_height < 2
        || !actual_width.is_multiple_of(2)
        || !actual_height.is_multiple_of(2)
        || (format.stride as usize) < actual_width * 2
    {
        return Err(format!(
            "Invalid YUYV camera format {}x{} stride {}",
            actual_width, actual_height, format.stride
        ));
    }

    Ok((device, actual_width, actual_height, format.stride as usize))
}

pub(super) fn start_v4l2(
    device_path: Option<&str>,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
) -> Result<CameraCapture, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (device_path, width, height, fps, bitrate);
        return Err("V4L2 camera capture is only available on Linux".into());
    }

    #[cfg(target_os = "linux")]
    {
        let (device, path, actual_width, actual_height, stride) =
            open_camera_device(device_path, width, height)?;

        let frame_context = CameraFrameContext {
            config: EncoderConfig::new()
                .bitrate(BitRate::from_bps(bitrate.clamp(80_000, 2_000_000)))
                .max_frame_rate(FrameRate::from_hz(f32::from(
                    u8::try_from(fps.clamp(5, 30)).unwrap_or(30),
                )))
                .usage_type(UsageType::CameraVideoRealTime)
                .profile(Profile::Baseline)
                .intra_frame_period(IntraFramePeriod::from_num_frames(fps.clamp(5, 30) * 2)),
            actual_width,
            actual_height,
            stride,
            frame_width: actual_width as u32,
            frame_height: actual_height as u32,
        };
        let mut stream = MmapStream::with_buffers(&device, Type::VideoCapture, 4)
            .map_err(|error| format!("Create V4L2 stream {path}: {error}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let force_keyframe = Arc::new(AtomicBool::new(false));
        let worker_force_keyframe = force_keyframe.clone();
        let (tx, frames) = sync_channel(2);
        let worker = thread::Builder::new()
            .name("zcall-camera".into())
            .spawn(move || {
                let mut encoder: Option<Encoder> = None;

                while !worker_stop.load(Ordering::Relaxed) {
                    match stream.next() {
                        Ok((data, _metadata)) => {
                            if !encode_camera_frame(
                                data,
                                &mut encoder,
                                &worker_force_keyframe,
                                &frame_context,
                                &tx,
                            ) {
                                break;
                            }
                        }
                        Err(error) => {
                            if !worker_stop.load(Ordering::Relaxed) {
                                crate::zcall_log!("[zcall-camera] V4L2 capture: {error}");
                            }
                            break;
                        }
                    }
                }
            })
            .map_err(|error| format!("Start camera worker: {error}"))?;

        Ok(CameraCapture {
            stop,
            force_keyframe,
            frames,
            worker: Mutex::new(Some(worker)),
        })
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn is_capture_node(flags: v4l::capability::Flags) -> bool {
    use v4l::capability::Flags;
    flags.intersects(Flags::VIDEO_CAPTURE | Flags::VIDEO_CAPTURE_MPLANE)
}

pub(crate) fn camera_device_paths() -> Vec<(String, String)> {
    let mut devices = Vec::new();
    let directory = Path::new("/sys/class/video4linux");
    let Ok(entries) = fs::read_dir(directory) else {
        return devices;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("video") {
            continue;
        }
        let id = format!("/dev/{name}");
        if !Path::new(&id).exists() {
            continue;
        }
        // UVC devices often expose a second metadata-only node with the same
        // product name. Only advertise nodes that can actually capture frames.
        let Ok(device) = v4l::Device::with_path(&id) else {
            continue;
        };
        let Ok(capabilities) = device.query_caps() else {
            continue;
        };
        if !is_capture_node(capabilities.capabilities)
            || !capabilities
                .capabilities
                .contains(v4l::capability::Flags::STREAMING)
        {
            continue;
        }
        let Ok(formats) = device.enum_formats() else {
            continue;
        };
        if !formats
            .iter()
            .any(|format| format.fourcc == FourCC::new(b"YUYV"))
        {
            continue;
        }
        let label = fs::read_to_string(entry.path().join("name"))
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| name.clone());
        devices.push((id, label));
    }
    devices.sort_by(|a, b| a.0.cmp(&b.0));
    devices
}
