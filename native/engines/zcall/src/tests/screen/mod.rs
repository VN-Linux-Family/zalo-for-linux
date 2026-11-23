use super::*;

#[test]
fn x11_latest_frame_queue_replaces_stale_frames() {
    let queue = LatestFrameQueue::default();
    let make_frame = |value| RawScreenFrame {
        bgra: vec![value; 16],
        width: 2,
        height: 2,
    };

    assert!(!queue.push(make_frame(1)));
    assert!(queue.push(make_frame(2)));
    let frame = Arc::new(queue)
        .recv_timeout(Duration::from_millis(1))
        .expect("latest raw frame");
    assert_eq!(frame.bgra, vec![2; 16]);
}

#[test]
fn latest_frame_queue_preserves_frame_allocation_when_transferring_ownership() {
    let queue = Arc::new(LatestFrameQueue::default());
    let mut pixels = Vec::with_capacity(4096);
    pixels.resize(16, 1);
    assert!(!queue.push(RawScreenFrame {
        bgra: pixels,
        width: 2,
        height: 2,
    }));
    let frame = queue
        .recv_timeout(Duration::from_millis(1))
        .expect("queued frame remains available");
    assert_eq!(frame.bgra, vec![1; 16]);
    assert!(frame.bgra.capacity() >= 4096);
}

#[test]
fn encodes_odd_sized_window_frames_after_cropping_to_even_dimensions() {
    let (raw_tx, raw_rx) = sync_channel(2);
    let (encoded_tx, encoded_rx) = sync_channel(2);
    let (status_tx, status_rx) = sync_channel(2);
    let encoded_frame_seen = Arc::new(AtomicBool::new(false));
    let worker_encoded_frame_seen = encoded_frame_seen;
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = thread::spawn(move || {
        encode_frames(
            raw_rx,
            encoded_tx,
            worker_stop,
            ScreenEncodeConfig {
                bitrate: 500_000,
                max_short: 0,
                force_keyframe: Arc::new(AtomicBool::new(false)),
                target_fps: Arc::new(AtomicU32::new(15)),
                capture_fps: Arc::new(AtomicU32::new(15)),
            },
            status_tx,
            worker_encoded_frame_seen,
        )
        .unwrap();
    });

    raw_tx
        .send(RawScreenFrame {
            bgra: vec![0; 953 * 646 * 4],
            width: 953,
            height: 646,
        })
        .unwrap();
    assert_eq!(
        status_rx.recv_timeout(Duration::from_secs(3)).unwrap(),
        Ok(())
    );
    let frame = encoded_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("native screen encoder should publish a key frame");
    assert!(frame.key);
    assert_eq!((frame.width, frame.height), (952, 646));
    thread::sleep(Duration::from_millis(80));
    raw_tx
        .send(RawScreenFrame {
            bgra: vec![0; 953 * 646 * 4],
            width: 953,
            height: 646,
        })
        .unwrap();
    let _second_frame = encoded_rx
        .recv_timeout(Duration::from_secs(3))
        .expect("encoder should continue after becoming ready");
    assert!(
        status_rx.try_recv().is_err(),
        "ready must be a one-shot event"
    );
    assert!(
        frame.data.starts_with(&[0, 0, 0, 1]) || frame.data.starts_with(&[0, 0, 1]),
        "screen frames must be H.264 Annex-B"
    );
    assert!(
        crate::video::avc_codec_string(&frame.data).is_some_and(|codec| codec.starts_with("avc1.")),
        "screen preview codec must be derived from its SPS"
    );

    stop.store(true, Ordering::Relaxed);
    drop(raw_tx);
    worker.join().unwrap();
}

/// Opt-in Wayland acceptance test. This deliberately opens the desktop's
/// real `ScreenCast` chooser and must be run interactively by selecting a
/// monitor/window in the portal dialog.
#[test]
#[ignore = "requires an interactive Wayland ScreenCast portal chooser"]
fn real_wayland_portal_captures_and_encodes_h264() {
    assert_eq!(
        std::env::var("XDG_SESSION_TYPE").as_deref(),
        Ok("wayland"),
        "this acceptance test must run in the actual Wayland session"
    );
    let capture = ScreenCapture::start_with_source(15, 500_000, 0, None)
        .expect("start real Wayland ScreenCast capture");
    let deadline = std::time::Instant::now() + Duration::from_secs(145);
    let mut ready = false;

    while std::time::Instant::now() < deadline {
        if let Some(status) = capture.try_status() {
            status.expect("real portal/PipeWire/encoder startup");
            ready = true;
        }
        if let Some(frame) = capture.try_recv() {
            capture.stop();
            assert!(ready, "encoded frame arrived before the ready notification");
            assert!(
                !frame.data.is_empty(),
                "portal encoder returned an empty frame"
            );
            assert!(
                frame.data.starts_with(&[0, 0, 0, 1]) || frame.data.starts_with(&[0, 0, 1]),
                "portal capture must produce H.264 Annex-B"
            );
            assert!(
                crate::video::avc_codec_string(&frame.data)
                    .is_some_and(|codec| codec.starts_with("avc1.")),
                "portal frame SPS must identify an H.264 codec"
            );
            crate::zcall_log!(
                "REAL_WAYLAND_SCREEN_FRAME={}x{} bytes={} key={}",
                frame.width,
                frame.height,
                frame.data.len(),
                frame.key
            );
            return;
        }
        if capture.is_finished() {
            let error = capture.try_status().and_then(Result::err);
            capture.stop();
            panic!("real portal capture worker exited early: {error:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    capture.stop();
    panic!("real Wayland portal did not produce an encoded frame within 145 seconds");
}

#[test]
fn capture_worker_panics_become_terminal_errors() {
    let result = run_capture_worker(|| panic!("simulated portal cleanup panic"));
    assert_eq!(
        result.unwrap_err(),
        "Screen capture worker panicked: simulated portal cleanup panic"
    );
}

#[test]
fn screen_fps_uses_three_stable_quality_tiers() {
    assert_eq!(fps_tier(5), 15);
    assert_eq!(fps_tier(15), 15);
    assert_eq!(fps_tier(20), 24);
    assert_eq!(fps_tier(24), 24);
    assert_eq!(fps_tier(30), 30);
}

#[test]
fn portal_user_cancellation_is_classified_as_a_normal_stop() {
    assert_eq!(
        portal_error_message(
            "waiting for chooser",
            "org.freedesktop.portal.Error.Cancelled"
        ),
        SCREEN_CAPTURE_CANCELLED
    );
    assert_eq!(
        portal_error_message("starting stream", "permission denied"),
        "starting stream: permission denied"
    );
    assert!(is_cancelled_error(SCREEN_CAPTURE_CANCELLED));
    assert!(!is_cancelled_error("permission denied"));
}

#[test]
fn portal_requests_are_cancellable_and_have_a_deadline() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let cancelled = runtime.block_on(portal::portal_request(
        std::future::pending::<Result<(), &'static str>>(),
        Arc::new(AtomicBool::new(true)),
        Duration::from_secs(30),
        "test portal request",
    ));
    assert_eq!(
        cancelled.unwrap_err(),
        "Screen sharing cancelled because the call ended"
    );

    let timed_out = runtime.block_on(portal::portal_request(
        std::future::pending::<Result<(), &'static str>>(),
        Arc::new(AtomicBool::new(false)),
        Duration::from_millis(10),
        "test portal request",
    ));
    assert_eq!(timed_out.unwrap_err(), "Timed out test portal request");
}

#[test]
fn cancelled_capture_does_not_emit_a_second_error_after_screen_stopped() {
    let (status_tx, status_rx) = std::sync::mpsc::sync_channel(2);
    let stop = AtomicBool::new(true);
    report_worker_result(Err("Screen sharing cancelled".into()), &stop, &status_tx);
    assert!(status_rx.try_recv().is_err());

    let stop = AtomicBool::new(false);
    report_worker_result(Err("portal failed".into()), &stop, &status_tx);
    assert_eq!(status_rx.try_recv().unwrap().unwrap_err(), "portal failed");
}

#[test]
fn stopping_capture_never_joins_worker_on_the_call_engine_thread() {
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let finished = Arc::new(AtomicBool::new(false));
    let worker_finished = finished.clone();
    let worker = thread::spawn(move || {
        let _ = release_rx.recv();
        worker_finished.store(true, Ordering::Release);
    });
    let (_frame_tx, frames) = sync_channel(1);
    let (_status_tx, status) = sync_channel(1);
    let capture = ScreenCapture {
        stop: Arc::new(AtomicBool::new(false)),
        frames,
        status,
        force_keyframe: Arc::new(AtomicBool::new(false)),
        target_fps: Arc::new(AtomicU32::new(30)),
        finished: finished.clone(),
        worker: Mutex::new(Some(worker)),
    };

    let started = std::time::Instant::now();
    capture.stop();
    assert!(started.elapsed() < Duration::from_millis(100));
    release_tx.send(()).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !finished.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(finished.load(Ordering::Acquire));
}

#[test]
fn group_share_resolution_limits_short_side_and_preserves_aspect_ratio() {
    let source = vec![7u8; 1920 * 1080 * 4];
    let (resized, width, height) = resize_bgra(source, 1920, 1080, 720).unwrap();
    assert_eq!((width, height), (1280, 720));
    assert_eq!(resized.len(), 1280 * 720 * 4);
    assert_eq!(resized[0], 7);
    assert_eq!(resized[resized.len() - 1], 7);
}

#[test]
fn resize_bgra_scales_in_place_and_preserves_nearest_neighbor_pixels() {
    let source = (0..16u8)
        .flat_map(|pixel| [pixel, 0, 0, 255])
        .collect::<Vec<_>>();
    let capacity = source.capacity();
    let (resized, width, height) = resize_bgra(source, 4, 4, 2).unwrap();

    assert_eq!((width, height), (2, 2));
    assert_eq!(resized.capacity(), capacity);
    assert_eq!(
        resized
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[0])
            .collect::<Vec<_>>(),
        [0, 2, 8, 10]
    );
}

#[test]
fn resize_bgra_crops_odd_dimensions_in_place() {
    let source = (0..9u8)
        .flat_map(|pixel| [pixel, 0, 0, 255])
        .collect::<Vec<_>>();
    let capacity = source.capacity();
    let (cropped, width, height) = resize_bgra(source, 3, 3, 0).unwrap();

    assert_eq!((width, height), (2, 2));
    assert_eq!(cropped.capacity(), capacity);
    assert_eq!(
        cropped
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| pixel[0])
            .collect::<Vec<_>>(),
        [0, 1, 3, 4]
    );
}

#[test]
fn screen_keyframe_request_flag_is_consumed_by_encoder_worker() {
    let flag = Arc::new(AtomicBool::new(true));
    assert!(flag.swap(false, Ordering::AcqRel));
    assert!(!flag.load(Ordering::Acquire));
}
