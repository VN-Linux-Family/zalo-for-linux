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
fn pipewire_capture_setup_and_teardown_can_be_repeated() {
    use super::{CaptureData, PipeWireCaptureGuard};
    use crate::screen::LatestFrameQueue;
    use std::fs::File;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let data = CaptureData {
        raw_frames: Arc::new(LatestFrameQueue::default()),
        raw_frame_seen: Arc::new(AtomicBool::new(false)),
    };

    for _ in 0..64 {
        let remote_fd = File::open("/dev/null").unwrap().into();
        if let Ok(mut capture) = PipeWireCaptureGuard::new(remote_fd, 1, &data) {
            let _ = capture.iterate(0);
            capture.disconnect();
        }
    }
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
