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
/// -> `OpenH264` pipeline. Run explicitly with --ignored and
/// `ZCALL_TEST_CAMERA_DEVICE=/dev/videoN`.
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
    assert_ne!(bytes, [] as [u8; 0]);
    assert!(
        bytes.starts_with(&[0, 0, 0, 1]) || bytes.starts_with(&[0, 0, 1]),
        "OpenH264 output should be Annex-B"
    );
    assert!(
        crate::video::avc_codec_string(&bytes).is_some_and(|codec| codec.starts_with("avc1.")),
        "local preview codec must be derived from the actual SPS"
    );
}
