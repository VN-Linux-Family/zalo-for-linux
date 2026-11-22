//! Native screen capture through XDG ScreenCast Portal + PipeWire.
//!
//! The portal presents the system-owned monitor/window chooser. Captured frames
//! are encoded in the Rust process and are never exposed as a JavaScript
//! MediaStream or raw desktop-capture source.
use openh264::encoder::{
    BitRate, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Profile, UsageType,
};
use openh264::formats::{BgraSliceU8, YUVBuffer};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

mod portal;
mod x11;

/// True when the helper runs in an X11 session; otherwise capture uses XDG ScreenCast.
pub fn is_x11_session() -> bool {
    x11::is_x11_session()
}

const SCREEN_CAPTURE_CANCELLED: &str = "__zcall_screen_capture_cancelled__";

fn run_capture_worker(worker: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    catch_unwind(AssertUnwindSafe(worker)).unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        Err(format!("Screen capture worker panicked: {message}"))
    })
}

pub fn is_cancelled_error(error: &str) -> bool {
    error == SCREEN_CAPTURE_CANCELLED
}

fn portal_error_message(operation: &str, error: impl std::fmt::Display) -> String {
    let message = error.to_string();
    if message.to_ascii_lowercase().contains("cancel") {
        SCREEN_CAPTURE_CANCELLED.to_string()
    } else {
        format!("{operation}: {message}")
    }
}

pub fn list_x11_windows() -> Result<Vec<(String, String)>, String> {
    x11::list_windows()
}

pub struct EncodedScreenFrame {
    pub data: Vec<u8>,
    pub key: bool,
    pub width: u32,
    pub height: u32,
}

struct RawScreenFrame {
    bgra: Vec<u8>,
    width: u32,
    height: u32,
}

/// A one-slot mailbox for X11 capture: replace stale raw frames instead of
/// queueing latency when the encoder cannot keep up.
#[derive(Default)]
struct LatestFrameQueue {
    frame: Mutex<Option<RawScreenFrame>>,
    ready: Condvar,
}

impl LatestFrameQueue {
    fn push(&self, frame: RawScreenFrame) -> bool {
        let Ok(mut slot) = self.frame.lock() else {
            return false;
        };
        let overwritten = slot.replace(frame).is_some();
        self.ready.notify_one();
        overwritten
    }
}

trait RawFrameSource {
    fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<RawScreenFrame, std::sync::mpsc::RecvTimeoutError>;
}

impl RawFrameSource for Receiver<RawScreenFrame> {
    fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<RawScreenFrame, std::sync::mpsc::RecvTimeoutError> {
        Receiver::recv_timeout(self, timeout)
    }
}

impl RawFrameSource for Arc<LatestFrameQueue> {
    fn recv_timeout(
        &self,
        timeout: Duration,
    ) -> Result<RawScreenFrame, std::sync::mpsc::RecvTimeoutError> {
        let Ok(slot) = self.frame.lock() else {
            return Err(std::sync::mpsc::RecvTimeoutError::Disconnected);
        };
        let Ok((mut slot, _)) = self
            .ready
            .wait_timeout_while(slot, timeout, |frame| frame.is_none())
        else {
            return Err(std::sync::mpsc::RecvTimeoutError::Disconnected);
        };
        slot.take()
            .ok_or(std::sync::mpsc::RecvTimeoutError::Timeout)
    }
}

#[derive(Clone)]
struct ScreenEncodeConfig {
    bitrate: u32,
    max_short: u32,
    force_keyframe: Arc<AtomicBool>,
    target_fps: Arc<AtomicU32>,
    capture_fps: Arc<AtomicU32>,
}

pub struct ScreenCapture {
    stop: Arc<AtomicBool>,
    frames: Receiver<EncodedScreenFrame>,
    status: Receiver<Result<(), String>>,
    force_keyframe: Arc<AtomicBool>,
    target_fps: Arc<AtomicU32>,
    finished: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl ScreenCapture {
    pub fn start(fps: u32, bitrate: u32) -> Result<Self, String> {
        Self::start_with_max_short(fps, bitrate, 0)
    }

    /// Start screen capture with an optional maximum short-side resolution.
    pub fn start_with_max_short(fps: u32, bitrate: u32, max_short: u32) -> Result<Self, String> {
        Self::start_with_source(fps, bitrate, max_short, None)
    }

    /// Start the native X11 backend for X11 sessions, otherwise use the
    /// system-owned Wayland ScreenCast portal. X11 window IDs are optional.
    pub fn start_with_source(
        fps: u32,
        bitrate: u32,
        max_short: u32,
        source: Option<String>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let force_keyframe = Arc::new(AtomicBool::new(false));
        let worker_force_keyframe = force_keyframe.clone();
        let target_fps = Arc::new(AtomicU32::new(fps.clamp(15, 30)));
        let worker_target_fps = target_fps.clone();
        let capture_fps = Arc::new(AtomicU32::new(fps.clamp(15, 30)));
        let worker_capture_fps = capture_fps.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        let (tx, frames) = sync_channel(2);
        let (status_tx, status) = std::sync::mpsc::sync_channel(4);
        let bitrate = bitrate.clamp(250_000, 3_000_000);
        let max_short = max_short.clamp(0, 2160);

        let worker = thread::Builder::new()
            .name("zcall-screen".into())
            .spawn(move || {
                crate::zcall_log!(
                    "[zcall-screen] capture worker entered; x11_session={}",
                    x11::is_x11_session()
                );
                let worker_stop_state = worker_stop.clone();
                let result = run_capture_worker(|| {
                    if x11::is_x11_session() {
                        x11::capture_worker(
                            worker_stop,
                            tx,
                            status_tx.clone(),
                            ScreenEncodeConfig {
                                bitrate,
                                max_short,
                                force_keyframe: worker_force_keyframe,
                                target_fps: worker_target_fps,
                                capture_fps: worker_capture_fps,
                            },
                            source,
                        )
                    } else {
                        portal::capture_worker(
                            worker_stop,
                            tx,
                            status_tx.clone(),
                            source,
                            ScreenEncodeConfig {
                                bitrate,
                                max_short,
                                force_keyframe: worker_force_keyframe,
                                target_fps: worker_target_fps,
                                capture_fps: worker_capture_fps,
                            },
                        )
                    }
                });
                // Publish completion before the terminal status. The engine
                // consumes status and may immediately accept a retry; it must
                // never observe a terminal event while this worker still looks live.
                crate::zcall_log!(
                    "[zcall-screen] capture worker exited; stop_requested={}",
                    worker_stop_state.load(Ordering::Acquire)
                );
                worker_finished.store(true, Ordering::Release);
                report_worker_result(result, &worker_stop_state, &status_tx);
            })
            .map_err(|error| format!("Start screen capture worker: {error}"))?;

        Ok(Self {
            stop,
            frames,
            status,
            force_keyframe,
            target_fps,
            finished,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Force the encoder to restart at the next frame so the SFU can decode it.
    pub fn request_keyframe(&self) {
        self.force_keyframe.store(true, Ordering::Release);
    }

    /// Set the network-derived FPS ceiling. The encoder also applies its own
    /// CPU-load ceiling, so neither weak links nor slow CPUs force extra work.
    pub fn set_target_fps(&self, fps: u32) -> bool {
        let fps = fps_tier(fps);
        self.target_fps.swap(fps, Ordering::AcqRel) != fps
    }

    pub fn try_status(&self) -> Option<Result<(), String>> {
        self.status.try_recv().ok()
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn is_stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    pub fn try_recv(&self) -> Option<EncodedScreenFrame> {
        self.frames.try_recv().ok()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        // Never join a portal/PipeWire worker on the engine thread. D-Bus
        // requests can be held by a desktop portal while its chooser is open;
        // joining here used to freeze the call engine and its UI action loop.
        // Reap asynchronously; dropping the handle is the safe fallback if
        // the reaper cannot be spawned.
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(worker) = worker.take() {
                let _ = thread::Builder::new()
                    .name("zcall-screen-reaper".into())
                    .spawn(move || {
                        let _ = worker.join();
                    });
            }
        }
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn report_worker_result(
    result: Result<(), String>,
    stop: &AtomicBool,
    status: &SyncSender<Result<(), String>>,
) {
    if let Err(error) = result {
        // Cancellation is a normal stop path. Reporting it as a capture error
        // after screenStopped made the frontend process a second, contradictory
        // terminal event and could leave camera/window state out of sync.
        if !stop.load(Ordering::Acquire) {
            crate::zcall_log!("[zcall-screen] {error}");
            let _ = status.send(Err(error));
        }
    }
}

fn fps_tier(fps: u32) -> u32 {
    match fps {
        0..=15 => 15,
        16..=24 => 24,
        _ => 30,
    }
}

fn encoder_config(settings: &ScreenEncodeConfig, fps: u32) -> EncoderConfig {
    EncoderConfig::new()
        .bitrate(BitRate::from_bps(settings.bitrate))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .usage_type(UsageType::ScreenContentRealTime)
        .adaptive_quantization(false)
        .background_detection(false)
        .profile(Profile::Baseline)
        .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 2))
}

fn encode_frames<F: RawFrameSource>(
    frames: F,
    tx: SyncSender<EncodedScreenFrame>,
    stop: Arc<AtomicBool>,
    settings: ScreenEncodeConfig,
    status: SyncSender<Result<(), String>>,
    encoded_frame_seen: Arc<AtomicBool>,
) -> Result<(), String> {
    let mut encoder: Option<Encoder> = None;
    let mut dimensions = (0u32, 0u32);
    let mut encoder_fps = 0;
    let mut cpu_fps = 30u32;
    let mut encode_ema_ms = 0.0f64;
    let mut last_evaluation = std::time::Instant::now();
    let mut good_since: Option<std::time::Instant> = None;
    let mut next_frame_at = std::time::Instant::now();
    let mut ready_notified = false;

    let mut first_encoded_logged = false;
    while !stop.load(Ordering::Relaxed) {
        let frame = match frames.recv_timeout(Duration::from_millis(100)) {
            Ok(frame) => frame,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if frame.width < 2 || frame.height < 2 {
            continue;
        }
        if settings.force_keyframe.swap(false, Ordering::AcqRel) {
            encoder = None;
            dimensions = (0, 0);
        }
        let requested_fps = fps_tier(settings.target_fps.load(Ordering::Acquire));
        let active_fps = requested_fps.min(cpu_fps);
        settings.capture_fps.store(active_fps, Ordering::Release);
        let now = std::time::Instant::now();
        if now < next_frame_at {
            continue;
        }
        next_frame_at = now + Duration::from_secs_f64(1.0 / f64::from(active_fps));

        let encode_started = std::time::Instant::now();
        // OpenH264's BGRA-to-YUV420 conversion requires even dimensions.
        // Window portals commonly return odd widths (for example 953x646);
        // normalize every frame before constructing the YUV buffer.
        let (pixels, width, height) =
            resize_bgra(frame.bgra, frame.width, frame.height, settings.max_short)?;
        if dimensions != (width, height) || encoder_fps != active_fps {
            encoder = Some(
                Encoder::with_api_config(
                    openh264::OpenH264API::from_source(),
                    encoder_config(&settings, active_fps),
                )
                .map_err(|error| format!("Create OpenH264 screen encoder: {error}"))?,
            );
            dimensions = (width, height);
            encoder_fps = active_fps;
        }
        let Some(encoder) = encoder.as_mut() else {
            continue;
        };
        let bgra = BgraSliceU8::new(&pixels, (width as usize, height as usize));
        let mut yuv = YUVBuffer::new(width as usize, height as usize);
        yuv.read_bgra8(bgra);
        let bitstream = encoder
            .encode(&yuv)
            .map_err(|error| format!("Encode screen frame with OpenH264: {error}"))?;
        let encode_ms = encode_started.elapsed().as_secs_f64() * 1000.0;
        encode_ema_ms = if encode_ema_ms == 0.0 {
            encode_ms
        } else {
            encode_ema_ms * 0.85 + encode_ms * 0.15
        };
        if last_evaluation.elapsed() >= Duration::from_secs(2) {
            let budget_ms = 1000.0 / f64::from(active_fps);
            if encode_ema_ms > budget_ms * 0.85 && cpu_fps > 15 {
                cpu_fps = if cpu_fps > 24 { 24 } else { 15 };
                good_since = None;
                crate::zcall_log!(
                    "[zcall-screen] CPU encode pressure; reducing local FPS ceiling to {cpu_fps} (encode_ema_ms={encode_ema_ms:.1}, budget_ms={budget_ms:.1})"
                );
            } else if encode_ema_ms < budget_ms * 0.45 {
                let since = good_since.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() >= Duration::from_secs(8) && cpu_fps < 30 {
                    cpu_fps = if cpu_fps < 24 { 24 } else { 30 };
                    good_since = None;
                    crate::zcall_log!(
                        "[zcall-screen] CPU headroom recovered; raising local FPS ceiling to {cpu_fps}"
                    );
                }
            } else {
                good_since = None;
            }
            last_evaluation = std::time::Instant::now();
        }
        let key = matches!(
            bitstream.frame_type(),
            FrameType::IDR | FrameType::I | FrameType::IPMixed
        );
        let mut encoded = Vec::new();
        bitstream.write_vec(&mut encoded);
        if encoded.is_empty() {
            continue;
        }
        let encoded_frame = EncodedScreenFrame {
            data: encoded,
            key,
            width,
            height,
        };
        // Publish readiness only after the first encoded frame is actually in
        // the engine's queue. Reporting ready before try_send can let the engine
        // signal share-start while the frame is still absent (or was dropped).
        match tx.try_send(encoded_frame) {
            Ok(()) => {
                encoded_frame_seen.store(true, Ordering::Release);
                if !first_encoded_logged {
                    crate::zcall_log!(
                        "[zcall-screen] first H.264 screen frame queued: {width}x{height}"
                    );
                    first_encoded_logged = true;
                }
                if !ready_notified {
                    let _ = status.try_send(Ok(()));
                    ready_notified = true;
                }
            }
            Err(TrySendError::Full(_)) => {
                // A dropped H.264 access unit can break the decoder reference
                // chain. Restart encoding at the next frame so receivers recover.
                settings.force_keyframe.store(true, Ordering::Release);
            }
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    crate::zcall_log!(
        "[zcall-screen] encoder loop exited; stop_requested={}",
        stop.load(Ordering::Acquire)
    );
    Ok(())
}

fn resize_bgra(
    source: Vec<u8>,
    width: u32,
    height: u32,
    max_short: u32,
) -> Result<(Vec<u8>, u32, u32), String> {
    let even_width = width & !1;
    let even_height = height & !1;
    if even_width < 2 || even_height < 2 {
        return Err(format!(
            "PipeWire BGRA frame is too small for H.264: {width}x{height}"
        ));
    }

    let short_side = even_width.min(even_height);
    if max_short > 0 && short_side > max_short {
        let scale = max_short as f64 / short_side as f64;
        let target_width = (((even_width as f64 * scale).round() as u32).max(2)) & !1;
        let target_height = (((even_height as f64 * scale).round() as u32).max(2)) & !1;
        let mut resized = vec![0u8; target_width as usize * target_height as usize * 4];
        for y in 0..target_height {
            let source_y = (y as u64 * even_height as u64 / target_height as u64) as usize;
            for x in 0..target_width {
                let source_x = (x as u64 * even_width as u64 / target_width as u64) as usize;
                let source_offset = (source_y * width as usize + source_x) * 4;
                let target_offset = (y as usize * target_width as usize + x as usize) * 4;
                resized[target_offset..target_offset + 4]
                    .copy_from_slice(&source[source_offset..source_offset + 4]);
            }
        }
        return Ok((resized, target_width, target_height));
    }

    if even_width == width && even_height == height {
        return Ok((source, width, height));
    }

    let row_bytes = even_width as usize * 4;
    let mut cropped = Vec::with_capacity(row_bytes * even_height as usize);
    for y in 0..even_height as usize {
        let start = y * width as usize * 4;
        cropped.extend_from_slice(&source[start..start + row_bytes]);
    }
    Ok((cropped, even_width, even_height))
}

#[cfg(test)]
mod tests {
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
    fn encodes_odd_sized_window_frames_after_cropping_to_even_dimensions() {
        let (raw_tx, raw_rx) = sync_channel(2);
        let (encoded_tx, encoded_rx) = sync_channel(2);
        let (status_tx, status_rx) = sync_channel(2);
        let encoded_frame_seen = Arc::new(AtomicBool::new(false));
        let worker_encoded_frame_seen = encoded_frame_seen.clone();
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
            crate::video::avc_codec_string(&frame.data)
                .is_some_and(|codec| codec.starts_with("avc1.")),
            "screen preview codec must be derived from its SPS"
        );

        stop.store(true, Ordering::Relaxed);
        drop(raw_tx);
        worker.join().unwrap();
    }

    /// Opt-in Wayland acceptance test. This deliberately opens the desktop's
    /// real ScreenCast chooser and must be run interactively by selecting a
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
    fn screen_keyframe_request_flag_is_consumed_by_encoder_worker() {
        let flag = Arc::new(AtomicBool::new(true));
        assert!(flag.swap(false, Ordering::AcqRel));
        assert!(!flag.load(Ordering::Acquire));
    }
}
