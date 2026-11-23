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
        bgra: [24, 48, 72, 255]
            .repeat(usize::from(screen.width_in_pixels()) * usize::from(screen.height_in_pixels())),
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
    let resize_worker = thread::spawn(move || {
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
    resize_worker.join().expect("X11 resize client");
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
        assert_ne!(frame.data, [] as [u8; 0]);
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
