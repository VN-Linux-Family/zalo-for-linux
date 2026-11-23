use super::*;

fn test_connection() -> Option<(RustConnection, x11::Window)> {
    if !is_x11_session() || std::env::var_os("DISPLAY").is_none() {
        return None;
    }
    let (conn, screen_num) = connect_x11rb().expect("connect to X11");
    let root = conn
        .setup()
        .roots
        .get(screen_num)
        .expect("root screen")
        .root;
    Some((conn, root))
}

fn capture(
    conn: &RustConnection,
    root: x11::Window,
    target: Target,
) -> Result<Option<RawScreenFrame>, String> {
    let mut shm_capture = PersistentShm::new(conn)?;
    let mut window_capture = None;
    capture_with_shm(conn, root, target, &mut shm_capture, &mut window_capture)
}

fn capture_window(
    conn: &RustConnection,
    root: x11::Window,
    win: x11::Window,
) -> Result<Option<RawScreenFrame>, String> {
    let mut shm_capture = PersistentShm::new(conn)?;
    let mut window_capture = None;
    let mut pointer_is_over_window = false;
    capture_window_with_shm(
        conn,
        root,
        win,
        &mut shm_capture,
        &mut window_capture,
        &mut pointer_is_over_window,
    )
}

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
fn cursor_is_visible_only_when_every_pointer_ancestor_selects_the_target_path() {
    let path = [10, 20, 30];

    assert!(pointer_path_matches(&path, &[20, 30]));
    assert!(!pointer_path_matches(&path, &[40, 30]));
    assert!(!pointer_path_matches(&path, &[20, 40]));
    assert!(!pointer_path_matches(&path, &[0, 0]));
    assert!(!pointer_path_matches(&path, &[20]));
    assert!(!pointer_path_matches(&path, &[20, 30, 40]));
}

#[test]
fn xfixes_cursor_position_matches_the_x11_pointer_hotspot() {
    let Some((conn, root)) = test_connection() else {
        return;
    };
    let pointer = x11::query_pointer(&conn, root)
        .expect("queue pointer query")
        .reply()
        .expect("query actual X11 pointer position");
    let cursor = match xfixes::get_cursor_image(&conn)
        .expect("queue XFixes cursor image")
        .reply()
    {
        Ok(cursor) => cursor,
        Err(_) => return,
    };
    assert_eq!(
        (cursor.x, cursor.y),
        (pointer.root_x, pointer.root_y),
        "XFixes cursor coordinates must identify the pointer hotspot in root coordinates"
    );
    assert!(cursor.width > 0 && cursor.height > 0);
    assert_eq!(
        cursor.cursor_image.len(),
        usize::from(cursor.width) * usize::from(cursor.height),
        "XFixes cursor image must contain every ARGB pixel"
    );
}

#[test]
fn root_capture_composites_the_live_xfixes_cursor_when_available() {
    let Some((conn, root)) = test_connection() else {
        return;
    };
    let screen = conn
        .setup()
        .roots
        .iter()
        .find(|screen| screen.root == root)
        .expect("root screen");
    let cursor = match xfixes::get_cursor_image(&conn)
        .expect("queue XFixes cursor image")
        .reply()
    {
        Ok(cursor) => cursor,
        Err(_) => return,
    };
    let mut frame = RawScreenFrame {
        bgra: [24, 48, 72, 255]
            .repeat(usize::from(screen.width_in_pixels) * usize::from(screen.height_in_pixels)),
        width: u32::from(screen.width_in_pixels),
        height: u32::from(screen.height_in_pixels),
    };
    overlay_cursor_reply(&cursor, (0, 0), &mut frame);

    let left = i32::from(cursor.x) - i32::from(cursor.xhot);
    let top = i32::from(cursor.y) - i32::from(cursor.yhot);
    let cursor_width = usize::from(cursor.width);
    let cursor_height = usize::from(cursor.height);
    let mut has_visible_pixel = false;
    for y in 0..cursor_height {
        for x in 0..cursor_width {
            let pixel = cursor.cursor_image[y * cursor_width + x];
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
    let Some((conn, root)) = test_connection() else {
        return;
    };
    let root_frame = capture(&conn, root, Target::Screen)
        .expect("capture X11 root")
        .expect("stable X11 root geometry");
    assert!(root_frame.width >= 2 && root_frame.height >= 2);
    let listed = list_windows().expect("enumerate X11 windows");
    assert!(listed
        .iter()
        .all(|(id, title)| id.starts_with("window:") && !title.is_empty()));
}

#[test]
fn window_capture_tracks_resized_drawable_geometry() {
    let Some((conn, root)) = test_connection() else {
        return;
    };
    let win = conn.generate_id().expect("allocate test window id");
    let values = x11::CreateWindowAux::new().override_redirect(1);
    x11::create_window(
        &conn,
        x11rb::COPY_FROM_PARENT as u8,
        win,
        root,
        20,
        20,
        320,
        200,
        0,
        x11::WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &values,
    )
    .expect("queue test window creation")
    .check()
    .expect("create X11 test window");
    x11::map_window(&conn, win)
        .expect("queue map test window")
        .check()
        .expect("map X11 test window");
    x11::configure_window(
        &conn,
        win,
        &x11::ConfigureWindowAux::new().stack_mode(x11::StackMode::ABOVE),
    )
    .expect("queue raise test window")
    .check()
    .expect("raise X11 test window");
    conn.flush().expect("flush X11 test window");

    let first = capture_window(&conn, root, win)
        .expect("capture initial X11 window")
        .expect("stable initial geometry");
    assert_eq!((first.width, first.height), (320, 200));

    x11::configure_window(
        &conn,
        win,
        &x11::ConfigureWindowAux::new()
            .width(640)
            .height(360)
            .stack_mode(x11::StackMode::ABOVE),
    )
    .expect("queue resize test window")
    .check()
    .expect("resize X11 test window");
    conn.flush().expect("flush resized X11 test window");

    let resized = capture_window(&conn, root, win)
        .expect("capture resized X11 window")
        .expect("stable resized geometry");
    assert_eq!((resized.width, resized.height), (640, 360));

    // Race two x11rb clients: resize the drawable while capture alternates
    // GetGeometry/GetImage. Transient resize BadMatch must not be fatal.
    let resize_win = win;
    let resize_worker = thread::spawn(move || {
        let (resize_conn, _) = connect_x11rb().expect("connect X11 resize client");
        for index in 0..40 {
            let (width, height) = if index % 2 == 0 {
                (320, 200)
            } else {
                (640, 360)
            };
            x11::configure_window(
                &resize_conn,
                resize_win,
                &x11::ConfigureWindowAux::new().width(width).height(height),
            )
            .expect("queue concurrent X11 resize")
            .check()
            .expect("resize X11 window during capture");
            thread::sleep(Duration::from_millis(1));
        }
    });
    for _ in 0..40 {
        if let Some(frame) =
            capture_window(&conn, root, win).expect("capture must survive a concurrent X11 resize")
        {
            assert!(matches!(
                (frame.width, frame.height),
                (320, 200) | (640, 360)
            ));
        }
    }
    resize_worker.join().expect("X11 resize client");
    let settled = capture_window(&conn, root, win)
        .expect("capture settled X11 window")
        .expect("settled X11 geometry");
    assert_eq!((settled.width, settled.height), (640, 360));

    x11::destroy_window(&conn, win)
        .expect("queue destroy test window")
        .check()
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
