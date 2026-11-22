//! Wayland XDG ScreenCast portal and PipeWire capture implementation.
use super::{
    encode_frames, portal_error_message, EncodedScreenFrame, RawScreenFrame, ScreenEncodeConfig,
};
use futures_util::future::{select, Either};
use pipewire as pw;
use pw::properties::properties;
use pw::spa::pod::Pod;
use std::future::Future;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ashpd::desktop::screencast::{
    CursorMode, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
};
use ashpd::desktop::PersistMode;

struct CaptureData {
    format: pw::spa::param::video::VideoInfoRaw,
    raw_tx: SyncSender<RawScreenFrame>,
    status: SyncSender<Result<(), String>>,
    raw_frame_seen: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
}

fn portal_source_types(source: Option<&str>) -> Result<SourceType, String> {
    match source {
        Some("monitor") | Some("screen") | Some("screen:0") => Ok(SourceType::MONITOR),
        Some("window") | Some("window:select") => Ok(SourceType::WINDOW),
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
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|error| format!("Create PipeWire main loop: {error}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|error| format!("Create PipeWire context: {error}"))?;
    let core = context
        .connect_fd_rc(remote_fd, None)
        .map_err(|error| format!("Connect to portal PipeWire stream: {error}"))?;
    crate::zcall_log!(
        "[zcall-screen] connected to portal PipeWire remote; expected node_id={node_id}"
    );

    // Start() may return before the portal has published its PipeWire node on
    // this remote connection. Connecting immediately races registry export and
    // KDE reports `target not found` (core error -2). Wait for the exact node.
    let core_error = Arc::new(Mutex::new(None::<String>));
    let core_error_callback = core_error.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, seq, result, message| {
            let detail = format!(
                "PipeWire core error: object={id}, seq={seq}, result={result}, message={message}"
            );
            crate::zcall_log!("[zcall-screen] {detail}");
            if let Ok(mut error) = core_error_callback.lock() {
                if error.is_none() {
                    *error = Some(detail);
                }
            }
        })
        .register();

    let registry = core
        .get_registry_rc()
        .map_err(|error| format!("Get portal PipeWire registry: {error}"))?;
    let target_node_seen = Arc::new(AtomicBool::new(false));
    let target_node_callback = target_node_seen.clone();
    let target_node_removed = target_node_seen.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.id == node_id {
                crate::zcall_log!(
                    "[zcall-screen] portal PipeWire target appeared in registry: node_id={node_id}"
                );
                target_node_callback.store(true, Ordering::Release);
            }
        })
        .global_remove(move |id| {
            if id == node_id {
                crate::zcall_log!(
                    "[zcall-screen] portal PipeWire target disappeared: node_id={node_id}"
                );
                target_node_removed.store(false, Ordering::Release);
            }
        })
        .register();

    let node_deadline = std::time::Instant::now() + Duration::from_secs(8);
    while !target_node_seen.load(Ordering::Acquire) {
        if stop.load(Ordering::Acquire) {
            return Err(
                "Screen sharing cancelled while waiting for the portal PipeWire node".into(),
            );
        }
        if let Some(error) = core_error.lock().ok().and_then(|mut error| error.take()) {
            return Err(error);
        }
        if std::time::Instant::now() >= node_deadline {
            return Err(format!(
                "Timed out waiting for portal PipeWire node {node_id} to appear in the remote registry"
            ));
        }
        mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
    }

    let stream = pw::stream::StreamBox::new(
        &core,
        "Zalo Call Screen Share",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|error| format!("Create PipeWire screen stream: {error}"))?;

    let stream_ready = Arc::new(AtomicBool::new(false));
    let ready_callback = stream_ready.clone();
    let connected_callback = stream_ready.clone();
    let raw_frame_seen = Arc::new(AtomicBool::new(false));
    let raw_frame_seen_callback = raw_frame_seen.clone();
    let (raw_tx, raw_rx) = sync_channel(2);
    let encoded_frame_seen = Arc::new(AtomicBool::new(false));
    let user_data = CaptureData {
        format: pw::spa::param::video::VideoInfoRaw::new(),
        raw_tx,
        status: status.clone(),
        raw_frame_seen: raw_frame_seen_callback,
        stop: stop.clone(),
    };
    let _listener = stream
        .add_local_listener_with_user_data(user_data)
        .state_changed(move |_, data, old, new| {
            crate::zcall_log!("[zcall-screen] PipeWire stream state: {old:?} -> {new:?}");
            match new {
            pw::stream::StreamState::Streaming => {
                ready_callback.store(true, Ordering::Release);
            }
            pw::stream::StreamState::Error(error) => {
                let message = format!("PipeWire screen stream failed: {error}");
                crate::zcall_log!("[zcall-screen] {message}");
                let _ = data.status.try_send(Err(message));
                data.stop.store(true, Ordering::Release);
            }
            pw::stream::StreamState::Unconnected
                if connected_callback.load(Ordering::Acquire)
                    && !data.stop.load(Ordering::Acquire) =>
            {
                let message = "PipeWire screen stream disconnected".to_string();
                crate::zcall_log!("[zcall-screen] {message}");
                let _ = data.status.try_send(Err(message));
                data.stop.store(true, Ordering::Release);
            }
            _ => {}
            }
        })
        .param_changed(|_, data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = pw::spa::param::format_utils::parse_format(param)
            else {
                return;
            };
            if media_type != pw::spa::param::format::MediaType::Video
                || media_subtype != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }
            let _ = data.format.parse(param);
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(frame_data) = datas.first_mut() else {
                return;
            };
            let chunk = frame_data.chunk();
            let offset = chunk.offset() as usize;
            let size = chunk.size() as usize;
            let width = data.format.size().width as usize;
            let height = data.format.size().height as usize;
            let stride = chunk.stride().unsigned_abs() as usize;
            if width < 2 || height < 2 || width > 4096 || height > 4096 || stride < width * 4 {
                return;
            }
            let Some(mapped) = frame_data.data() else {
                return;
            };
            let Some(end) = offset.checked_add(size) else {
                return;
            };
            if end > mapped.len() || size < stride.saturating_mul(height) {
                return;
            }

            // Compact potentially padded PipeWire BGRx rows before passing the
            // pixels to OpenH264. BGRx uses the same channel layout as BGRA.
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
            match data.raw_tx.try_send(frame) {
                Ok(()) => {
                    if !data.raw_frame_seen.swap(true, Ordering::AcqRel) {
                        crate::zcall_log!(
                            "[zcall-screen] first raw PipeWire frame received: {width}x{height}, stride={stride}, bytes={size}"
                        );
                    }
                },
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    data.stop.store(true, Ordering::Release);
                }
            }
        })
        .register()
        .map_err(|error| format!("Register PipeWire stream listener: {error}"))?;

    let obj = pw::spa::pod::object!(
        pw::spa::utils::SpaTypes::ObjectParamFormat,
        pw::spa::param::ParamType::EnumFormat,
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaType,
            Id,
            pw::spa::param::format::MediaType::Video
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::MediaSubtype,
            Id,
            pw::spa::param::format::MediaSubtype::Raw
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            pw::spa::param::video::VideoFormat::BGRA,
            pw::spa::param::video::VideoFormat::BGRA,
            pw::spa::param::video::VideoFormat::BGRx,
        ),
        pw::spa::pod::property!(
            pw::spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            pw::spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            pw::spa::utils::Rectangle {
                width: 320,
                height: 240
            },
            pw::spa::utils::Rectangle {
                width: 1920,
                height: 1080
            }
        ),
        // Do not constrain VideoFramerate here. KDE's portal source advertises
        // an unspecified frame rate (0/1) plus VideoMaxFramerate; forcing a
        // fixed/ranged VideoFramerate makes PipeWire's SPA intersection fail
        // with `no more input formats` before the stream can start.
    );
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .map_err(|error| format!("Serialize PipeWire video format: {error:?}"))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values)
        .ok_or_else(|| "Could not build PipeWire video format".to_string())?];

    crate::zcall_log!(
        "[zcall-screen] negotiating BGRA/BGRx PipeWire format without a framerate constraint"
    );
    stream
        .connect(
            pw::spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|error| {
            format!("Negotiate PipeWire screen node {node_id} (BGRA/BGRx): {error}")
        })?;
    crate::zcall_log!("[zcall-screen] PipeWire stream connected; waiting for first frame");

    // Start the encoder only after all fallible PipeWire setup has succeeded.
    // This prevents a failed connect/format negotiation from detaching a worker
    // blocked on raw_rx before the capture loop has a chance to shut it down.
    let encoder_stop = stop.clone();
    let encoder_settings = settings.clone();
    let encoder_status = status.clone();
    let encoder_encoded_frame_seen = encoded_frame_seen.clone();
    let encoder_tx = tx;
    let encoder_worker = thread::Builder::new()
        .name("zcall-screen-encoder".into())
        .spawn(move || {
            if let Err(error) = encode_frames(
                raw_rx,
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

    let stream_deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut frame_deadline = None;
    let capture_result = loop {
        if stop.load(Ordering::Acquire) {
            crate::zcall_log!("[zcall-screen] stop observed in PipeWire capture loop");
            break Ok(());
        }
        if let Some(error) = core_error.lock().ok().and_then(|mut error| error.take()) {
            break Err(error);
        }
        let now = std::time::Instant::now();
        if stream_ready.load(Ordering::Acquire) {
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
        let result = mainloop
            .loop_()
            .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
        if result < 0 {
            break Err(format!("PipeWire main loop iteration failed: {result}"));
        }
    };

    // Every exit path closes the PipeWire stream and stops/joins the encoder;
    // otherwise a timeout detached an encoder waiting forever on its channel.
    stop.store(true, Ordering::Release);
    crate::zcall_log!("[zcall-screen] disconnecting PipeWire stream");
    if let Err(error) = stream.disconnect() {
        crate::zcall_log!("[zcall-screen] PipeWire stream disconnect returned: {error}");
    }
    let disconnect_result = mainloop
        .loop_()
        .iterate(pw::loop_::Timeout::Finite(Duration::from_millis(50)));
    crate::zcall_log!(
        "[zcall-screen] PipeWire disconnect processed (iterate={disconnect_result}); releasing stream listener"
    );
    drop(_listener);
    drop(stream);
    crate::zcall_log!("[zcall-screen] PipeWire stream and listener released; joining encoder");
    let encoder_result = encoder_worker
        .join()
        .map_err(|_| "Screen encoder worker panicked".to_string());
    crate::zcall_log!("[zcall-screen] encoder worker joined");
    capture_result.and(encoder_result)
}

#[cfg(test)]
mod tests {
    use super::portal_source_types;
    use ashpd::desktop::screencast::SourceType;

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
}
