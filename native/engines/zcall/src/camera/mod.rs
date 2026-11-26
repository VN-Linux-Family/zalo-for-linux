//! Camera capture backend selection, shared frame conversion, and `OpenH264` encoding.
//!
//! Frames enter as YUYV, are converted to I420 and encoded before crossing the
//! Rust media/UI boundary. JavaScript receives encoded H.264 only for the
//! local preview; network transmission stays entirely inside the Rust engine.

mod pipewire;
mod portal;
pub(crate) mod v4l2;
#[cfg(test)]
use openh264::encoder::{BitRate, FrameRate, Profile, UsageType};
use openh264::encoder::{Encoder, EncoderConfig, FrameType};
use openh264::formats::YUVBuffer;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub struct EncodedCameraFrame {
    pub data: Vec<u8>,
    pub key: bool,
    pub width: u32,
    pub height: u32,
}

pub struct CameraCapture {
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    frames: Receiver<EncodedCameraFrame>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
pub(super) struct CameraFrameContext {
    config: EncoderConfig,
    actual_width: usize,
    actual_height: usize,
    stride: usize,
    frame_width: u32,
    frame_height: u32,
}

#[cfg(target_os = "linux")]
pub(super) fn encode_camera_frame(
    data: &[u8],
    encoder: &mut Option<Encoder>,
    force_keyframe: &AtomicBool,
    context: &CameraFrameContext,
    tx: &std::sync::mpsc::SyncSender<EncodedCameraFrame>,
) -> bool {
    if take_keyframe_request(force_keyframe) {
        *encoder = None;
    }
    if encoder.is_none() {
        *encoder =
            match Encoder::with_api_config(openh264::OpenH264API::from_source(), context.config) {
                Ok(encoder) => Some(encoder),
                Err(error) => {
                    crate::zcall_log!("[zcall-camera] OpenH264 reinit: {error}");
                    return true;
                }
            };
    }
    let Some(yuv) = yuyv_to_i420(
        data,
        context.actual_width,
        context.actual_height,
        context.stride,
    ) else {
        crate::zcall_log!("[zcall-camera] Dropped malformed YUYV frame");
        return true;
    };
    let Some(encoder) = encoder.as_mut() else {
        return true;
    };
    match encoder.encode(&yuv) {
        Ok(bitstream) => {
            let key = matches!(
                bitstream.frame_type(),
                FrameType::IDR | FrameType::I | FrameType::IPMixed
            );
            let mut encoded_frame_data = Vec::new();
            bitstream.write_vec(&mut encoded_frame_data);
            if encoded_frame_data.is_empty() {
                return true;
            }
            let frame = EncodedCameraFrame {
                data: encoded_frame_data,
                key,
                width: context.frame_width,
                height: context.frame_height,
            };
            !matches!(tx.try_send(frame), Err(TrySendError::Disconnected(_)))
        }
        Err(error) => {
            crate::zcall_log!("[zcall-camera] OpenH264 encode: {error}");
            true
        }
    }
}

fn select_camera_backend<T>(
    backend: crate::devices::CameraBackend,
    v4l2: impl FnOnce() -> Result<T, String>,
    pipewire: impl FnOnce() -> Result<T, String>,
    portal: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    use crate::devices::CameraBackend;
    match backend {
        CameraBackend::V4l2 => v4l2(),
        CameraBackend::PipeWire => pipewire(),
        CameraBackend::Portal => portal(),
        CameraBackend::Auto => v4l2().or_else(|v4l2_error| {
            pipewire().or_else(|pipewire_error| {
                portal().map_err(|portal_error| {
                    format!("camera auto-select failed (V4L2: {v4l2_error}; PipeWire: {pipewire_error}; portal: {portal_error})")
                })
            })
        }),
    }
}

impl CameraCapture {
    /// Select the configured camera backend. `auto` tries V4L2, native `PipeWire`,
    /// then the XDG Camera portal in that order.
    pub fn start_selected(
        device_path: Option<&str>,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self, String> {
        select_camera_backend(
            crate::devices::load_prefs().camera_backend,
            || Self::start(device_path, width, height, fps, bitrate),
            || Self::start_pipewire_direct_selected(device_path, width, height, fps, bitrate),
            || Self::start_pipewire_portal(width, height, fps, bitrate),
        )
    }

    pub fn start(
        device_path: Option<&str>,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self, String> {
        v4l2::start_v4l2(device_path, width, height, fps, bitrate)
    }

    pub fn try_recv(&self) -> Option<EncodedCameraFrame> {
        self.frames.try_recv().ok()
    }

    /// Force the next captured frame through a fresh encoder, producing SPS/PPS
    /// and an IDR frame for a remote decoder that requested recovery.
    pub fn request_keyframe(&self) {
        self.force_keyframe.store(true, Ordering::Release);
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = std::thread::Builder::new()
                    .name("zcall-camera-reaper".into())
                    .spawn(move || {
                        let _ = worker.join();
                    });
            }
        }
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reorder packed UYVY 4:2:2 into the V4L2 YUYV byte order.
fn uyvy_to_yuyv(input: &[u8], width: usize, height: usize, stride: usize) -> Option<Vec<u8>> {
    if width < 2
        || height == 0
        || !width.is_multiple_of(2)
        || stride < width.checked_mul(2)?
        || input.len() < stride.checked_mul(height)?
    {
        return None;
    }
    let mut output = vec![0; width.checked_mul(height)?.checked_mul(2)?];
    for row in 0..height {
        let src = row * stride;
        let dst = row * width * 2;
        for x in (0..width).step_by(2) {
            let s = src + x * 2;
            let d = dst + x * 2;
            output[d] = input[s + 1];
            output[d + 1] = input[s];
            output[d + 2] = input[s + 3];
            output[d + 3] = input[s + 2];
        }
    }
    Some(output)
}

/// Convert packed BGRA pixels to YUYV 4:2:2 for the existing camera encoder path.
fn bgra_to_yuyv(input: &[u8], width: usize, height: usize, stride: usize) -> Option<Vec<u8>> {
    if width < 2
        || height == 0
        || !width.is_multiple_of(2)
        || stride < width.checked_mul(4)?
        || input.len() < stride.checked_mul(height)?
    {
        return None;
    }
    let mut output = vec![0; width.checked_mul(height)?.checked_mul(2)?];
    let to_y = |r: i32, g: i32, b: i32| ((66 * r + 129 * g + 25 * b + 128) >> 8) + 16;
    let to_u = |r: i32, g: i32, b: i32| ((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128;
    let to_v = |r: i32, g: i32, b: i32| ((112 * r - 94 * g - 18 * b + 128) >> 8) + 128;
    let clamp = |value: i32| value.clamp(0, 255) as u8;
    for y in 0..height {
        for x in (0..width).step_by(2) {
            let p0 = y * stride + x * 4;
            let p1 = p0 + 4;
            let (b0, g0, r0) = (
                i32::from(input[p0]),
                i32::from(input[p0 + 1]),
                i32::from(input[p0 + 2]),
            );
            let (b1, g1, r1) = (
                i32::from(input[p1]),
                i32::from(input[p1 + 1]),
                i32::from(input[p1 + 2]),
            );
            let out = (y * width + x) * 2;
            output[out] = clamp(to_y(r0, g0, b0));
            output[out + 1] = clamp(i32::midpoint(to_u(r0, g0, b0), to_u(r1, g1, b1)));
            output[out + 2] = clamp(to_y(r1, g1, b1));
            output[out + 3] = clamp(i32::midpoint(to_v(r0, g0, b0), to_v(r1, g1, b1)));
        }
    }
    Some(output)
}

/// Convert packed YUYV 4:2:2 into planar I420 4:2:0, averaging chroma across
/// each adjacent pair of rows. stride is the V4L2 bytes-per-line value.
pub(super) fn yuyv_to_i420(
    input: &[u8],
    width: usize,
    height: usize,
    stride: usize,
) -> Option<YUVBuffer> {
    if width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
        || stride < width * 2
    {
        return None;
    }
    let required = stride.checked_mul(height)?;
    if input.len() < required {
        return None;
    }

    let luma_len = width.checked_mul(height)?;
    let chroma_len = luma_len / 4;
    let mut output = vec![0u8; luma_len + chroma_len * 2];

    for y in 0..height {
        let row = &input[y * stride..y * stride + width * 2];
        for pair in 0..(width / 2) {
            let src = pair * 4;
            let dst = y * width + pair * 2;
            output[dst] = row[src];
            output[dst + 1] = row[src + 2];
        }
    }

    let u_offset = luma_len;
    let v_offset = luma_len + chroma_len;
    for y in (0..height).step_by(2) {
        let row0 = &input[y * stride..y * stride + width * 2];
        let row1 = &input[(y + 1) * stride..(y + 1) * stride + width * 2];
        for pair in 0..(width / 2) {
            let src = pair * 4;
            let dst = (y / 2) * (width / 2) + pair;
            output[u_offset + dst] =
                (u16::from(row0[src + 1]) + u16::from(row1[src + 1])).div_ceil(2) as u8;
            output[v_offset + dst] =
                (u16::from(row0[src + 3]) + u16::from(row1[src + 3])).div_ceil(2) as u8;
        }
    }

    Some(YUVBuffer::from_vec(output, width, height))
}

fn take_keyframe_request(requested: &AtomicBool) -> bool {
    requested.swap(false, Ordering::AcqRel)
}

#[cfg(test)]
#[path = "../tests/camera.rs"]
mod tests;
