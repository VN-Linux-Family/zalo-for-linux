//! V4L2 camera capture and native OpenH264 encoding.
//!
//! Frames enter as YUYV, are converted to I420 and encoded before crossing the
//! Rust media/UI boundary. JavaScript receives encoded H.264 only for the
//! local preview; network transmission stays entirely inside the Rust engine.
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile, UsageType,
};
use openh264::formats::YUVBuffer;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

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

impl CameraCapture {
    pub fn start(
        device_path: Option<&str>,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
    ) -> Result<Self, String> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (device_path, width, height, fps, bitrate);
            return Err("V4L2 camera capture is only available on Linux".into());
        }

        #[cfg(target_os = "linux")]
        {
            let path = device_path
                .filter(|path| !path.is_empty())
                .map(str::to_owned)
                .or_else(first_camera_device)
                .ok_or_else(|| "No V4L2 camera device was found".to_string())?;

            if !Path::new(&path).exists() {
                return Err(format!("Camera device does not exist: {path}"));
            }

            let device =
                Device::with_path(&path).map_err(|error| format!("Open {path}: {error}"))?;
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

            let mut stream = MmapStream::with_buffers(&device, Type::VideoCapture, 4)
                .map_err(|error| format!("Create V4L2 stream {path}: {error}"))?;
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = stop.clone();
            let force_keyframe = Arc::new(AtomicBool::new(false));
            let worker_force_keyframe = force_keyframe.clone();
            let (tx, frames) = sync_channel(2);
            let frame_width = actual_width as u32;
            let frame_height = actual_height as u32;
            let stride = format.stride as usize;
            let fps = fps.clamp(5, 30);

            let worker = thread::Builder::new()
                .name("zcall-camera".into())
                .spawn(move || {
                    let config = EncoderConfig::new()
                        .bitrate(BitRate::from_bps(bitrate.clamp(80_000, 2_000_000)))
                        .max_frame_rate(FrameRate::from_hz(fps as f32))
                        .usage_type(UsageType::CameraVideoRealTime)
                        .profile(Profile::Baseline)
                        .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 2));
                    let mut encoder: Option<Encoder> = None;

                    while !worker_stop.load(Ordering::Relaxed) {
                        match stream.next() {
                            Ok((data, _metadata)) => {
                                if take_keyframe_request(&worker_force_keyframe) {
                                    encoder = None;
                                }
                                if encoder.is_none() {
                                    encoder = match Encoder::with_api_config(
                                        openh264::OpenH264API::from_source(),
                                        config,
                                    ) {
                                        Ok(encoder) => Some(encoder),
                                        Err(error) => {
                                            crate::zcall_log!(
                                                "[zcall-camera] OpenH264 reinit: {error}"
                                            );
                                            continue;
                                        }
                                    };
                                }
                                let Some(yuv) =
                                    yuyv_to_i420(data, actual_width, actual_height, stride)
                                else {
                                    crate::zcall_log!(
                                        "[zcall-camera] Dropped malformed YUYV frame"
                                    );
                                    continue;
                                };
                                let Some(encoder) = encoder.as_mut() else {
                                    continue;
                                };
                                match encoder.encode(&yuv) {
                                    Ok(bitstream) => {
                                        let frame_type = bitstream.frame_type();
                                        let key = matches!(
                                            frame_type,
                                            FrameType::IDR | FrameType::I | FrameType::IPMixed
                                        );
                                        let mut encoded = Vec::new();
                                        bitstream.write_vec(&mut encoded);
                                        if encoded.is_empty() {
                                            continue;
                                        }
                                        let frame = EncodedCameraFrame {
                                            data: encoded,
                                            key,
                                            width: frame_width,
                                            height: frame_height,
                                        };
                                        match tx.try_send(frame) {
                                            Ok(()) | Err(TrySendError::Full(_)) => {}
                                            Err(TrySendError::Disconnected(_)) => break,
                                        }
                                    }
                                    Err(error) => {
                                        crate::zcall_log!(
                                            "[zcall-camera] OpenH264 encode: {error}"
                                        );
                                    }
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

            Ok(Self {
                stop,
                force_keyframe,
                frames,
                worker: Mutex::new(Some(worker)),
            })
        }
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
                let _ = worker.join();
            }
        }
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "linux")]
fn first_camera_device() -> Option<String> {
    crate::devices::camera_device_paths()
        .into_iter()
        .next()
        .map(|(path, _)| path)
}

#[cfg(not(target_os = "linux"))]
fn first_camera_device() -> Option<String> {
    None
}

/// Convert packed YUYV 4:2:2 into planar I420 4:2:0, averaging chroma across
/// each adjacent pair of rows. stride is the V4L2 bytes-per-line value.
fn yuyv_to_i420(input: &[u8], width: usize, height: usize, stride: usize) -> Option<YUVBuffer> {
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
mod tests {
    use super::*;
    use openh264::formats::YUVSource;

    #[test]
    fn converts_yuyv_to_i420_respecting_input_stride() {
        let row0 = [10, 20, 30, 40, 50, 60, 70, 80, 0, 0, 0, 0];
        let row1 = [11, 30, 31, 50, 51, 70, 71, 90, 0, 0, 0, 0];
        let input = [row0.as_slice(), row1.as_slice()].concat();
        let yuv = yuyv_to_i420(&input, 4, 2, 12).unwrap();
        assert_eq!(yuv.y(), &[10, 30, 50, 70, 11, 31, 51, 71]);
        assert_eq!(yuv.u(), &[25, 65]);
        assert_eq!(yuv.v(), &[45, 85]);
    }

    #[test]
    fn camera_keyframe_request_is_consumed_once_by_the_encoder_worker() {
        let requested = AtomicBool::new(false);
        assert!(!take_keyframe_request(&requested));
        requested.store(true, Ordering::Release);
        assert!(take_keyframe_request(&requested));
        assert!(!take_keyframe_request(&requested));
    }

    #[test]
    fn rejects_invalid_yuyv_layouts() {
        assert!(yuyv_to_i420(&[], 4, 2, 4).is_none());
        assert!(yuyv_to_i420(&[], 3, 2, 8).is_none());
        assert!(yuyv_to_i420(&[], 4, 1, 8).is_none());
    }

    /// Opt-in hardware acceptance test. Unlike the unit tests above, this opens
    /// a real V4L2 device and verifies the complete capture -> YUYV conversion
    /// -> OpenH264 pipeline. Run explicitly with --ignored and
    /// ZCALL_TEST_CAMERA_DEVICE=/dev/videoN.
    #[test]
    #[ignore = "requires a physical V4L2 camera; run explicitly with --ignored"]
    fn real_v4l2_camera_captures_and_encodes_h264() {
        let device =
            std::env::var("ZCALL_TEST_CAMERA_DEVICE").unwrap_or_else(|_| "/dev/video0".to_string());
        let capture = CameraCapture::start(Some(&device), 640, 360, 30, 500_000)
            .unwrap_or_else(|error| panic!("open real camera {device}: {error}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

        while std::time::Instant::now() < deadline {
            if let Some(frame) = capture.try_recv() {
                capture.stop();
                assert_eq!((frame.width, frame.height), (640, 360));
                assert!(!frame.data.is_empty(), "real camera encoded an empty frame");
                assert!(
                    frame.data.starts_with(&[0, 0, 0, 1]) || frame.data.starts_with(&[0, 0, 1]),
                    "real camera output must be H.264 Annex-B"
                );
                assert!(
                    crate::video::avc_codec_string(&frame.data)
                        .is_some_and(|codec| codec.starts_with("avc1.")),
                    "real camera SPS must identify an H.264 codec"
                );
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        capture.stop();
        panic!("real camera {device} did not produce an encoded frame within 10 seconds");
    }

    #[test]
    fn openh264_produces_annex_b_for_a_keyframe() {
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(200_000))
            .max_frame_rate(FrameRate::from_hz(15.0))
            .usage_type(UsageType::CameraVideoRealTime)
            .profile(Profile::Baseline);
        let mut encoder =
            Encoder::with_api_config(openh264::OpenH264API::from_source(), config).unwrap();
        let yuv = YUVBuffer::new(320, 240);
        let stream = encoder.encode(&yuv).unwrap();
        assert!(matches!(
            stream.frame_type(),
            FrameType::IDR | FrameType::I | FrameType::IPMixed
        ));
        let mut bytes = Vec::new();
        stream.write_vec(&mut bytes);
        assert!(!bytes.is_empty());
        assert!(
            bytes.starts_with(&[0, 0, 0, 1]) || bytes.starts_with(&[0, 0, 1]),
            "OpenH264 output should be Annex-B"
        );
        assert!(
            crate::video::avc_codec_string(&bytes).is_some_and(|codec| codec.starts_with("avc1.")),
            "local preview codec must be derived from the actual SPS"
        );
    }
}
