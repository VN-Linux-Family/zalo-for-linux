//! Native screen capture through XDG `ScreenCast` Portal + `PipeWire`.
//!
//! The portal presents the system-owned monitor/window chooser. Captured frames
//! are encoded in the Rust process and are never exposed as a JavaScript
//! `MediaStream` or raw desktop-capture source.
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

/// True when the helper runs in an X11 session; otherwise capture uses XDG `ScreenCast`.
#[must_use]
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

#[must_use]
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
        Self::recv_timeout(self, timeout)
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
    /// system-owned Wayland `ScreenCast` portal. X11 window IDs are optional.
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
        let worker_capture_fps = capture_fps;
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
                            &worker_stop,
                            tx,
                            &status_tx,
                            source.as_deref(),
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

const fn fps_tier(fps: u32) -> u32 {
    match fps {
        0..=15 => 15,
        16..=24 => 24,
        _ => 30,
    }
}

fn encoder_config(settings: &ScreenEncodeConfig, fps: u32) -> EncoderConfig {
    EncoderConfig::new()
        .bitrate(BitRate::from_bps(settings.bitrate))
        .max_frame_rate(FrameRate::from_hz(f32::from(
            u8::try_from(fps).unwrap_or(30),
        )))
        .usage_type(UsageType::ScreenContentRealTime)
        .adaptive_quantization(false)
        .background_detection(false)
        .profile(Profile::Baseline)
        .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 2))
}

fn update_ema(previous: f64, sample: f64) -> f64 {
    if previous == 0.0 {
        sample
    } else {
        sample.mul_add(0.15, previous * 0.85)
    }
}

fn update_cpu_fps(
    cpu_fps: &mut u32,
    good_since: &mut Option<std::time::Instant>,
    encode_ema_ms: f64,
    active_fps: u32,
) {
    let budget_ms = 1000.0 / f64::from(active_fps);
    if encode_ema_ms > budget_ms * 0.85 && *cpu_fps > 15 {
        *cpu_fps = if *cpu_fps > 24 { 24 } else { 15 };
        *good_since = None;
        crate::zcall_log!(
            "[zcall-screen] CPU encode pressure; reducing local FPS ceiling to {} (encode_ema_ms={encode_ema_ms:.1}, budget_ms={budget_ms:.1})",
            *cpu_fps
        );
    } else if encode_ema_ms < budget_ms * 0.45 {
        let since = good_since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() >= Duration::from_secs(8) && *cpu_fps < 30 {
            *cpu_fps = if *cpu_fps < 24 { 24 } else { 30 };
            *good_since = None;
            crate::zcall_log!(
                "[zcall-screen] CPU headroom recovered; raising local FPS ceiling to {}",
                *cpu_fps
            );
        }
    } else {
        *good_since = None;
    }
}

fn encode_screen_frame(
    encoder: &mut Encoder,
    yuv: &mut YUVBuffer,
    pixels: &[u8],
    width: u32,
    height: u32,
    started: std::time::Instant,
) -> Result<Option<(EncodedScreenFrame, f64, f64, f64)>, String> {
    let bgra = BgraSliceU8::new(pixels, (width as usize, height as usize));
    yuv.read_bgra8(bgra);
    let convert_ms = started.elapsed().as_secs_f64() * 1000.0;
    let h264_started = std::time::Instant::now();
    let bitstream = encoder
        .encode(yuv)
        .map_err(|error| format!("Encode screen frame with OpenH264: {error}"))?;
    let h264_ms = h264_started.elapsed().as_secs_f64() * 1000.0;
    let encode_ms = started.elapsed().as_secs_f64() * 1000.0;
    let key = matches!(
        bitstream.frame_type(),
        FrameType::IDR | FrameType::I | FrameType::IPMixed
    );
    let mut data = Vec::new();
    bitstream.write_vec(&mut data);
    if data.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        EncodedScreenFrame {
            data,
            key,
            width,
            height,
        },
        convert_ms,
        h264_ms,
        encode_ms,
    )))
}

fn encode_frames<F: RawFrameSource>(
    frames: &F,
    tx: &SyncSender<EncodedScreenFrame>,
    stop: &Arc<AtomicBool>,
    settings: &ScreenEncodeConfig,
    status: &SyncSender<Result<(), String>>,
    encoded_frame_seen: &Arc<AtomicBool>,
) -> Result<(), String> {
    let mut encoder: Option<Encoder> = None;
    let mut yuv_buffer: Option<YUVBuffer> = None;
    let mut dimensions = (0u32, 0u32);
    let mut encoder_fps = 0;
    let mut cpu_fps = 30u32;
    let mut encode_ema_ms = 0.0f64;
    let mut convert_ema_ms = 0.0f64;
    let mut h264_ema_ms = 0.0f64;
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
        if dimensions != (width, height) {
            // Reuse the YUV allocation across frames. Creating a full-size
            // 4:2:0 buffer for every desktop frame adds allocator churn at 30 FPS.
            yuv_buffer = Some(YUVBuffer::new(width as usize, height as usize));
        }
        if dimensions != (width, height) || encoder_fps != active_fps {
            encoder = Some(
                Encoder::with_api_config(
                    openh264::OpenH264API::from_source(),
                    encoder_config(settings, active_fps),
                )
                .map_err(|error| format!("Create OpenH264 screen encoder: {error}"))?,
            );
            dimensions = (width, height);
            encoder_fps = active_fps;
        }
        let Some(encoder) = encoder.as_mut() else {
            continue;
        };
        let Some(yuv) = yuv_buffer.as_mut() else {
            continue;
        };
        let Some((encoded_frame, convert_ms, h264_ms, encode_ms)) =
            encode_screen_frame(encoder, yuv, &pixels, width, height, encode_started)?
        else {
            continue;
        };
        convert_ema_ms = update_ema(convert_ema_ms, convert_ms);
        h264_ema_ms = update_ema(h264_ema_ms, h264_ms);
        encode_ema_ms = update_ema(encode_ema_ms, encode_ms);
        if last_evaluation.elapsed() >= Duration::from_secs(2) {
            update_cpu_fps(&mut cpu_fps, &mut good_since, encode_ema_ms, active_fps);
            last_evaluation = std::time::Instant::now();
        }
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
    mut source: Vec<u8>,
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
        let scale = f64::from(max_short) / f64::from(short_side);
        let target_width = (((f64::from(even_width) * scale).round() as u32).max(2)) & !1;
        let target_height = (((f64::from(even_height) * scale).round() as u32).max(2)) & !1;
        for y in 0..target_height {
            let source_y =
                (u64::from(y) * u64::from(even_height) / u64::from(target_height)) as usize;
            for x in 0..target_width {
                let source_x =
                    (u64::from(x) * u64::from(even_width) / u64::from(target_width)) as usize;
                let source_offset = (source_y * width as usize + source_x) * 4;
                let target_offset = (y as usize * target_width as usize + x as usize) * 4;
                source.copy_within(source_offset..source_offset + 4, target_offset);
            }
        }
        source.truncate(target_width as usize * target_height as usize * 4);
        return Ok((source, target_width, target_height));
    }

    if even_width == width && even_height == height {
        return Ok((source, width, height));
    }

    let row_bytes = even_width as usize * 4;
    for y in 0..even_height as usize {
        let start = y * width as usize * 4;
        let target = y * row_bytes;
        source.copy_within(start..start + row_bytes, target);
    }
    source.truncate(row_bytes * even_height as usize);
    Ok((source, even_width, even_height))
}

#[cfg(test)]
#[path = "../tests/screen/mod.rs"]
mod tests;
