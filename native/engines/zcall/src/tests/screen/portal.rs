use super::portal_source_types;
use ashpd::desktop::screencast::SourceType;

#[test]
fn pipewire_failure_is_reported_before_worker_stop_suppresses_duplicates() {
    use super::report_capture_failure;
    use crate::screen::report_worker_result;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::sync_channel;

    let (status_tx, status_rx) = sync_channel(2);
    let stop = AtomicBool::new(false);
    let result = Err("PipeWire screen source disappeared".to_string());

    report_capture_failure(&result, &stop, &status_tx);
    stop.store(true, Ordering::Release);
    report_worker_result(result, &stop, &status_tx);

    assert_eq!(
        status_rx.try_recv().unwrap().unwrap_err(),
        "PipeWire screen source disappeared"
    );
    assert!(status_rx.try_recv().is_err());
}

#[test]
fn portal_source_selection_restricts_monitor_and_window_choices() {
    assert_eq!(
        portal_source_types(Some("monitor")).unwrap(),
        SourceType::MONITOR
    );
    assert_eq!(
        portal_source_types(Some("window")).unwrap(),
        SourceType::WINDOW
    );
    assert_eq!(
        portal_source_types(None).unwrap(),
        SourceType::MONITOR | SourceType::WINDOW
    );
    assert!(portal_source_types(Some("unknown")).is_err());
}

#[test]
#[ignore = "requires an interactive XDG ScreenCast portal source selection"]
fn captures_a_real_frame_through_the_desktop_portal_and_pipewire() {
    use super::{capture_worker, ScreenEncodeConfig};
    use crate::screen::EncodedScreenFrame;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    let stop = Arc::new(AtomicBool::new(false));
    let force_keyframe = Arc::new(AtomicBool::new(false));
    let target_fps = Arc::new(AtomicU32::new(15));
    let capture_fps = Arc::new(AtomicU32::new(15));
    let (frame_tx, frame_rx) = sync_channel::<EncodedScreenFrame>(2);
    let (status_tx, status_rx) = sync_channel(2);
    let worker_stop = stop.clone();
    let worker_status = status_tx.clone();

    let worker = thread::spawn(move || {
        capture_worker(
            &worker_stop,
            frame_tx,
            &worker_status,
            None,
            ScreenEncodeConfig {
                bitrate: 1_000_000,
                max_short: 0,
                force_keyframe,
                target_fps,
                capture_fps,
            },
        )
    });

    let deadline = Instant::now() + Duration::from_secs(180);
    let mut captured = None;
    let mut failure = None;
    while Instant::now() < deadline && !worker.is_finished() {
        if let Ok(frame) = frame_rx.recv_timeout(Duration::from_millis(100)) {
            captured = Some((frame.width, frame.height, frame.data.len()));
            break;
        }
        if let Ok(Err(error)) = status_rx.try_recv() {
            failure = Some(error);
            break;
        }
    }

    stop.store(true, Ordering::Release);
    let worker_result = worker.join().expect("portal capture worker must not panic");
    assert!(
        worker_result.is_ok(),
        "portal capture worker failed: {worker_result:?}"
    );
    assert!(
        failure.is_none(),
        "portal capture reported an error before producing a frame: {failure:?}"
    );
    let (width, height, bytes) = captured.expect("portal capture must produce an encoded frame");
    assert!(width >= 2 && height >= 2 && bytes > 0);
}
