//! X11 capture through the XCB protocol. Wayland stays portal-only.
use super::{resize_bgra, RawScreenFrame, ScreenEncodeConfig};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::SyncSender,
        Arc,
    },
    thread,
    time::{Duration, Instant},
};
use xcb::{shm, x, Connection, Xid, XidNew};

pub(super) fn is_x11_session() -> bool {
    let session = std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    session == "x11"
        || (session.is_empty()
            && std::env::var_os("DISPLAY").is_some()
            && std::env::var_os("WAYLAND_DISPLAY").is_none())
}
fn connect() -> Result<(Connection, i32), String> {
    let (conn, screen_num) =
        Connection::connect(None).map_err(|e| format!("Connect to X11 display: {e}"))?;

    // XFixes requires an explicit version handshake before extension requests.
    // Without it, GetCursorImage is rejected with BadRequest even when the
    // server advertises XFIXES in QueryExtension/xdpyinfo.
    match conn.wait_for_reply(conn.send_request(&xcb::xfixes::QueryVersion {
        client_major_version: 6,
        client_minor_version: 0,
    })) {
        Ok(version) if version.major_version() >= 2 => crate::zcall_log!(
            "[zcall-screen] XFixes negotiated version {}.{}",
            version.major_version(),
            version.minor_version()
        ),
        Ok(version) => crate::zcall_log!(
            "[zcall-screen] XFixes {}.{} does not support cursor images",
            version.major_version(),
            version.minor_version()
        ),
        Err(error) => {
            crate::zcall_log!("[zcall-screen] XFixes version negotiation failed: {error}")
        }
    }

    Ok((conn, screen_num))
}
fn atom(conn: &Connection, name: &[u8]) -> Result<x::Atom, String> {
    conn.wait_for_reply(conn.send_request(&x::InternAtom {
        only_if_exists: false,
        name,
    }))
    .map(|r| r.atom())
    .map_err(|e| format!("Resolve X11 atom: {e}"))
}
fn prop(
    conn: &Connection,
    win: x::Window,
    property: x::Atom,
    r#type: x::Atom,
) -> Result<Vec<u8>, String> {
    conn.wait_for_reply(conn.send_request(&x::GetProperty {
        delete: false,
        window: win,
        property,
        r#type,
        long_offset: 0,
        long_length: 16384,
    }))
    .map(|reply| reply.value::<u8>().to_vec())
    .map_err(|e| format!("Read X11 property: {e}"))
}
fn title(conn: &Connection, win: x::Window, net_name: x::Atom, utf8: x::Atom) -> String {
    prop(conn, win, net_name, utf8)
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| prop(conn, win, x::ATOM_WM_NAME, x::ATOM_STRING).ok())
        .map(|v| {
            String::from_utf8_lossy(&v)
                .trim_matches('\0')
                .trim()
                .to_owned()
        })
        .unwrap_or_default()
}
fn windows(conn: &Connection, root: x::Window) -> Result<Vec<x::Window>, String> {
    let list = atom(conn, b"_NET_CLIENT_LIST_STACKING")?;
    let reply = conn
        .wait_for_reply(conn.send_request(&x::GetProperty {
            delete: false,
            window: root,
            property: list,
            r#type: x::ATOM_ANY,
            long_offset: 0,
            long_length: 16384,
        }))
        .map_err(|e| format!("Read X11 client list: {e}"))?;
    let result: Vec<_> = reply
        .value::<x::Window>()
        .to_vec()
        .into_iter()
        .filter(|w| !w.is_none())
        .collect();
    if !result.is_empty() {
        return Ok(result);
    }
    conn.wait_for_reply(conn.send_request(&x::QueryTree { window: root }))
        .map(|r| r.children().to_vec())
        .map_err(|e| format!("Enumerate X11 windows: {e}"))
}
pub fn list_windows() -> Result<Vec<(String, String)>, String> {
    if !is_x11_session() {
        return Err("X11 window selection is available only in an X11 session".into());
    }
    let (conn, screen_num) = connect()?;
    let setup = conn.get_setup();
    let screen = setup
        .roots()
        .nth(screen_num as usize)
        .ok_or("X11 display has no default screen")?;
    let (root, net_name, utf8) = (
        screen.root(),
        atom(&conn, b"_NET_WM_NAME")?,
        atom(&conn, b"UTF8_STRING")?,
    );
    let mut out = Vec::new();
    for win in windows(&conn, root)? {
        let name = title(&conn, win, net_name, utf8);
        if name.is_empty() || name.starts_with("Zalo Call") {
            continue;
        }
        let attrs = conn.wait_for_reply(conn.send_request(&x::GetWindowAttributes { window: win }));
        if !attrs.is_ok_and(|a| a.map_state() == x::MapState::Viewable) {
            continue;
        }
        let geometry = conn.wait_for_reply(conn.send_request(&x::GetGeometry {
            drawable: x::Drawable::Window(win),
        }));
        if !geometry.is_ok_and(|g| g.width() >= 64 && g.height() >= 48) {
            continue;
        }
        out.push((format!("window:{}", win.resource_id()), name));
    }
    Ok(out)
}
#[derive(Clone, Copy)]
enum Target {
    Screen,
    Window(u32),
}
fn parse_target(source: Option<&str>) -> Result<Target, String> {
    match source {
        None | Some("") | Some("screen") | Some("screen:0") => Ok(Target::Screen),
        Some(v) if v.starts_with("window:") => {
            let id = v[7..].parse::<u32>().map_err(|_| "Invalid X11 window id")?;
            if id == 0 {
                Err("Invalid X11 window id".into())
            } else {
                Ok(Target::Window(id))
            }
        }
        _ => Err("Unsupported X11 capture source".into()),
    }
}
struct SysvShm {
    id: libc::c_int,
    address: *mut libc::c_void,
}
impl Drop for SysvShm {
    fn drop(&mut self) {
        // SAFETY: `address` and `id` are the live mapping and segment created
        // together by `shmat`/`shmget`; this owner drops them exactly once.
        unsafe {
            libc::shmdt(self.address);
            libc::shmctl(self.id, libc::IPC_RMID, std::ptr::null_mut());
        }
    }
}

/// Reuse one MIT-SHM segment for the lifetime of an X11 capture worker.
/// The server attachment is detached before the local mapping is released.
struct PersistentShm<'a> {
    conn: &'a Connection,
    segment: Option<SysvShm>,
    shmseg: Option<shm::Seg>,
    capacity: usize,
}

impl<'a> PersistentShm<'a> {
    fn new(conn: &'a Connection) -> Result<Self, String> {
        if shm::get_extension_data(conn).is_none() {
            return Err(
                "X11 server does not provide MIT-SHM; native X11 capture requires it".into(),
            );
        }
        let version = conn
            .wait_for_reply(conn.send_request(&shm::QueryVersion {}))
            .map_err(|error| format!("Negotiate MIT-SHM: {error}"))?;
        if version.major_version() < 1 {
            return Err(format!(
                "Unsupported MIT-SHM version {}.{}",
                version.major_version(),
                version.minor_version()
            ));
        }
        crate::zcall_log!(
            "[zcall-screen] MIT-SHM negotiated version {}.{}",
            version.major_version(),
            version.minor_version()
        );
        Ok(Self {
            conn,
            segment: None,
            shmseg: None,
            capacity: 0,
        })
    }

    fn detach(&mut self) {
        if let Some(shmseg) = self.shmseg.take() {
            let request = self.conn.send_request_checked(&shm::Detach { shmseg });
            if let Err(error) = self.conn.check_request(request) {
                crate::zcall_log!("[zcall-screen] MIT-SHM detach failed: {error:?}");
            }
        }
        self.segment = None;
        self.capacity = 0;
    }

    fn ensure_capacity(&mut self, required: usize) -> Result<(), String> {
        if self.capacity >= required && self.segment.is_some() {
            return Ok(());
        }
        self.detach();
        // SAFETY: IPC_PRIVATE requests a new segment; the size is non-zero
        // and the permissions restrict access to this process's user.
        let id = unsafe {
            libc::shmget(
                libc::IPC_PRIVATE,
                required.max(4096),
                libc::IPC_CREAT | 0o600,
            )
        };
        if id < 0 {
            return Err(format!(
                "Allocate persistent X11 shared memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: `id` is the newly-created segment returned above; a null
        // requested address lets the kernel choose a suitable mapping.
        let address = unsafe { libc::shmat(id, std::ptr::null(), 0) };
        if address as isize == -1 {
            // SAFETY: `id` is the segment created above and has not yet been
            // transferred to `SysvShm`, so this path removes it exactly once.
            unsafe {
                libc::shmctl(id, libc::IPC_RMID, std::ptr::null_mut());
            }
            return Err(format!(
                "Map persistent X11 shared memory: {}",
                std::io::Error::last_os_error()
            ));
        }
        let segment = SysvShm { id, address };
        let shmseg: shm::Seg = self.conn.generate_id();
        let request = self.conn.send_request_checked(&shm::Attach {
            shmseg,
            shmid: id as u32,
            read_only: false,
        });
        self.conn
            .check_request(request)
            .map_err(|error| format!("Attach persistent X11 shared memory: {error:?}"))?;
        self.segment = Some(segment);
        self.shmseg = Some(shmseg);
        self.capacity = required.max(4096);
        Ok(())
    }

    fn get_image(
        &mut self,
        drawable: x::Drawable,
        width: u16,
        height: u16,
        required: usize,
    ) -> Result<Vec<u8>, String> {
        self.ensure_capacity(required)?;
        let shmseg = self.shmseg.ok_or("MIT-SHM segment is unavailable")?;
        let reply = self
            .conn
            .wait_for_reply(self.conn.send_request(&shm::GetImage {
                drawable,
                x: 0,
                y: 0,
                width,
                height,
                plane_mask: u32::MAX,
                format: x::ImageFormat::ZPixmap as u8,
                shmseg,
                offset: 0,
            }))
            .map_err(|error| format!("MIT-SHM image capture: {error:?}"))?;
        if (reply.size() as usize) < required {
            return Err("MIT-SHM frame is truncated".into());
        }
        let address = self
            .segment
            .as_ref()
            .ok_or("MIT-SHM mapping is unavailable")?
            .address;
        // SAFETY: the segment remains mapped and attached until this frame is
        // copied; GetImage's reply confirms the server has completed the write.
        let bytes = unsafe { std::slice::from_raw_parts(address.cast::<u8>(), required) };
        Ok(bytes.to_vec())
    }
}

impl Drop for PersistentShm<'_> {
    fn drop(&mut self) {
        self.detach();
    }
}

fn capture_drawable(
    conn: &Connection,
    drawable: x::Drawable,
    width: u16,
    height: u16,
    depth: u8,
    shm_capture: &mut PersistentShm<'_>,
) -> Result<RawScreenFrame, String> {
    let width = width.min(8192);
    let height = height.min(8192);
    if width < 2 || height < 2 {
        return Err("X11 source has invalid dimensions".into());
    }
    let format = conn
        .get_setup()
        .pixmap_formats()
        .iter()
        .find(|format| format.depth() == depth)
        .ok_or("Unknown X11 pixel format")?;
    let bpp = usize::from(format.bits_per_pixel());
    if bpp != 32 {
        return Err(format!("Unsupported X11 pixel format ({bpp} bpp)"));
    }
    let pad = usize::from(format.scanline_pad()).max(8);
    let stride = (usize::from(width) * bpp).div_ceil(pad) * (pad / 8);
    let required = stride * usize::from(height);
    let source = shm_capture
        .get_image(drawable, width, height, required)
        .map_err(|error| format!("Capture X11 frame through MIT-SHM: {error}"))?;
    if source.len() < required {
        return Err("Truncated X11 frame".into());
    }
    let pixel_bytes = usize::from(width) * usize::from(height) * 4;
    let bgra = if cfg!(target_endian = "little") && stride == usize::from(width) * 4 {
        // On the common little-endian X11 layout, GetImage already returns
        // BGRA-compatible bytes. Reuse its allocation and only normalize the
        // unused alpha byte instead of allocating and copying a second frame.
        let mut pixels = source;
        pixels.truncate(pixel_bytes);
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        pixels
    } else {
        let mut bgra = vec![0; pixel_bytes];
        for y in 0..usize::from(height) {
            for x in 0..usize::from(width) {
                let source_offset = y * stride + x * 4;
                let dest_offset = (y * usize::from(width) + x) * 4;
                if cfg!(target_endian = "little") {
                    bgra[dest_offset..dest_offset + 3]
                        .copy_from_slice(&source[source_offset..source_offset + 3]);
                } else {
                    bgra[dest_offset..dest_offset + 3].copy_from_slice(&[
                        source[source_offset + 3],
                        source[source_offset + 2],
                        source[source_offset + 1],
                    ]);
                }
                bgra[dest_offset + 3] = 255;
            }
        }
        bgra
    };
    Ok(RawScreenFrame {
        bgra,
        width: u32::from(width),
        height: u32::from(height),
    })
}

fn composite_cursor(
    frame: &mut RawScreenFrame,
    left: i32,
    top: i32,
    cursor_width: usize,
    cursor_height: usize,
    pixels: &[u32],
) {
    let frame_width = frame.width as usize;
    let frame_height = frame.height as usize;
    if frame_width == 0
        || frame_height == 0
        || pixels.len() < cursor_width.saturating_mul(cursor_height)
        || frame.bgra.len() < frame_width.saturating_mul(frame_height).saturating_mul(4)
    {
        return;
    }

    for cursor_y in 0..cursor_height {
        let dest_y = top + cursor_y as i32;
        if dest_y < 0 || dest_y >= frame_height as i32 {
            continue;
        }
        for cursor_x in 0..cursor_width {
            let dest_x = left + cursor_x as i32;
            if dest_x < 0 || dest_x >= frame_width as i32 {
                continue;
            }
            let argb = pixels[cursor_y * cursor_width + cursor_x];
            let alpha = (argb >> 24) & 0xff;
            if alpha == 0 {
                continue;
            }
            let inverse_alpha = 255 - alpha;
            let offset = (dest_y as usize * frame_width + dest_x as usize) * 4;
            // XFixes cursor pixels are premultiplied ARGB. Do not multiply
            // their RGB channels by alpha a second time.
            let blue = argb & 0xff;
            let green = (argb >> 8) & 0xff;
            let red = (argb >> 16) & 0xff;
            frame.bgra[offset] =
                (blue + (u32::from(frame.bgra[offset]) * inverse_alpha + 127) / 255).min(255) as u8;
            frame.bgra[offset + 1] = (green
                + (u32::from(frame.bgra[offset + 1]) * inverse_alpha + 127) / 255)
                .min(255) as u8;
            frame.bgra[offset + 2] = (red
                + (u32::from(frame.bgra[offset + 2]) * inverse_alpha + 127) / 255)
                .min(255) as u8;
            frame.bgra[offset + 3] = 255;
        }
    }
}

static CURSOR_IMAGE_ERRORS: AtomicU64 = AtomicU64::new(0);
static CURSOR_POSITION_ERRORS: AtomicU64 = AtomicU64::new(0);

/// Composite a cursor reply already requested before GetImage. Pipelining
/// requests keeps cursor lookup and window-coordinate translation off the
/// capture path's serial round-trip critical path.
fn overlay_cursor(
    cursor: &xcb::xfixes::GetCursorImageReply,
    origin: (i32, i32),
    frame: &mut RawScreenFrame,
) {
    let left = i32::from(cursor.x()) - i32::from(cursor.xhot()) - origin.0;
    let top = i32::from(cursor.y()) - i32::from(cursor.yhot()) - origin.1;
    let cursor_width = usize::from(cursor.width());
    let cursor_height = usize::from(cursor.height());
    let pixels = cursor.cursor_image();
    if cursor_width == 0
        || cursor_height == 0
        || pixels.len() < cursor_width.saturating_mul(cursor_height)
    {
        return;
    }

    composite_cursor(frame, left, top, cursor_width, cursor_height, pixels);
}

/// A window may be resized between GetGeometry and GetImage. In that case
/// GetImage can fail with BadMatch or return a frame using stale dimensions.
/// Re-check after the image request and retry instead of publishing a cropped
/// frame or terminating the capture worker.
fn capture_window_with_shm(
    conn: &Connection,
    win: x::Window,
    shm_capture: &mut PersistentShm<'_>,
) -> Result<Option<RawScreenFrame>, String> {
    const RESIZE_RETRIES: usize = 3;
    let mut geometry_changed = false;
    let mut last_bad_match = None;

    for _ in 0..RESIZE_RETRIES {
        let geometry = conn
            .wait_for_reply(conn.send_request(&x::GetGeometry {
                drawable: x::Drawable::Window(win),
            }))
            .map_err(|error| format!("X11 window unavailable: {error}"))?;
        let (width, height, depth) = (geometry.width(), geometry.height(), geometry.depth());
        let frame = capture_drawable(
            conn,
            x::Drawable::Window(win),
            width,
            height,
            depth,
            shm_capture,
        );

        let current = conn
            .wait_for_reply(conn.send_request(&x::GetGeometry {
                drawable: x::Drawable::Window(win),
            }))
            .map_err(|error| format!("X11 window disappeared during capture: {error}"))?;
        let dimensions_unchanged =
            current.width() == width && current.height() == height && current.depth() == depth;

        match (frame, dimensions_unchanged) {
            (Ok(frame), true) => return Ok(Some(frame)),
            (Ok(_), false) => geometry_changed = true,
            (Err(error), false) => {
                geometry_changed = true;
                last_bad_match = Some(error);
            }
            (Err(error), true) if error.contains("X(Match(") || error.contains("BadMatch") => {
                // The server can report BadMatch while a resize is in flight
                // even when the follow-up geometry request still sees the old
                // size. Retry it as a transient race, not a fatal source error.
                last_bad_match = Some(error);
            }
            (Err(error), true) => return Err(error),
        }
    }

    if geometry_changed {
        // Continuous interactive resizing is normal. Skip this frame; the next
        // iteration will read the settled geometry without ending screen share.
        Ok(None)
    } else if let Some(error) = last_bad_match {
        Err(error)
    } else {
        Ok(None)
    }
}

fn capture_with_shm(
    conn: &Connection,
    root: x::Window,
    target: Target,
    shm_capture: &mut PersistentShm<'_>,
) -> Result<Option<RawScreenFrame>, String> {
    // Queue cursor and window-origin requests before the image request. XCB
    // sends them in order, allowing the server to process all three while the
    // client is waiting for the captured pixels instead of serializing extra
    // round trips into every frame.
    let cursor_cookie = conn.send_request(&xcb::xfixes::GetCursorImage {});
    let window_position_cookie = match target {
        Target::Screen => None,
        Target::Window(id) => Some(conn.send_request(&x::TranslateCoordinates {
            src_window: x::Window::new(id),
            dst_window: root,
            src_x: 0,
            src_y: 0,
        })),
    };

    let capture_result = match target {
        Target::Screen => conn
            .get_setup()
            .roots()
            .find(|screen| screen.root() == root)
            .ok_or_else(|| "X11 root disappeared".to_string())
            .and_then(|screen| {
                capture_drawable(
                    conn,
                    x::Drawable::Window(root),
                    screen.width_in_pixels(),
                    screen.height_in_pixels(),
                    screen.root_depth(),
                    shm_capture,
                )
                .map(Some)
            }),
        Target::Window(id) => capture_window_with_shm(conn, x::Window::new(id), shm_capture),
    };

    // Always consume the pipelined replies, even if image capture failed, so
    // an error cannot leave stale cookies queued on the shared XCB connection.
    let cursor_result = conn.wait_for_reply(cursor_cookie);
    let origin_result = match window_position_cookie {
        Some(cookie) => conn
            .wait_for_reply(cookie)
            .map(|position| (i32::from(position.dst_x()), i32::from(position.dst_y()))),
        None => Ok((0, 0)),
    };

    let mut frame = capture_result?;
    if let Some(frame) = frame.as_mut() {
        match (cursor_result, origin_result) {
            (Ok(cursor), Ok(origin)) => overlay_cursor(&cursor, origin, frame),
            (Err(error), _) => {
                let count = CURSOR_IMAGE_ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
                if count == 1 || count.is_multiple_of(60) {
                    crate::zcall_log!(
                        "[zcall-screen] XFixes cursor image request failed {count} times: {error}"
                    );
                }
            }
            (_, Err(error)) => {
                let count = CURSOR_POSITION_ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
                if count == 1 || count.is_multiple_of(60) {
                    crate::zcall_log!(
                        "[zcall-screen] X11 window-origin request failed {count} times: {error}"
                    );
                }
            }
        }
    }
    Ok(frame)
}

#[cfg(test)]
fn capture(
    conn: &Connection,
    root: x::Window,
    target: Target,
) -> Result<Option<RawScreenFrame>, String> {
    let mut shm_capture = PersistentShm::new(conn)?;
    capture_with_shm(conn, root, target, &mut shm_capture)
}

#[cfg(test)]
fn capture_window(conn: &Connection, win: x::Window) -> Result<Option<RawScreenFrame>, String> {
    let mut shm_capture = PersistentShm::new(conn)?;
    capture_window_with_shm(conn, win, &mut shm_capture)
}

pub(super) fn capture_worker(
    stop: Arc<AtomicBool>,
    tx: SyncSender<super::EncodedScreenFrame>,
    status: SyncSender<Result<(), String>>,
    settings: ScreenEncodeConfig,
    source: Option<String>,
) -> Result<(), String> {
    if !is_x11_session() {
        return Err("X11 capture requested outside an X11 session".into());
    }
    let target = parse_target(source.as_deref())?;
    let (conn, screen_num) = connect()?;
    let root = conn
        .get_setup()
        .roots()
        .nth(screen_num as usize)
        .ok_or("X11 display has no default screen")?
        .root();
    let mut shm_capture = PersistentShm::new(&conn)?;
    let raw_frames = Arc::new(super::LatestFrameQueue::default());
    let encoder_raw_frames = raw_frames.clone();
    let enc_stop = stop.clone();
    let enc_settings = settings.clone();
    let enc_status = status.clone();
    let encoded_frame_seen = Arc::new(AtomicBool::new(false));
    let encoder_encoded_frame_seen = encoded_frame_seen.clone();
    let encoder = thread::Builder::new()
        .name("zcall-x11-screen-encoder".into())
        .spawn(move || {
            if let Err(error) = super::encode_frames(
                encoder_raw_frames,
                tx,
                enc_stop.clone(),
                enc_settings,
                enc_status.clone(),
                encoder_encoded_frame_seen,
            ) {
                crate::zcall_log!("[zcall-screen] X11 encoder failed: {error}");
                let _ = enc_status.try_send(Err(error));
                enc_stop.store(true, Ordering::Release);
            }
        })
        .map_err(|e| format!("Start X11 encoder: {e}"))?;
    let startup_deadline = Instant::now() + Duration::from_secs(15);
    let mut stats_started = Instant::now();
    let mut captured_frames = 0u64;
    let mut overwritten_frames = 0u64;
    let mut late_frames = 0u64;
    let mut total_capture_ms = 0.0f64;
    let mut max_capture_ms = 0.0f64;
    let capture_result = (|| {
        while !stop.load(Ordering::Acquire) {
            if Instant::now() >= startup_deadline && !encoded_frame_seen.load(Ordering::Acquire) {
                return Err("Timed out waiting for the first encoded X11 screen frame".into());
            }
            let start = Instant::now();
            let Some(frame) = capture_with_shm(&conn, root, target, &mut shm_capture)? else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            let (bgra, width, height) =
                resize_bgra(frame.bgra, frame.width, frame.height, settings.max_short)?;
            if raw_frames.push(RawScreenFrame {
                bgra,
                width,
                height,
            }) {
                overwritten_frames += 1;
            }
            let fps = settings.capture_fps.load(Ordering::Acquire);
            let interval = Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));
            let elapsed = start.elapsed();
            let capture_ms = elapsed.as_secs_f64() * 1000.0;
            captured_frames += 1;
            total_capture_ms += capture_ms;
            max_capture_ms = max_capture_ms.max(capture_ms);
            if elapsed >= interval {
                late_frames += 1;
            }
            if stats_started.elapsed() >= Duration::from_secs(5) {
                let window_seconds = stats_started.elapsed().as_secs_f64();
                let measured_fps = captured_frames as f64 / window_seconds;
                let average_ms = total_capture_ms / captured_frames.max(1) as f64;
                crate::zcall_log!(
                    "[zcall-screen] X11 capture stats: fps={measured_fps:.1}, avg_ms={average_ms:.1}, max_ms={max_capture_ms:.1}, overwritten={overwritten_frames}, late={late_frames}"
                );
                stats_started = Instant::now();
                captured_frames = 0;
                overwritten_frames = 0;
                late_frames = 0;
                total_capture_ms = 0.0;
                max_capture_ms = 0.0;
            }
            if elapsed < interval {
                thread::sleep(interval - elapsed);
            }
        }
        Ok(())
    })();
    if capture_result.is_err() {
        // The encoder owns another Arc to the latest-frame mailbox, so it
        // cannot observe producer disconnection. Explicitly stop and join it
        // on every capture error path to avoid leaking a worker thread.
        stop.store(true, Ordering::Release);
    }
    let _ = encoder.join();
    capture_result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn composites_premultiplied_cursor_pixels_over_the_capture() {
        let mut frame = RawScreenFrame {
            bgra: vec![10, 20, 30, 255],
            width: 1,
            height: 1,
        };
        // Half-transparent red in premultiplied ARGB, over a dark background.
        composite_cursor(&mut frame, 0, 0, 1, 1, &[0x8080_0000]);
        assert_eq!(frame.bgra, vec![5, 10, 143, 255]);
    }

    #[test]
    fn clips_cursor_pixels_outside_the_captured_window() {
        let mut frame = RawScreenFrame {
            bgra: vec![0, 0, 0, 255],
            width: 1,
            height: 1,
        };
        composite_cursor(&mut frame, -1, 0, 2, 1, &[0xffff_ffff, 0xffff_0000]);
        assert_eq!(frame.bgra, vec![0, 0, 255, 255]);
    }

    #[test]
    fn parses_x11_source_ids() {
        assert!(matches!(parse_target(None), Ok(Target::Screen)));
        assert!(matches!(parse_target(Some("screen:0")), Ok(Target::Screen)));
        assert!(matches!(
            parse_target(Some("window:42")),
            Ok(Target::Window(42))
        ));
        assert!(parse_target(Some("window:nope")).is_err());
        assert!(parse_target(Some("window:0")).is_err());
        assert!(parse_target(Some("camera:0")).is_err());
    }

    #[test]
    fn xfixes_cursor_position_matches_the_x11_pointer_hotspot() {
        if !is_x11_session() || std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let (conn, screen_num) = connect().expect("connect to X11");
        let root = conn
            .get_setup()
            .roots()
            .nth(screen_num as usize)
            .expect("root screen")
            .root();
        let pointer = conn
            .wait_for_reply(conn.send_request(&x::QueryPointer { window: root }))
            .expect("query actual X11 pointer position");
        let cursor = conn
            .wait_for_reply(conn.send_request(&xcb::xfixes::GetCursorImage {}))
            .expect("read XFixes cursor image");
        assert_eq!(
            (cursor.x(), cursor.y()),
            (pointer.root_x(), pointer.root_y()),
            "XFixes cursor coordinates must identify the pointer hotspot in root coordinates"
        );
        assert!(cursor.width() > 0 && cursor.height() > 0);
        assert_eq!(
            cursor.cursor_image().len(),
            usize::from(cursor.width()) * usize::from(cursor.height()),
            "XFixes cursor image must contain every ARGB pixel"
        );
    }

    #[test]
    fn root_capture_composites_the_live_xfixes_cursor_when_available() {
        if !is_x11_session() || std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let (conn, screen_num) = connect().expect("connect to X11");
        let screen = conn
            .get_setup()
            .roots()
            .nth(screen_num as usize)
            .expect("root screen");
        let cursor = match conn.wait_for_reply(conn.send_request(&xcb::xfixes::GetCursorImage {})) {
            Ok(cursor) => cursor,
            Err(_) => return, // XFixes may be unavailable on a minimal X server.
        };
        let mut frame = RawScreenFrame {
            bgra: [24, 48, 72, 255].repeat(
                usize::from(screen.width_in_pixels()) * usize::from(screen.height_in_pixels()),
            ),
            width: u32::from(screen.width_in_pixels()),
            height: u32::from(screen.height_in_pixels()),
        };
        overlay_cursor(&cursor, (0, 0), &mut frame);

        let left = i32::from(cursor.x()) - i32::from(cursor.xhot());
        let top = i32::from(cursor.y()) - i32::from(cursor.yhot());
        let cursor_width = usize::from(cursor.width());
        let cursor_height = usize::from(cursor.height());
        let mut has_visible_pixel = false;
        for y in 0..cursor_height {
            for x in 0..cursor_width {
                let pixel = cursor.cursor_image()[y * cursor_width + x];
                if pixel >> 24 == 0 || pixel & 0x00ff_ffff == 0 {
                    continue;
                }
                let px = left + x as i32;
                let py = top + y as i32;
                if px < 0 || py < 0 || px >= frame.width as i32 || py >= frame.height as i32 {
                    continue;
                }
                let offset = (py as usize * frame.width as usize + px as usize) * 4;
                let expected = [pixel as u8, (pixel >> 8) as u8, (pixel >> 16) as u8];
                if frame.bgra[offset..offset + 3] == expected {
                    has_visible_pixel = true;
                    break;
                }
            }
            if has_visible_pixel {
                break;
            }
        }
        assert!(
            has_visible_pixel,
            "the XFixes cursor pixels must be present in the captured frame"
        );
    }

    #[test]
    fn captures_root_and_lists_windows_when_an_x_server_is_available() {
        if !is_x11_session() || std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let (conn, screen_num) = connect().expect("connect to X11");
        let root = conn
            .get_setup()
            .roots()
            .nth(screen_num as usize)
            .expect("root screen")
            .root();
        let root_frame = capture(&conn, root, Target::Screen)
            .expect("capture X11 root")
            .expect("stable X11 root geometry");
        assert!(root_frame.width >= 2 && root_frame.height >= 2);
        let listed = list_windows().expect("enumerate X11 windows");
        if let Some((id, _)) = listed.first() {
            let id = id
                .strip_prefix("window:")
                .expect("window source prefix")
                .parse()
                .expect("window id");
            let frame = capture(&conn, root, Target::Window(id))
                .expect("capture X11 window")
                .expect("stable X11 window geometry");
            assert!(frame.width >= 2 && frame.height >= 2);
            assert_eq!(
                frame.bgra.len(),
                frame.width as usize * frame.height as usize * 4
            );
        }
    }

    #[test]
    fn window_capture_tracks_resized_drawable_geometry() {
        if !is_x11_session() || std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let (conn, screen_num) = connect().expect("connect to X11");
        let root = conn
            .get_setup()
            .roots()
            .nth(screen_num as usize)
            .expect("root screen")
            .root();
        let win: x::Window = conn.generate_id();
        conn.check_request(conn.send_request_checked(&x::CreateWindow {
            depth: x::COPY_FROM_PARENT as u8,
            wid: win,
            parent: root,
            x: 0,
            y: 0,
            width: 320,
            height: 200,
            border_width: 0,
            class: x::WindowClass::InputOutput,
            visual: x::COPY_FROM_PARENT as x::Visualid,
            value_list: &[],
        }))
        .expect("create X11 test window");
        conn.check_request(conn.send_request_checked(&x::MapWindow { window: win }))
            .expect("map X11 test window");
        conn.flush().expect("flush X11 test window");

        let first = capture_window(&conn, win)
            .expect("capture initial X11 window")
            .expect("stable initial geometry");
        assert_eq!((first.width, first.height), (320, 200));

        conn.check_request(conn.send_request_checked(&x::ConfigureWindow {
            window: win,
            value_list: &[x::ConfigWindow::Width(640), x::ConfigWindow::Height(360)],
        }))
        .expect("resize X11 test window");
        conn.flush().expect("flush resized X11 window");

        let resized = capture_window(&conn, win)
            .expect("capture resized X11 window")
            .expect("stable resized geometry");
        assert_eq!((resized.width, resized.height), (640, 360));

        // Race two X11 clients: resize the drawable while the capture client
        // alternates GetGeometry/GetImage requests. A resize must never turn a
        // transient BadMatch into a fatal capture error.
        let window_id = win.resource_id();
        let resizer = thread::spawn(move || {
            let (resize_conn, _) = connect().expect("connect X11 resize client");
            let resize_win = x::Window::new(window_id);
            for index in 0..40 {
                let (width, height) = if index % 2 == 0 {
                    (320, 200)
                } else {
                    (640, 360)
                };
                let request = resize_conn.send_request_checked(&x::ConfigureWindow {
                    window: resize_win,
                    value_list: &[
                        x::ConfigWindow::Width(width),
                        x::ConfigWindow::Height(height),
                    ],
                });
                resize_conn
                    .check_request(request)
                    .expect("resize X11 window during capture");
                thread::sleep(Duration::from_millis(1));
            }
        });
        for _ in 0..40 {
            if let Some(frame) =
                capture_window(&conn, win).expect("capture must survive a concurrent X11 resize")
            {
                assert!(matches!(
                    (frame.width, frame.height),
                    (320, 200) | (640, 360)
                ));
            }
        }
        resizer.join().expect("X11 resize client");
        let settled = capture_window(&conn, win)
            .expect("capture settled X11 window")
            .expect("settled X11 geometry");
        assert_eq!((settled.width, settled.height), (640, 360));

        conn.check_request(conn.send_request_checked(&x::DestroyWindow { window: win }))
            .expect("destroy X11 test window");
    }

    #[test]
    fn x11_capture_worker_encodes_a_root_frame_when_an_x_server_is_available() {
        if !is_x11_session() || std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel(2);
        let (status_tx, status_rx) = std::sync::mpsc::sync_channel(2);
        let settings = ScreenEncodeConfig {
            bitrate: 250_000,
            max_short: 480,
            force_keyframe: Arc::new(AtomicBool::new(false)),
            target_fps: Arc::new(std::sync::atomic::AtomicU32::new(15)),
            capture_fps: Arc::new(std::sync::atomic::AtomicU32::new(15)),
        };
        let worker = thread::spawn(move || {
            capture_worker(
                worker_stop,
                frame_tx,
                status_tx,
                settings,
                Some("screen:0".into()),
            )
        });
        assert_eq!(
            status_rx
                .recv_timeout(Duration::from_secs(3))
                .expect("X11 capture status"),
            Ok(())
        );

        // Keep the real X11 capture + encoder worker running long enough to
        // exercise sustained frame pacing and emit its five-second diagnostics.
        let deadline = Instant::now() + Duration::from_secs(6);
        let mut frame_count = 0usize;
        let mut first_frame_was_key = false;
        while Instant::now() < deadline {
            let frame = frame_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("continuous X11 capture should keep producing frames");
            if frame_count == 0 {
                first_frame_was_key = frame.key;
                assert!(frame.data.starts_with(&[0, 0, 0, 1]));
            }
            assert!(frame.width >= 2 && frame.height >= 2);
            assert!(!frame.data.is_empty());
            frame_count += 1;
        }
        stop.store(true, Ordering::Release);
        worker
            .join()
            .expect("X11 capture worker")
            .expect("encode X11 frame");
        assert!(first_frame_was_key, "first X11 frame must be an IDR");
        assert!(
            frame_count >= 20,
            "sustained X11 capture produced too few encoded frames: {frame_count}"
        );
        crate::zcall_log!("X11_SUSTAINED_CAPTURE_TEST frames={frame_count} duration_ms=6000");
    }
}
