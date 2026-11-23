//! Pure-Rust X11 capture through x11rb. Wayland stays portal-only.
use super::{RawScreenFrame, ScreenEncodeConfig};
use std::{
    fs::File,
    os::unix::fs::FileExt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::SyncSender,
    },
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{composite, shm, xfixes, xproto as x11},
    rust_connection::RustConnection,
};

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

fn connect_x11rb() -> Result<(RustConnection, usize), String> {
    let (conn, screen_num) = RustConnection::connect(None)
        .map_err(|error| format!("Connect to X11 display: {error}"))?;

    match xfixes::query_version(&conn, 6, 0)
        .map_err(|error| format!("{error:?}"))
        .and_then(|cookie| cookie.reply().map_err(|error| format!("{error:?}")))
    {
        Ok(version) if version.major_version >= 2 => crate::zcall_log!(
            "[zcall-screen] XFixes negotiated version {}.{}",
            version.major_version,
            version.minor_version
        ),
        Ok(version) => crate::zcall_log!(
            "[zcall-screen] XFixes {}.{} does not support cursor images",
            version.major_version,
            version.minor_version
        ),
        Err(error) => {
            crate::zcall_log!("[zcall-screen] XFixes version negotiation failed: {error}");
        }
    }

    Ok((conn, screen_num))
}

fn atom(conn: &RustConnection, name: &[u8]) -> Result<x11::Atom, String> {
    x11::intern_atom(conn, false, name)
        .map_err(|error| format!("Send X11 InternAtom: {error:?}"))?
        .reply()
        .map(|reply| reply.atom)
        .map_err(|error| format!("Resolve X11 atom: {error:?}"))
}

fn prop(
    conn: &RustConnection,
    win: x11::Window,
    property: x11::Atom,
    type_: x11::Atom,
) -> Result<Vec<u8>, String> {
    x11::get_property(conn, false, win, property, type_, 0, 16384)
        .map_err(|error| format!("Send X11 GetProperty: {error:?}"))?
        .reply()
        .map(|reply| reply.value)
        .map_err(|error| format!("Read X11 property: {error:?}"))
}

fn title(conn: &RustConnection, win: x11::Window, net_name: x11::Atom, utf8: x11::Atom) -> String {
    prop(conn, win, net_name, utf8)
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            prop(
                conn,
                win,
                x11::AtomEnum::WM_NAME.into(),
                x11::AtomEnum::STRING.into(),
            )
            .ok()
        })
        .map_or_else(String::new, |value| {
            String::from_utf8_lossy(&value)
                .trim_matches('\0')
                .trim()
                .to_owned()
        })
}

fn windows(conn: &RustConnection, root: x11::Window) -> Result<Vec<x11::Window>, String> {
    let list = atom(conn, b"_NET_CLIENT_LIST_STACKING")?;
    let reply = x11::get_property(conn, false, root, list, x11::AtomEnum::ANY, 0, 16384)
        .map_err(|error| format!("Send X11 client-list request: {error:?}"))?
        .reply()
        .map_err(|error| format!("Read X11 client list: {error:?}"))?;

    let result: Vec<_> = if reply.format == 32 {
        reply
            .value
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| u32::from_ne_bytes(*bytes))
            .filter(|window| *window != 0)
            .collect()
    } else {
        Vec::new()
    };
    if !result.is_empty() {
        return Ok(result);
    }

    x11::query_tree(conn, root)
        .map_err(|error| format!("Send X11 QueryTree: {error:?}"))?
        .reply()
        .map(|reply| reply.children)
        .map_err(|error| format!("Enumerate X11 windows: {error:?}"))
}

pub fn list_windows() -> Result<Vec<(String, String)>, String> {
    if !is_x11_session() {
        return Err("X11 window selection is available only in an X11 session".into());
    }
    let (conn, screen_num) = connect_x11rb()?;
    let screen = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or("X11 display has no default screen")?;
    let (root, net_name, utf8) = (
        screen.root,
        atom(&conn, b"_NET_WM_NAME")?,
        atom(&conn, b"UTF8_STRING")?,
    );

    let mut out = Vec::new();
    for win in windows(&conn, root)? {
        let name = title(&conn, win, net_name, utf8);
        if name.is_empty() || name.starts_with("Zalo Call") {
            continue;
        }
        let attrs = x11::get_window_attributes(&conn, win)
            .map_err(|error| format!("{error:?}"))
            .and_then(|cookie| cookie.reply().map_err(|error| format!("{error:?}")));
        if !attrs.is_ok_and(|attributes| attributes.map_state == x11::MapState::VIEWABLE) {
            continue;
        }
        let geometry = x11::get_geometry(&conn, win)
            .map_err(|error| format!("{error:?}"))
            .and_then(|cookie| cookie.reply().map_err(|error| format!("{error:?}")));
        if !geometry.is_ok_and(|geometry| geometry.width >= 64 && geometry.height >= 48) {
            continue;
        }
        out.push((format!("window:{win}"), name));
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
        None | Some("" | "screen" | "screen:0") => Ok(Target::Screen),
        Some(value) if value.starts_with("window:") => {
            let id = value[7..]
                .parse::<u32>()
                .map_err(|_| "Invalid X11 window id")?;
            if id == 0 {
                Err("Invalid X11 window id".into())
            } else {
                Ok(Target::Window(id))
            }
        }
        _ => Err("Unsupported X11 capture source".into()),
    }
}

/// MIT-SHM 1.2 FD-backed capture. No GetImage fallback: capture fails if the
/// server cannot provide a shared-memory segment.
struct PersistentShm<'a> {
    conn: &'a RustConnection,
    segment_file: Option<File>,
    shmseg: Option<shm::Seg>,
    capacity: usize,
}

impl<'a> PersistentShm<'a> {
    fn new(conn: &'a RustConnection) -> Result<Self, String> {
        let version = shm::query_version(conn)
            .map_err(|error| format!("Query MIT-SHM version: {error:?}"))?
            .reply()
            .map_err(|error| format!("Read MIT-SHM version: {error:?}"))?;
        if version.major_version < 1 || (version.major_version == 1 && version.minor_version < 2) {
            return Err(format!(
                "MIT-SHM 1.2 FD segments are required (server has {}.{})",
                version.major_version, version.minor_version
            ));
        }
        crate::zcall_log!(
            "[zcall-screen] MIT-SHM negotiated version {}.{}",
            version.major_version,
            version.minor_version
        );
        Ok(Self {
            conn,
            segment_file: None,
            shmseg: None,
            capacity: 0,
        })
    }

    fn detach(&mut self) {
        if let Some(shmseg) = self.shmseg.take() {
            if let Ok(cookie) = shm::detach(self.conn, shmseg) {
                let _ = cookie.check();
            }
        }
        self.segment_file = None;
        self.capacity = 0;
    }

    fn ensure_capacity(&mut self, required: usize) -> Result<(), String> {
        if self.capacity >= required && self.segment_file.is_some() {
            return Ok(());
        }
        self.detach();

        let capacity = required.max(4096);
        let size =
            u32::try_from(capacity).map_err(|_| "MIT-SHM frame exceeds the protocol size limit")?;
        let shmseg = self
            .conn
            .generate_id()
            .map_err(|error| format!("Allocate MIT-SHM resource id: {error:?}"))?;
        let reply = shm::create_segment(self.conn, shmseg, size, false)
            .map_err(|error| format!("Create MIT-SHM FD segment: {error:?}"))?
            .reply()
            .map_err(|error| format!("Receive MIT-SHM FD segment: {error:?}"))?;
        if reply.nfd != 1 {
            if let Ok(cookie) = shm::detach(self.conn, shmseg) {
                let _ = cookie.check();
            }
            return Err(format!("MIT-SHM returned {} FDs instead of one", reply.nfd));
        }

        let segment_file = File::from(reply.shm_fd);
        let metadata = segment_file
            .metadata()
            .map_err(|error| format!("Inspect MIT-SHM FD: {error}"))?;
        if metadata.len() < capacity as u64 {
            if let Ok(cookie) = shm::detach(self.conn, shmseg) {
                let _ = cookie.check();
            }
            return Err(format!(
                "MIT-SHM FD is only {} bytes; expected {capacity}",
                metadata.len()
            ));
        }
        self.segment_file = Some(segment_file);
        self.shmseg = Some(shmseg);
        self.capacity = capacity;
        Ok(())
    }

    fn get_image(
        &mut self,
        drawable: x11::Drawable,
        width: u16,
        height: u16,
        required: usize,
    ) -> Result<Vec<u8>, String> {
        self.ensure_capacity(required)?;
        let shmseg = self.shmseg.ok_or("MIT-SHM segment is unavailable")?;
        let reply = shm::get_image(
            self.conn,
            drawable,
            0,
            0,
            width,
            height,
            u32::MAX,
            x11::ImageFormat::Z_PIXMAP.into(),
            shmseg,
            0,
        )
        .map_err(|error| format!("Send MIT-SHM GetImage: {error:?}"))?
        .reply()
        .map_err(|error| format!("MIT-SHM image capture: {error:?}"))?;
        if (reply.size as usize) < required {
            return Err("MIT-SHM frame is truncated".into());
        }

        let mut bytes = vec![0; required];
        self.segment_file
            .as_ref()
            .ok_or("MIT-SHM FD is unavailable")?
            .read_exact_at(&mut bytes, 0)
            .map_err(|error| format!("Read MIT-SHM frame: {error}"))?;
        Ok(bytes)
    }
}

impl Drop for PersistentShm<'_> {
    fn drop(&mut self) {
        self.detach();
    }
}

fn capture_drawable(
    conn: &RustConnection,
    drawable: x11::Drawable,
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
        .setup()
        .pixmap_formats
        .iter()
        .find(|format| format.depth == depth)
        .ok_or("Unknown X11 pixel format")?;
    let bpp = usize::from(format.bits_per_pixel);
    if bpp != 32 {
        return Err(format!("Unsupported X11 pixel format ({bpp} bpp)"));
    }
    let pad = usize::from(format.scanline_pad).max(8);
    let stride = (usize::from(width) * bpp).div_ceil(pad) * (pad / 8);
    let required = stride * usize::from(height);
    let source = shm_capture
        .get_image(drawable, width, height, required)
        .map_err(|error| format!("Capture X11 frame: {error}"))?;

    let pixel_bytes = usize::from(width) * usize::from(height) * 4;
    let lsb_first = conn.setup().image_byte_order == x11::ImageOrder::LSB_FIRST;
    let bgra = if lsb_first && stride == usize::from(width) * 4 {
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
                if lsb_first {
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
    let frame_width = usize::try_from(frame.width).unwrap_or(usize::MAX);
    let frame_height = usize::try_from(frame.height).unwrap_or(usize::MAX);
    let frame_width_i64 = i64::from(frame.width);
    let frame_height_i64 = i64::from(frame.height);
    if frame_width == 0
        || frame_height == 0
        || pixels.len() < cursor_width.saturating_mul(cursor_height)
        || frame.bgra.len() < frame_width.saturating_mul(frame_height).saturating_mul(4)
    {
        return;
    }

    for cursor_y in 0..cursor_height {
        let Ok(y_offset) = i64::try_from(cursor_y) else {
            continue;
        };
        let dest_y = i64::from(top) + y_offset;
        if !(0..frame_height_i64).contains(&dest_y) {
            continue;
        }
        for cursor_x in 0..cursor_width {
            let Ok(x_offset) = i64::try_from(cursor_x) else {
                continue;
            };
            let dest_x = i64::from(left) + x_offset;
            if !(0..frame_width_i64).contains(&dest_x) {
                continue;
            }
            let argb = pixels[cursor_y * cursor_width + cursor_x];
            let alpha = (argb >> 24) & 0xff;
            if alpha == 0 {
                continue;
            }
            let inverse_alpha = 255 - alpha;
            let dest_y = usize::try_from(dest_y).expect("cursor y was checked non-negative");
            let dest_x = usize::try_from(dest_x).expect("cursor x was checked non-negative");
            let offset = (dest_y * frame_width + dest_x) * 4;
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

fn overlay_cursor_reply(
    cursor: &xfixes::GetCursorImageReply,
    origin: (i32, i32),
    frame: &mut RawScreenFrame,
) {
    let left = i32::from(cursor.x) - i32::from(cursor.xhot) - origin.0;
    let top = i32::from(cursor.y) - i32::from(cursor.yhot) - origin.1;
    let cursor_width = usize::from(cursor.width);
    let cursor_height = usize::from(cursor.height);
    if cursor_width == 0
        || cursor_height == 0
        || cursor.cursor_image.len() < cursor_width.saturating_mul(cursor_height)
    {
        return;
    }
    composite_cursor(
        frame,
        left,
        top,
        cursor_width,
        cursor_height,
        &cursor.cursor_image,
    );
}

fn pointer_path(
    conn: &RustConnection,
    root: x11::Window,
    window: x11::Window,
) -> Result<Vec<x11::Window>, String> {
    let mut path = vec![window];
    let mut current = window;

    while current != root {
        if path.len() >= 64 {
            return Err("X11 window ancestry exceeds 64 levels".into());
        }
        let tree = x11::query_tree(conn, current)
            .map_err(|error| format!("Queue X11 window ancestry query: {error:?}"))?
            .reply()
            .map_err(|error| format!("Read X11 window ancestry: {error:?}"))?;
        if tree.parent == 0 || tree.parent == current {
            return Err("X11 target window is not a child of the capture root".into());
        }
        current = tree.parent;
        path.push(current);
    }

    path.reverse();
    Ok(path)
}

fn pointer_path_matches(path: &[x11::Window], children: &[x11::Window]) -> bool {
    path.len() >= 2
        && children.len() == path.len() - 1
        && path
            .windows(2)
            .zip(children)
            .all(|(parent_child, actual_child)| parent_child[1] == *actual_child)
}

/// Capture cursor and window origin requests are queued before image capture
/// to overlap their server processing with the potentially larger pixel read.
fn capture_with_shm<'a>(
    conn: &'a RustConnection,
    root: x11::Window,
    target: Target,
    shm_capture: &mut PersistentShm<'_>,
    window_capture: &mut Option<CompositeWindowPixmap<'a>>,
) -> Result<Option<RawScreenFrame>, String> {
    let cursor_cookie = xfixes::get_cursor_image(conn).ok();
    let window_position_cookie = match target {
        Target::Screen => None,
        Target::Window(id) => x11::translate_coordinates(conn, id, root, 0, 0).ok(),
    };
    let mut pointer_is_over_window = false;

    let capture_result = match target {
        Target::Screen => conn
            .setup()
            .roots
            .iter()
            .find(|screen| screen.root == root)
            .ok_or_else(|| "X11 root disappeared".to_string())
            .and_then(|screen| {
                capture_drawable(
                    conn,
                    root,
                    screen.width_in_pixels,
                    screen.height_in_pixels,
                    screen.root_depth,
                    shm_capture,
                )
                .map(Some)
            }),
        Target::Window(id) => capture_window_with_shm(
            conn,
            root,
            id,
            shm_capture,
            window_capture,
            &mut pointer_is_over_window,
        ),
    };

    let cursor_result = cursor_cookie.map(|cookie| {
        cookie
            .reply()
            .map_err(|error| format!("Read XFixes cursor image: {error:?}"))
    });
    let origin_result = match target {
        Target::Screen => Ok((0, 0)),
        Target::Window(_) => window_position_cookie.map_or_else(
            || Err("Queue X11 window-origin request failed".into()),
            |cookie| {
                cookie
                    .reply()
                    .map(|position| (i32::from(position.dst_x), i32::from(position.dst_y)))
                    .map_err(|error| format!("Translate X11 window origin: {error:?}"))
            },
        ),
    };

    let mut frame = capture_result?;
    if let Some(frame) = frame.as_mut() {
        match (cursor_result, origin_result) {
            (Some(Ok(cursor)), Ok(origin))
                if matches!(target, Target::Screen) || pointer_is_over_window =>
            {
                overlay_cursor_reply(&cursor, origin, frame);
            }
            (Some(Ok(_)), Ok(_)) => {}
            (Some(Err(error)), _) => {
                let count = CURSOR_IMAGE_ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
                if count == 1 || count.is_multiple_of(60) {
                    crate::zcall_log!(
                        "[zcall-screen] XFixes cursor image request failed {count} times: {error}"
                    );
                }
            }
            (None, _) => {}
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

/// Keeps an off-screen Composite pixmap for a selected window. Reading the
/// window drawable directly with GetImage only guarantees valid pixels for
/// unobscured regions; the named pixmap remains independent of occlusion.
struct CompositeWindowPixmap<'a> {
    conn: &'a RustConnection,
    window: x11::Window,
    pixmap: x11::Pixmap,
    width: u16,
    height: u16,
    depth: u8,
    redirected_here: bool,
    pointer_path: Vec<x11::Window>,
}

impl<'a> CompositeWindowPixmap<'a> {
    fn new(
        conn: &'a RustConnection,
        root: x11::Window,
        window: x11::Window,
        width: u16,
        height: u16,
        depth: u8,
    ) -> Result<Self, String> {
        composite::query_version(conn, 0, 4)
            .map_err(|error| format!("Query X11 Composite version: {error:?}"))?
            .reply()
            .map_err(|error| format!("Read X11 Composite version: {error:?}"))?;

        let mut capture = Self {
            conn,
            window,
            pixmap: 0,
            width,
            height,
            depth,
            redirected_here: false,
            pointer_path: pointer_path(conn, root, window).unwrap_or_default(),
        };
        capture.name_pixmap()?;
        Ok(capture)
    }

    fn name_pixmap(&mut self) -> Result<(), String> {
        let pixmap = self
            .conn
            .generate_id()
            .map_err(|error| format!("Allocate X11 Composite pixmap id: {error:?}"))?;

        let named = composite::name_window_pixmap(self.conn, self.window, pixmap)
            .map_err(|error| format!("Queue X11 Composite NameWindowPixmap: {error:?}"))?
            .check();

        if let Err(first_error) = named {
            // A window without an existing compositor redirect has no named
            // pixmap yet. Redirect it automatically (not manually), preserving
            // normal on-screen painting, then retry.
            composite::redirect_window(
                self.conn,
                self.window,
                composite::Redirect::AUTOMATIC,
            )
            .map_err(|error| format!("Queue X11 Composite RedirectWindow: {error:?}"))?
            .check()
            .map_err(|error| {
                format!("X11 window has no capturable Composite pixmap ({first_error}); redirect failed: {error}")
            })?;
            self.redirected_here = true;
            composite::name_window_pixmap(self.conn, self.window, pixmap)
                .map_err(|error| format!("Queue X11 Composite NameWindowPixmap: {error:?}"))?
                .check()
                .map_err(|error| format!("Name redirected X11 window pixmap: {error}"))?;
        }

        self.pixmap = pixmap;
        Ok(())
    }

    fn refresh(&mut self, width: u16, height: u16, depth: u8) -> Result<(), String> {
        if (self.width, self.height, self.depth) == (width, height, depth) {
            return Ok(());
        }
        if self.pixmap != 0 {
            if let Ok(cookie) = x11::free_pixmap(self.conn, self.pixmap) {
                let _ = cookie.check();
            }
            self.pixmap = 0;
        }
        self.width = width;
        self.height = height;
        self.depth = depth;
        self.name_pixmap()
    }
}

impl Drop for CompositeWindowPixmap<'_> {
    fn drop(&mut self) {
        if self.pixmap != 0 {
            if let Ok(cookie) = x11::free_pixmap(self.conn, self.pixmap) {
                let _ = cookie.check();
            }
        }
        if self.redirected_here {
            if let Ok(cookie) =
                composite::unredirect_window(self.conn, self.window, composite::Redirect::AUTOMATIC)
            {
                let _ = cookie.check();
            }
        }
    }
}

fn capture_window_with_shm<'a>(
    conn: &'a RustConnection,
    root: x11::Window,
    win: x11::Window,
    shm_capture: &mut PersistentShm<'_>,
    window_capture: &mut Option<CompositeWindowPixmap<'a>>,
    pointer_is_over_window: &mut bool,
) -> Result<Option<RawScreenFrame>, String> {
    const RESIZE_RETRIES: usize = 3;
    let mut geometry_changed = false;
    let mut last_bad_match = None;

    for _ in 0..RESIZE_RETRIES {
        let geometry = x11::get_geometry(conn, win)
            .map_err(|error| format!("Queue X11 window geometry: {error:?}"))?
            .reply()
            .map_err(|error| format!("X11 window unavailable: {error:?}"))?;
        let (width, height, depth) = (geometry.width, geometry.height, geometry.depth);

        if window_capture.is_none() {
            *window_capture = Some(CompositeWindowPixmap::new(
                conn, root, win, width, height, depth,
            )?);
        }
        let capture = window_capture
            .as_mut()
            .ok_or("X11 Composite capture is unavailable")?;
        capture.refresh(width, height, depth)?;
        if capture.pointer_path.is_empty() {
            capture.pointer_path = pointer_path(conn, root, win).unwrap_or_default();
        }

        // Queue each ancestor hit-test before reading pixels so the server can
        // process them alongside MIT-SHM without adding a roundtrip per level.
        let pointer_cookies = capture
            .pointer_path
            .windows(2)
            .map(|pair| {
                x11::query_pointer(conn, pair[0])
                    .map(|cookie| (cookie, pair[1]))
                    .map_err(|error| format!("Queue X11 pointer hit-test: {error:?}"))
            })
            .collect::<Result<Vec<_>, _>>();
        let frame = capture_drawable(conn, capture.pixmap, width, height, depth, shm_capture);
        let mut refresh_pointer_path = false;
        *pointer_is_over_window = pointer_cookies.is_ok_and(|cookies| {
            let mut children = Vec::with_capacity(cookies.len());
            let mut same_screen = true;
            for (cookie, _) in cookies {
                match cookie.reply() {
                    Ok(reply) if reply.same_screen => children.push(reply.child),
                    Ok(_) => same_screen = false,
                    Err(_) => {
                        same_screen = false;
                        refresh_pointer_path = true;
                    }
                }
            }
            same_screen && pointer_path_matches(&capture.pointer_path, &children)
        });
        if refresh_pointer_path {
            capture.pointer_path = pointer_path(conn, root, win).unwrap_or_default();
        }

        let current = x11::get_geometry(conn, win)
            .map_err(|error| format!("Queue X11 window geometry recheck: {error:?}"))?
            .reply()
            .map_err(|error| format!("X11 window disappeared during capture: {error:?}"))?;
        let dimensions_unchanged =
            current.width == width && current.height == height && current.depth == depth;

        match (frame, dimensions_unchanged) {
            (Ok(frame), true) => return Ok(Some(frame)),
            (Ok(_), false) => geometry_changed = true,
            (Err(error), false) => {
                geometry_changed = true;
                last_bad_match = Some(error);
            }
            (Err(error), true) if error.contains("Match") || error.contains("BadMatch") => {
                last_bad_match = Some(error);
                thread::sleep(Duration::from_millis(2));
            }
            (Err(error), true) => return Err(error),
        }
    }

    if geometry_changed {
        Ok(None)
    } else if let Some(error) = last_bad_match {
        Err(error)
    } else {
        Ok(None)
    }
}

// These owned values are intentionally moved into the worker's encoder thread.
// Floating-point conversion is acceptable for approximate diagnostic FPS/timing only.
#[allow(clippy::needless_pass_by_value, clippy::cast_precision_loss)]
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
    let (conn, screen_num) = connect_x11rb()?;
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .ok_or("X11 display has no default screen")?
        .root;
    let mut shm_capture = PersistentShm::new(&conn)?;
    let mut window_capture = None;
    let raw_frames = Arc::new(super::LatestFrameQueue::default());
    let encoder_raw_frames = raw_frames.clone();
    let enc_stop = stop.clone();
    let enc_settings = settings.clone();
    let enc_status = status;
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
        .map_err(|error| format!("Start X11 encoder: {error}"))?;

    let startup_deadline = Instant::now() + Duration::from_secs(15);
    let mut stats_started = Instant::now();
    let mut captured_frames = 0u64;
    let mut overwritten_frames = 0u64;
    let mut total_capture_ms = 0.0f64;
    let mut max_capture_ms = 0.0f64;
    let capture_result = (|| {
        while !stop.load(Ordering::Acquire) {
            if Instant::now() >= startup_deadline && !encoded_frame_seen.load(Ordering::Acquire) {
                return Err("Timed out waiting for the first encoded X11 screen frame".into());
            }
            let start = Instant::now();
            let captured =
                capture_with_shm(&conn, root, target, &mut shm_capture, &mut window_capture)?;
            let Some(frame) = captured else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            if stop.load(Ordering::Acquire) {
                break;
            }
            if raw_frames.push(frame) {
                overwritten_frames += 1;
            }

            let elapsed = start.elapsed();
            let capture_ms = elapsed.as_secs_f64() * 1000.0;
            captured_frames += 1;
            total_capture_ms += capture_ms;
            max_capture_ms = max_capture_ms.max(capture_ms);
            if stats_started.elapsed() >= Duration::from_secs(5) {
                let window_seconds = stats_started.elapsed().as_secs_f64();
                let measured_fps = captured_frames as f64 / window_seconds;
                let average_ms = total_capture_ms / captured_frames.max(1) as f64;
                crate::zcall_log!(
                    "[zcall-screen] X11 capture stats: fps={measured_fps:.1}, avg_ms={average_ms:.1}, max_ms={max_capture_ms:.1}, overwritten={overwritten_frames}"
                );
                stats_started = Instant::now();
                captured_frames = 0;
                overwritten_frames = 0;
                total_capture_ms = 0.0;
                max_capture_ms = 0.0;
            }
        }
        Ok(())
    })();

    if capture_result.is_err() {
        stop.store(true, Ordering::Release);
    }
    let _ = encoder.join();
    capture_result
}

#[cfg(test)]
#[path = "../tests/screen/x11.rs"]
mod tests;
