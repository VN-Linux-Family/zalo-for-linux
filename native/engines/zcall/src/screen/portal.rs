//! Wayland XDG `ScreenCast` portal and `PipeWire` capture implementation.
use super::{
    encode_frames, portal_error_message, EncodedScreenFrame, LatestFrameQueue, RawScreenFrame,
    ScreenEncodeConfig,
};
use futures_util::future::{select, Either};
use std::ffi::{c_char, c_int, c_void};
use std::future::Future;
use std::os::fd::{IntoRawFd, OwnedFd};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ashpd::desktop::screencast::{
    CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
};
use ashpd::desktop::PersistMode;

struct CaptureData {
    raw_frames: Arc<LatestFrameQueue>,
    raw_frame_seen: Arc<AtomicBool>,
}

#[repr(C)]
struct PipeWireCapture {
    _private: [u8; 0],
}

type PipeWireFrameFn = Option<extern "C" fn(*mut c_void, *const u8, u32, u32, u32, i32, u32, u32)>;

// These safe declarations are private to this module. `PipeWireCaptureGuard`
// passes only live handles and bounded output buffers; the callback userdata
// remains allocated until the capture is freed.
unsafe extern "C" {
    safe fn zcall_pw_capture_new(
        remote_fd: c_int,
        node_id: u32,
        on_frame: PipeWireFrameFn,
        userdata: *mut c_void,
        error: *mut c_char,
        error_size: usize,
    ) -> *mut PipeWireCapture;
    safe fn zcall_pw_capture_node_seen(capture: *const PipeWireCapture) -> c_int;
    safe fn zcall_pw_capture_connect(capture: *mut PipeWireCapture) -> c_int;
    safe fn zcall_pw_capture_streaming(capture: *const PipeWireCapture) -> c_int;
    safe fn zcall_pw_capture_iterate(capture: *mut PipeWireCapture, timeout_ms: c_int) -> c_int;
    safe fn zcall_pw_capture_error(
        capture: *const PipeWireCapture,
        output: *mut c_char,
        output_size: usize,
    );
    safe fn zcall_pw_capture_disconnect(capture: *mut PipeWireCapture);
    safe fn zcall_pw_capture_free(capture: *mut PipeWireCapture);
}

fn report_capture_failure(
    result: &Result<(), String>,
    stop: &AtomicBool,
    status: &SyncSender<Result<(), String>>,
) {
    if let Err(error) = result {
        if !stop.load(Ordering::Acquire) {
            crate::zcall_log!(
                "[zcall-screen] PipeWire capture failed; reporting terminal status: {error}"
            );
            let _ = status.try_send(Err(error.clone()));
        }
    }
}

fn c_buffer_string(buffer: &[c_char]) -> String {
    let bytes = buffer
        .iter()
        .take_while(|&&byte| byte != 0)
        .map(|&byte| byte as u8)
        .collect::<Vec<_>>();
    String::from_utf8_lossy(&bytes).into_owned()
}

struct PipeWireCaptureGuard(NonNull<PipeWireCapture>);

impl PipeWireCaptureGuard {
    fn new(remote_fd: OwnedFd, node_id: u32, userdata: &CaptureData) -> Result<Self, String> {
        let mut error = [0 as c_char; 512];
        let capture = zcall_pw_capture_new(
            remote_fd.into_raw_fd(),
            node_id,
            Some(pipewire_frame),
            std::ptr::from_ref::<CaptureData>(userdata)
                .cast_mut()
                .cast(),
            error.as_mut_ptr(),
            error.len(),
        );
        let Some(capture) = NonNull::new(capture) else {
            let detail = c_buffer_string(&error);
            return Err(if detail.is_empty() {
                "Create PipeWire capture failed".into()
            } else {
                detail
            });
        };
        Ok(Self(capture))
    }

    fn node_seen(&self) -> bool {
        zcall_pw_capture_node_seen(self.0.as_ptr()) != 0
    }

    fn streaming(&self) -> bool {
        zcall_pw_capture_streaming(self.0.as_ptr()) != 0
    }

    fn connect(&mut self) -> Result<(), String> {
        let result = zcall_pw_capture_connect(self.0.as_ptr());
        if result < 0 {
            Err(self
                .error()
                .unwrap_or_else(|| format!("Connect PipeWire capture failed: {result}")))
        } else {
            Ok(())
        }
    }

    fn iterate(&mut self, timeout_ms: c_int) -> Result<(), String> {
        let result = zcall_pw_capture_iterate(self.0.as_ptr(), timeout_ms);
        if result < 0 {
            Err(format!("PipeWire main loop iteration failed: {result}"))
        } else {
            Ok(())
        }
    }

    fn error(&self) -> Option<String> {
        let mut error = [0 as c_char; 512];
        zcall_pw_capture_error(self.0.as_ptr(), error.as_mut_ptr(), error.len());
        let error = c_buffer_string(&error);
        (!error.is_empty()).then_some(error)
    }

    fn disconnect(&mut self) {
        zcall_pw_capture_disconnect(self.0.as_ptr());
    }
}

impl Drop for PipeWireCaptureGuard {
    fn drop(&mut self) {
        zcall_pw_capture_free(self.0.as_ptr());
    }
}

struct PipeWireFrame {
    userdata: *mut c_void,
    mapped: *const u8,
    max_size: u32,
    offset: u32,
    size: u32,
    stride: i32,
    width: u32,
    height: u32,
}

extern "C" fn pipewire_frame(
    userdata: *mut c_void,
    mapped: *const u8,
    max_size: u32,
    offset: u32,
    size: u32,
    stride: i32,
    width: u32,
    height: u32,
) {
    let frame = PipeWireFrame {
        userdata,
        mapped,
        max_size,
        offset,
        size,
        stride,
        width,
        height,
    };
    let _ = catch_unwind(AssertUnwindSafe(|| pipewire_frame_inner(frame)));
}

fn pipewire_frame_inner(frame: PipeWireFrame) {
    let PipeWireFrame {
        userdata,
        mapped,
        max_size,
        offset,
        size,
        stride,
        width,
        height,
    } = frame;
    let width = width as usize;
    let height = height as usize;
    let offset = offset as usize;
    let size = size as usize;
    let stride = stride.unsigned_abs() as usize;
    if userdata.is_null()
        || mapped.is_null()
        || width < 2
        || height < 2
        || width > 4096
        || height > 4096
        || stride < width * 4
    {
        return;
    }
    let Some(end) = offset.checked_add(size) else {
        return;
    };
    if end > max_size as usize || size < stride.saturating_mul(height) {
        return;
    }

    // SAFETY: PipeWire invokes this callback synchronously with a valid mapped
    // buffer for `max_size` bytes; userdata points to the live CaptureData box.
    // The mapped buffer is copied before this callback returns.
    let (data, mapped) = unsafe {
        (
            &*userdata.cast::<CaptureData>(),
            std::slice::from_raw_parts(mapped, max_size as usize),
        )
    };
    let source = &mapped[offset..end];
    let mut compact = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let begin = row * stride;
        let Some(line_end) = begin.checked_add(width * 4) else {
            return;
        };
        if line_end > source.len() {
            return;
        }
        compact.extend_from_slice(&source[begin..line_end]);
    }

    let frame = RawScreenFrame {
        bgra: compact,
        width: width as u32,
        height: height as u32,
    };
    data.raw_frames.push(frame);
    if !data.raw_frame_seen.swap(true, Ordering::AcqRel) {
        crate::zcall_log!(
            "[zcall-screen] first raw PipeWire frame received: {width}x{height}, stride={stride}, bytes={size}"
        );
    }
}

fn portal_source_types(source: Option<&str>) -> Result<SourceType, String> {
    match source {
        Some("monitor" | "screen" | "screen:0") => Ok(SourceType::MONITOR),
        Some("window" | "window:select") => Ok(SourceType::WINDOW),
        Some(other) => Err(format!("Unsupported Wayland screen source: {other}")),
        None => Ok(SourceType::MONITOR | SourceType::WINDOW),
    }
}

pub(super) fn capture_worker(
    stop: Arc<AtomicBool>,
    tx: SyncSender<EncodedScreenFrame>,
    status: SyncSender<Result<(), String>>,
    source: Option<String>,
    settings: ScreenEncodeConfig,
) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("Create portal runtime: {error}"))?;

    // ashpd 0.14 caches its first session-bus connection in a process-wide
    // OnceLock. A per-capture Tokio runtime cannot own that cached connection:
    // shutting the runtime down after the first share leaves ashpd returning a
    // clone whose background driver no longer runs. The next Screencast::new()
    // then hangs while constructing its proxy. Own a fresh zbus connection for
    // this capture and pass it explicitly so it is never stored in ashpd's
    // process-global connection cache.
    let (session, connection, remote_fd, node_id) = runtime.block_on(async {
        crate::zcall_log!("[zcall-screen] requesting XDG ScreenCast portal");
        crate::zcall_log!("[zcall-screen] opening dedicated session-bus connection");
        let connection = portal_request(
            ashpd::zbus::Connection::session(),
            stop.clone(),
            Duration::from_secs(10),
            "connecting to the session D-Bus",
        )
        .await?;
        crate::zcall_log!("[zcall-screen] session-bus connection established");
        let portal = portal_request(
            Screencast::with_connection(connection.clone()),
            stop.clone(),
            Duration::from_secs(10),
            "connecting to the ScreenCast portal",
        )
        .await?;
        crate::zcall_log!("[zcall-screen] portal connected; creating session");
        let session = portal_request(
            portal.create_session(Default::default()),
            stop.clone(),
            Duration::from_secs(10),
            "creating the ScreenCast session",
        )
        .await?;

        // Every request after CreateSession is enclosed so any denial,
        // timeout, cancellation or PipeWire setup failure closes the portal
        // session before a later share attempt can start.
        let prepared = async {
            crate::zcall_log!("[zcall-screen] session created; opening source chooser");
            let source_types = portal_source_types(source.as_deref())?;
            let selected = portal_request(
                portal.select_sources(
                    &session,
                    SelectSourcesOptions::default()
                        .set_cursor_mode(CursorMode::EMBEDDED)
                        .set_sources(source_types)
                        .set_multiple(false)
                        .set_persist_mode(PersistMode::DoNot),
                ),
                stop.clone(),
                Duration::from_secs(120),
                "waiting for the ScreenCast source chooser",
            )
            .await?;
            crate::zcall_log!("[zcall-screen] source chooser returned; starting portal stream");
            selected
                .response()
                .map_err(|error| portal_error_message("ScreenCast source selection", error))?;

            let started = portal_request(
                portal.start(&session, None, StartCastOptions::default()),
                stop.clone(),
                Duration::from_secs(30),
                "starting the ScreenCast portal stream",
            )
            .await?
            .response()
            .map_err(|error| portal_error_message("ScreenCast start", error))?;
            crate::zcall_log!("[zcall-screen] portal stream started; opening PipeWire remote");
            let stream = started
                .streams()
                .first()
                .ok_or_else(|| "ScreenCast portal returned no PipeWire stream".to_string())?;
            let node_id = stream.pipe_wire_node_id();
            let remote_fd = portal_request(
                portal.open_pipe_wire_remote(&session, Default::default()),
                stop.clone(),
                Duration::from_secs(10),
                "opening the portal PipeWire connection",
            )
            .await?;
            Ok::<_, String>((remote_fd, node_id))
        }
        .await;

        if prepared.is_err() {
            let _ = tokio::time::timeout(Duration::from_secs(2), session.close()).await;
        }
        let (remote_fd, node_id) = prepared?;
        Ok::<_, String>((session, connection, remote_fd, node_id))
    })?;

    crate::zcall_log!("[zcall-screen] PipeWire capture setup complete; entering capture lifetime");
    let result = pipewire_capture(remote_fd, node_id, stop, tx, status, settings);
    crate::zcall_log!("[zcall-screen] PipeWire capture returned; releasing portal D-Bus session");
    // Do not issue Session.Close here. On some desktop portal implementations
    // that method call never completes, leaving the capture worker alive and
    // every subsequent share queued behind it. The XDG portal owns sessions by
    // the client's D-Bus unique name; dropping the last proxy and shutting down
    // this dedicated runtime closes that connection and lets the portal tear
    // down the session without waiting for a Close reply.
    drop(session);
    drop(connection);
    runtime.shutdown_timeout(Duration::from_secs(2));
    crate::zcall_log!("[zcall-screen] portal D-Bus connection released");
    result
}

pub(super) async fn portal_request<T, F, E>(
    future: F,
    stop: Arc<AtomicBool>,
    timeout: Duration,
    operation: &str,
) -> Result<T, String>
where
    F: Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    match select(
        Box::pin(tokio::time::timeout(timeout, future)),
        Box::pin(wait_for_stop(stop)),
    )
    .await
    {
        Either::Left((Ok(Ok(value)), _)) => Ok(value),
        Either::Left((Ok(Err(error)), _)) => Err(portal_error_message(operation, error)),
        Either::Left((Err(_), _)) => Err(format!("Timed out {operation}")),
        Either::Right(((), _)) => Err("Screen sharing cancelled because the call ended".into()),
    }
}

async fn wait_for_stop(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn pipewire_capture(
    remote_fd: OwnedFd,
    node_id: u32,
    stop: Arc<AtomicBool>,
    tx: SyncSender<EncodedScreenFrame>,
    status: SyncSender<Result<(), String>>,
    settings: ScreenEncodeConfig,
) -> Result<(), String> {
    let raw_frames = Arc::new(LatestFrameQueue::default());
    let encoder_frames = raw_frames.clone();
    let raw_frame_seen = Arc::new(AtomicBool::new(false));
    let encoded_frame_seen = Arc::new(AtomicBool::new(false));
    // Keep userdata alive until the C capture is fully destroyed. Rust drops
    // locals in reverse order, so the guard below is intentionally declared after it.
    let user_data = Box::new(CaptureData {
        raw_frames,
        raw_frame_seen: raw_frame_seen.clone(),
    });
    let mut capture = PipeWireCaptureGuard::new(remote_fd, node_id, &user_data)?;
    crate::zcall_log!(
        "[zcall-screen] connected to portal PipeWire remote; expected node_id={node_id}"
    );

    // Start() may return before the portal has published its PipeWire node on
    // this remote connection. Wait for the exact node before connecting.
    let node_deadline = std::time::Instant::now() + Duration::from_secs(8);
    while !capture.node_seen() {
        if stop.load(Ordering::Acquire) {
            return Err(
                "Screen sharing cancelled while waiting for the portal PipeWire node".into(),
            );
        }
        if let Some(error) = capture.error() {
            return Err(error);
        }
        if std::time::Instant::now() >= node_deadline {
            return Err(format!(
                "Timed out waiting for portal PipeWire node {node_id} to appear in the remote registry"
            ));
        }
        capture.iterate(50)?;
    }
    crate::zcall_log!(
        "[zcall-screen] portal PipeWire target appeared in registry: node_id={node_id}"
    );

    capture.connect().map_err(|error| {
        format!("Negotiate PipeWire screen node {node_id} (BGRA/BGRx): {error}")
    })?;
    crate::zcall_log!(
        "[zcall-screen] PipeWire stream connected; negotiating BGRA/BGRx without a framerate constraint"
    );

    // Start the encoder only after all fallible PipeWire setup has succeeded.
    let encoder_stop = stop.clone();
    let encoder_settings = settings;
    let encoder_status = status.clone();
    let encoder_encoded_frame_seen = encoded_frame_seen.clone();
    let encoder_tx = tx;
    let encoder_worker = thread::Builder::new()
        .name("zcall-screen-encoder".into())
        .spawn(move || {
            if let Err(error) = encode_frames(
                encoder_frames,
                encoder_tx,
                encoder_stop.clone(),
                encoder_settings,
                encoder_status.clone(),
                encoder_encoded_frame_seen,
            ) {
                crate::zcall_log!("[zcall-screen] encoder failed: {error}");
                let _ = encoder_status.try_send(Err(error));
                encoder_stop.store(true, Ordering::Release);
            }
        })
        .map_err(|error| format!("Start screen encoder worker: {error}"))?;

    crate::zcall_log!("[zcall-screen] PipeWire stream connected; waiting for first frame");
    let stream_deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut frame_deadline = None;
    let capture_result = loop {
        if stop.load(Ordering::Acquire) {
            crate::zcall_log!("[zcall-screen] stop observed in PipeWire capture loop");
            break Ok(());
        }
        if let Some(error) = capture.error() {
            break Err(error);
        }
        let now = std::time::Instant::now();
        if capture.streaming() {
            let deadline = frame_deadline.get_or_insert(now + Duration::from_secs(15));
            if !raw_frame_seen.load(Ordering::Acquire) && now >= *deadline {
                break Err("Timed out waiting for the first PipeWire screen frame".into());
            }
            if raw_frame_seen.load(Ordering::Acquire)
                && !encoded_frame_seen.load(Ordering::Acquire)
                && now >= *deadline
            {
                break Err("Timed out waiting for the first encoded screen frame".into());
            }
        } else if now >= stream_deadline {
            break Err("Timed out waiting for the PipeWire screen stream to start".into());
        }
        if let Err(error) = capture.iterate(50) {
            break Err(error);
        }
    };

    // Publish unexpected PipeWire loss before setting the shared stop flag.
    // `report_worker_result` suppresses errors after stop to avoid duplicate
    // terminal events for user cancellation; report native failure while
    // cancellation is still distinguishable.
    report_capture_failure(&capture_result, &stop, &status);

    stop.store(true, Ordering::Release);
    crate::zcall_log!("[zcall-screen] disconnecting PipeWire stream");
    capture.disconnect();
    crate::zcall_log!("[zcall-screen] PipeWire stream disconnected; joining encoder");
    let encoder_result = encoder_worker
        .join()
        .map_err(|_| "Screen encoder worker panicked".to_string());
    crate::zcall_log!("[zcall-screen] encoder worker joined");
    capture_result.and(encoder_result)
}

#[cfg(test)]
#[path = "../tests/screen/portal.rs"]
mod tests;
