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
    assert!(
        crate::devices::camera_device_paths()
            .iter()
            .any(|(path, _)| path == &device),
        "camera inventory must include the requested usable capture node {device}"
    );
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
#[ignore = "requires a physical V4L2 camera and a non-capture video node"]
fn real_camera_selection_skips_metadata_node_preference() {
    let available = crate::devices::camera_device_paths();
    let metadata_node = std::fs::read_dir("/dev")
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("video"))
                && !available
                    .iter()
                    .any(|(capture, _)| path.to_str() == Some(capture.as_str()))
        });
    let Some(metadata_node) = metadata_node else {
        return;
    };
    let preferred = metadata_node.to_string_lossy().into_owned();
    let capture =
        CameraCapture::start(Some(&preferred), 640, 360, 30, 500_000).unwrap_or_else(|error| {
            panic!("metadata preference {preferred} should select a usable camera: {error}")
        });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if capture.try_recv().is_some() {
            capture.stop();
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    capture.stop();
    panic!("valid camera did not produce a frame after rejecting metadata preference {preferred}");
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

#[test]
fn pipewire_bgra_conversion_respects_stride_and_packs_yuyv_pairs() {
    let padded = [
        0_u8, 0, 255, 255, // red
        0, 255, 0, 255, // green
        9, 9, 9, 9, // row padding
        255, 0, 0, 255, // blue
        255, 255, 255, 255, // white
        9, 9, 9, 9, // row padding
    ];
    let converted = super::bgra_to_yuyv(&padded, 2, 2, 12).unwrap();
    assert_eq!(converted.len(), 8);
    assert_eq!(converted[0], 82); // red luma
    assert_eq!(converted[2], 144); // green luma
    assert_eq!(converted[4], 41); // blue luma
    assert_eq!(converted[6], 235); // white luma
    assert!(super::bgra_to_yuyv(&padded, 2, 2, 4).is_none());
}

#[test]
fn pipewire_uyvy_conversion_reorders_chroma_and_luma_bytes() {
    let input = [128_u8, 10, 140, 20, 130, 30, 150, 40];
    assert_eq!(
        super::uyvy_to_yuyv(&input, 2, 2, 4).unwrap(),
        [10, 128, 20, 140, 30, 130, 40, 150]
    );
    assert!(super::uyvy_to_yuyv(&input, 3, 2, 6).is_none());
}

/// Opt-in hardware acceptance test for the native `PipeWire` camera path.
/// Run explicitly with --ignored on a session exposing a camera-role node.
#[test]
#[ignore = "requires a live PipeWire session and camera-role node; run explicitly with --ignored"]
fn real_pipewire_camera_captures_and_encodes_h264() {
    let capture =
        CameraCapture::start_pipewire_direct_selected(Some("/dev/video0"), 640, 360, 30, 500_000)
            .unwrap_or_else(|error| panic!("open native PipeWire camera: {error}"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);

    while std::time::Instant::now() < deadline {
        if let Some(frame) = capture.try_recv() {
            capture.stop();
            assert!(
                (160..=1920).contains(&frame.width)
                    && (120..=1080).contains(&frame.height)
                    && frame.width % 2 == 0
                    && frame.height % 2 == 0,
                "PipeWire camera negotiated invalid dimensions {}x{}",
                frame.width,
                frame.height
            );
            assert!(
                !frame.data.is_empty(),
                "PipeWire camera encoded an empty frame"
            );
            assert!(
                frame.data.starts_with(&[0, 0, 0, 1]) || frame.data.starts_with(&[0, 0, 1]),
                "PipeWire camera output must be H.264 Annex-B"
            );
            assert!(
                crate::video::avc_codec_string(&frame.data)
                    .is_some_and(|codec| codec.starts_with("avc1.")),
                "PipeWire camera SPS must identify an H.264 codec"
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    capture.stop();
    panic!("native PipeWire camera did not produce an encoded frame within 10 seconds");
}

/// Opt-in end-to-end test of camera permission, portal remote, and H.264.
#[test]
#[ignore = "requires an interactive XDG Camera portal and camera permission; run explicitly with --ignored"]
fn real_camera_portal_captures_and_encodes_h264() {
    let capture = CameraCapture::start_pipewire_portal(640, 360, 30, 500_000)
        .unwrap_or_else(|error| panic!("open XDG Camera portal stream: {error}"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);

    while std::time::Instant::now() < deadline {
        if let Some(frame) = capture.try_recv() {
            capture.stop();
            assert!(frame.width >= 160 && frame.height >= 120);
            assert!(
                !frame.data.is_empty(),
                "portal camera encoded an empty frame"
            );
            assert!(
                frame.data.starts_with(&[0, 0, 0, 1]) || frame.data.starts_with(&[0, 0, 1]),
                "portal camera output must be H.264 Annex-B"
            );
            assert!(
                crate::video::avc_codec_string(&frame.data)
                    .is_some_and(|codec| codec.starts_with("avc1.")),
                "portal camera SPS must identify an H.264 codec"
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    capture.stop();
    panic!("XDG Camera portal did not produce an encoded frame within 15 seconds");
}

#[test]
#[ignore = "requires an interactive XDG Camera portal and camera permission; run explicitly with --ignored"]
fn real_camera_portal_can_restart_repeatedly_in_one_process() {
    for cycle in 1..=3 {
        let capture = CameraCapture::start_pipewire_portal(640, 360, 30, 500_000)
            .unwrap_or_else(|error| panic!("portal camera cycle {cycle}: {error}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let frame = loop {
            if let Some(frame) = capture.try_recv() {
                break frame;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "portal camera cycle {cycle} did not produce a frame within 15 seconds"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert!(frame.width >= 160 && frame.height >= 120);
        assert!(
            !frame.data.is_empty(),
            "portal cycle {cycle} encoded an empty frame"
        );
        assert!(
            crate::video::avc_codec_string(&frame.data)
                .is_some_and(|codec| codec.starts_with("avc1.")),
            "portal cycle {cycle} did not produce an H.264 SPS"
        );
        capture.stop();
        drop(capture);
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[test]
fn explicit_camera_backends_do_not_fall_back_to_another_backend() {
    use crate::devices::CameraBackend;
    use std::cell::RefCell;
    let calls = RefCell::new(Vec::new());

    let selected = select_camera_backend(
        CameraBackend::PipeWire,
        || {
            calls.borrow_mut().push("v4l2");
            Ok("V4L2")
        },
        || {
            calls.borrow_mut().push("pipewire");
            Ok("PipeWire")
        },
        || {
            calls.borrow_mut().push("portal");
            Ok("Portal")
        },
    )
    .unwrap();
    assert_eq!(selected, "PipeWire");
    assert_eq!(*calls.borrow(), ["pipewire"]);

    calls.borrow_mut().clear();
    let error = select_camera_backend::<()>(
        CameraBackend::V4l2,
        || {
            calls.borrow_mut().push("v4l2");
            Err("camera busy".into())
        },
        || {
            calls.borrow_mut().push("pipewire");
            Ok(())
        },
        || {
            calls.borrow_mut().push("portal");
            Ok(())
        },
    )
    .unwrap_err();
    assert_eq!(error, "camera busy");
    assert_eq!(*calls.borrow(), ["v4l2"]);
}

#[test]
fn auto_camera_backend_uses_v4l2_then_pipewire_then_portal() {
    use crate::devices::CameraBackend;
    use std::cell::RefCell;
    let calls = RefCell::new(Vec::new());

    let selected = select_camera_backend(
        CameraBackend::Auto,
        || {
            calls.borrow_mut().push("v4l2");
            Err("V4L2 unavailable".into())
        },
        || {
            calls.borrow_mut().push("pipewire");
            Err("PipeWire unavailable".into())
        },
        || {
            calls.borrow_mut().push("portal");
            Ok("Portal")
        },
    )
    .unwrap();
    assert_eq!(selected, "Portal");
    assert_eq!(*calls.borrow(), ["v4l2", "pipewire", "portal"]);
}

#[test]
fn auto_camera_backend_stops_after_first_successful_backend() {
    use crate::devices::CameraBackend;
    use std::cell::RefCell;
    let calls = RefCell::new(Vec::new());

    let selected = select_camera_backend(
        CameraBackend::Auto,
        || {
            calls.borrow_mut().push("v4l2");
            Err("V4L2 unavailable".into())
        },
        || {
            calls.borrow_mut().push("pipewire");
            Ok("PipeWire")
        },
        || {
            calls.borrow_mut().push("portal");
            Ok("Portal")
        },
    )
    .unwrap();
    assert_eq!(selected, "PipeWire");
    assert_eq!(*calls.borrow(), ["v4l2", "pipewire"]);
}

#[test]
fn auto_camera_backend_reports_all_backend_failures() {
    use crate::devices::CameraBackend;
    let error = select_camera_backend::<()>(
        CameraBackend::Auto,
        || Err("V4L2 unavailable".into()),
        || Err("PipeWire unavailable".into()),
        || Err("portal denied".into()),
    )
    .unwrap_err();
    assert!(error.contains("V4L2: V4L2 unavailable"));
    assert!(error.contains("PipeWire: PipeWire unavailable"));
    assert!(error.contains("portal: portal denied"));
}
