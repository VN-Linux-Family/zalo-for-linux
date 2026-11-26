//! Wayland XDG `ScreenCast` portal and `PipeWire` capture implementation.
use super::{
    encode_frames, portal_error_message, EncodedScreenFrame, LatestFrameQueue, RawScreenFrame,
    ScreenEncodeConfig,
};
use futures_util::future::{select, Either};
use pipewire_client::{
    format::VideoFormatCaps, remote::PipeWireRemote, PipeWireStream, StreamOptions,
};
use std::future::Future;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use ashpd::desktop::screencast::{
    CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
};
use ashpd::desktop::PersistMode;

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

fn portal_source_types(source: Option<&str>) -> Result<SourceType, String> {
    match source {
        Some("monitor" | "screen" | "screen:0") => Ok(SourceType::MONITOR),
        Some("window" | "window:select") => Ok(SourceType::WINDOW),
        Some(other) => Err(format!("Unsupported Wayland screen source: {other}")),
        None => Ok(SourceType::MONITOR | SourceType::WINDOW),
    }
}

pub(super) fn capture_worker(
    stop: &Arc<AtomicBool>,
    tx: SyncSender<EncodedScreenFrame>,
    status: &SyncSender<Result<(), String>>,
    source: Option<&str>,
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
            portal.create_session(<_>::default()),
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
            let source_types = portal_source_types(source)?;
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
                Duration::from_secs(120),
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
                portal.open_pipe_wire_remote(
                    &session,
                    ashpd::desktop::screencast::OpenPipeWireRemoteOptions::default(),
                ),
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

fn spawn_encoder_worker(
    frames: Arc<LatestFrameQueue>,
    stop: &Arc<AtomicBool>,
    settings: ScreenEncodeConfig,
    status: &SyncSender<Result<(), String>>,
    encoded_frame_seen: Arc<AtomicBool>,
    tx: SyncSender<EncodedScreenFrame>,
) -> Result<thread::JoinHandle<()>, String> {
    let encoder_stop = stop.clone();
    let encoder_status = status.clone();
    thread::Builder::new()
        .name("zcall-screen-encoder".into())
        .spawn(move || {
            if let Err(error) = encode_frames(
                &frames,
                &tx,
                &encoder_stop,
                &settings,
                &encoder_status,
                &encoded_frame_seen,
            ) {
                crate::zcall_log!("[zcall-screen] encoder failed: {error}");
                let _ = encoder_status.try_send(Err(error));
                encoder_stop.store(true, Ordering::Release);
            }
        })
        .map_err(|error| format!("Start screen encoder worker: {error}"))
}

fn pipewire_capture(
    remote_fd: OwnedFd,
    node_id: u32,
    stop: &Arc<AtomicBool>,
    tx: SyncSender<EncodedScreenFrame>,
    status: &SyncSender<Result<(), String>>,
    settings: ScreenEncodeConfig,
) -> Result<(), String> {
    let raw_frames = Arc::new(LatestFrameQueue::default());
    let encoder_frames = raw_frames.clone();
    let raw_frame_seen = Arc::new(AtomicBool::new(false));
    let encoded_frame_seen = Arc::new(AtomicBool::new(false));
    let remote = PipeWireRemote::from_portal(remote_fd, node_id);
    let stream_options = StreamOptions {
        name: "Zalo Call Screen Share".into(),
        client_properties: vec![("application.name".into(), "Zalo".into())],
        ..StreamOptions::default()
    };
    let mut stream = PipeWireStream::connect_with_cancellation_and_options(
        remote,
        &VideoFormatCaps::default(),
        &stream_options,
        || stop.load(Ordering::Acquire),
    )
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::Interrupted {
            "Screen sharing cancelled while connecting to the portal PipeWire remote".to_string()
        } else {
            format!("Connect PipeWire screen stream for node {node_id}: {error}")
        }
    })?;
    crate::zcall_log!(
        "[zcall-screen] PipeWire native stream connected to portal node_id={node_id}"
    );

    // Start the encoder only after all fallible PipeWire setup has succeeded.
    let encoder_worker = spawn_encoder_worker(
        encoder_frames,
        stop,
        settings,
        status,
        encoded_frame_seen.clone(),
        tx,
    )?;

    let capture_result = capture_pipewire_frames(
        &mut stream,
        &raw_frames,
        &raw_frame_seen,
        &encoded_frame_seen,
        stop,
    );

    report_capture_failure(&capture_result, stop, status);
    stop.store(true, Ordering::Release);
    crate::zcall_log!("[zcall-screen] disconnecting PipeWire stream");
    let disconnect_result = stream
        .disconnect()
        .map_err(|error| format!("Disconnect PipeWire screen stream: {error}"));
    crate::zcall_log!("[zcall-screen] PipeWire stream disconnected; joining encoder");
    let encoder_result = encoder_worker
        .join()
        .map_err(|_| "Screen encoder worker panicked".to_string());
    crate::zcall_log!("[zcall-screen] encoder worker joined");
    capture_result.and(disconnect_result).and(encoder_result)
}

fn capture_pipewire_frames(
    stream: &mut PipeWireStream,
    raw_frames: &LatestFrameQueue,
    raw_frame_seen: &AtomicBool,
    encoded_frame_seen: &AtomicBool,
    stop: &AtomicBool,
) -> Result<(), String> {
    crate::zcall_log!("[zcall-screen] PipeWire stream connected; waiting for first frame");
    let stream_deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut frame_deadline = None;
    loop {
        if stop.load(Ordering::Acquire) {
            crate::zcall_log!("[zcall-screen] stop observed in PipeWire capture loop");
            return Ok(());
        }
        let now = std::time::Instant::now();
        if stream.is_streaming() {
            let deadline = frame_deadline.get_or_insert(now + Duration::from_secs(15));
            if !raw_frame_seen.load(Ordering::Acquire) && now >= *deadline {
                return Err("Timed out waiting for the first PipeWire screen frame".into());
            }
            if raw_frame_seen.load(Ordering::Acquire)
                && !encoded_frame_seen.load(Ordering::Acquire)
                && now >= *deadline
            {
                return Err("Timed out waiting for the first encoded screen frame".into());
            }
        } else if now >= stream_deadline {
            return Err("Timed out waiting for the PipeWire screen stream to start".into());
        }

        let frame = stream
            .next_frame(Duration::from_millis(50))
            .map_err(|error| format!("PipeWire screen capture failed: {error}"))?;
        if let Some(frame) = frame {
            let width = frame.width;
            let height = frame.height;
            let stride = frame.stride;
            raw_frames.push(RawScreenFrame {
                bgra: frame.into_bgra(),
                width,
                height,
            });
            if !raw_frame_seen.swap(true, Ordering::AcqRel) {
                crate::zcall_log!(
                    "[zcall-screen] first raw PipeWire frame received: {width}x{height}, stride={stride}"
                );
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/screen/portal.rs"]
mod tests;
