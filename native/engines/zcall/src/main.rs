//! Standalone Native Call Engine for Zalo Linux (Rust implementation).

use zcall::engine::EngineCore;
use zcall::protocol::callv2_frame::{format_outgoing_frames, FrameEvent, FrameParser};
use zcall::ui::TcpUiTransport;

use std::env;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

fn zalo_main_js_path() -> PathBuf {
    if let Ok(path) = env::var("ZALO_MAIN_JS") {
        return PathBuf::from(path);
    }
    let base = env::var("ZALO_LINUX_DIR")
        .unwrap_or_else(|_| format!("{}/Zalo-Linux", env::var("HOME").unwrap_or_default()));
    PathBuf::from(base).join("app/main-dist/main.js")
}

fn load_callv2_key(main_js: &Path) -> Result<[u8; 16], String> {
    let source = std::fs::read_to_string(main_js)
        .map_err(|error| format!("Failed to read {}: {}", main_js.display(), error))?;
    let marker = "callV2TransportAesKey:\"";
    let start = source
        .find(marker)
        .ok_or_else(|| "callV2TransportAesKey not found".to_string())?
        + marker.len();
    let end = source[start..]
        .find('\"')
        .ok_or_else(|| "Invalid callV2TransportAesKey format".to_string())?
        + start;
    zcall::crypto::decode_transport_key(&source[start..end])
        .map_err(|error| format!("{} in {}", error, main_js.display()))
}

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("zcall - native Zalo call signaling/media engine");
        println!("Usage: zcall [--recv PATH] [--send PATH] [--send-udp] [--group] [--verbose]");
        println!("       zcall --check");
        return;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("zcall {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|arg| arg == "--check") {
        let main_js = zalo_main_js_path();
        match load_callv2_key(&main_js) {
            Ok(_) => {
                println!(
                    "zcall preflight OK; call-v2 transport key loaded from {}",
                    main_js.display()
                );
                return;
            }
            Err(err) => {
                eprintln!("zcall preflight failed: {err}");
                std::process::exit(2);
            }
        }
    }

    let mut recv_target = None;
    let mut send_target = None;
    let mut send_udp = env::var("ZCALL_SEND_UDP").is_ok_and(|v| v == "1");
    let mut is_group = env::var("ZCALL_GROUP").map_or(true, |v| v != "0");
    let mut verbose = env::var("ZCALL_VERBOSE").is_ok_and(|v| !v.is_empty());

    let default_dir = env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let default_recv = format!("{}/socketzalorecv2021", default_dir.trim_end_matches('/'));
    let default_send = format!("{}/socketzalosend2021", default_dir.trim_end_matches('/'));

    let mut i = 1;
    let mut positionals = Vec::new();
    while i < args.len() {
        let arg = &args[i];
        if arg == "--recv" && i + 1 < args.len() {
            recv_target = Some(args[i + 1].clone());
            i += 2;
        } else if arg == "--send" && i + 1 < args.len() {
            send_target = Some(args[i + 1].clone());
            i += 2;
        } else if arg == "--send-udp" {
            send_udp = true;
            i += 1;
        } else if arg == "--group" {
            is_group = true;
            i += 1;
        } else if arg == "--verbose" {
            verbose = true;
            i += 1;
        } else if !arg.starts_with("--") {
            positionals.push(arg.clone());
            i += 1;
        } else {
            i += 1;
        }
    }

    let recv_path = recv_target
        .or_else(|| positionals.first().cloned())
        .unwrap_or(default_recv);
    let send_path = send_target
        .or_else(|| positionals.get(1).cloned())
        .unwrap_or(default_send);

    // Logging setup
    let log_file: Option<File> = if let Ok(log_dir) = env::var("ZCALL_LOG_DIR") {
        let filename = format!("native-engine-{}.log", chrono_timestamp_date());
        let path = PathBuf::from(log_dir).join(filename);
        OpenOptions::new().create(true).append(true).open(path).ok()
    } else {
        None
    };

    log(
        log_file.as_ref(),
        &format!(
            "Starting native zcall Rust engine: send_udp={send_udp}, group={is_group}, sockets: recv={recv_path}, send={send_path}"
        ),
    );

    zcall::logging::init(log_file.as_ref().and_then(|file| file.try_clone().ok()));
    log(log_file.as_ref(), &format!("runtime env: session_type={:?}, wayland_display={}, display={}, ui_port_present={}, ui_token_present={}", env::var("XDG_SESSION_TYPE").ok(), env::var_os("WAYLAND_DISPLAY").is_some(), env::var_os("DISPLAY").is_some(), env::var_os("ZCALL_UI_PORT").is_some(), env::var_os("ZCALL_UI_TOKEN").is_some()));

    let main_js = zalo_main_js_path();
    let key = match load_callv2_key(&main_js) {
        Ok(k) => k,
        Err(e) => {
            log(
                log_file.as_ref(),
                &format!("Failed to load callV2TransportAesKey: {e}"),
            );
            log(log_file.as_ref(), &format!("FATAL: {e}"));
            std::process::exit(1);
        }
    };

    let recv_stream = match UnixStream::connect(&recv_path) {
        Ok(s) => s,
        Err(e) => {
            log(
                log_file.as_ref(),
                &format!("Failed to connect to recv socket {recv_path}: {e}"),
            );
            log(log_file.as_ref(), &format!("FATAL: {e}"));
            std::process::exit(1);
        }
    };

    let send_stream = match UnixStream::connect(&send_path) {
        Ok(s) => s,
        Err(e) => {
            log(
                log_file.as_ref(),
                &format!("Failed to connect to send socket {send_path}: {e}"),
            );
            log(log_file.as_ref(), &format!("FATAL: {e}"));
            std::process::exit(1);
        }
    };

    let _ = recv_stream.set_nonblocking(true);
    let _ = send_stream.set_nonblocking(true);

    let running = Arc::new(AtomicBool::new(true));

    // Signal handlers (SIGINT, SIGTERM)
    let shutdown = Arc::new(AtomicBool::new(false));
    if let Err(error) = signal_hook::flag::register(libc::SIGINT, Arc::clone(&shutdown)) {
        log(
            log_file.as_ref(),
            &format!("Failed to register SIGINT handler: {error}"),
        );
        std::process::exit(1);
    }
    if let Err(error) = signal_hook::flag::register(libc::SIGTERM, Arc::clone(&shutdown)) {
        log(
            log_file.as_ref(),
            &format!("Failed to register SIGTERM handler: {error}"),
        );
        std::process::exit(1);
    }

    let ui = TcpUiTransport::connect_from_env()
        .map(|transport| Box::new(transport) as Box<dyn zcall::ui::UiTransport>);
    let mut engine = EngineCore::new(send_udp, is_group, ui);
    engine.start();

    let mut parser = FrameParser::new(key, false);
    let mut mut_send_stream = send_stream;
    let mut mut_recv_stream = recv_stream;

    let mut read_buf = [0u8; 8192];

    while running.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed) {
        // 1. Read from Zalo
        match mut_send_stream.read(&mut read_buf) {
            Ok(0) => {
                log(
                    log_file.as_ref(),
                    "send socket EOF from Zalo, shutting down.",
                );
                break;
            }
            Ok(n) => {
                let events = parser.push(&read_buf[..n]);
                for ev in events {
                    match ev {
                        FrameEvent::Json(val) => {
                            if verbose {
                                log(log_file.as_ref(), &format!("<- Zalo JSON: {val}"));
                            }
                            engine.on_zalo_frame(&val);
                        }
                        FrameEvent::Raw(raw) => {
                            if verbose {
                                log(log_file.as_ref(), &format!("<- Zalo RAW: {raw}"));
                            }
                        }
                        FrameEvent::Error(err) => {
                            log(log_file.as_ref(), &format!("Frame parse error: {err}"));
                        }
                        FrameEvent::Token(_) => {}
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // No data right now
            }
            Err(e) => {
                log(
                    log_file.as_ref(),
                    &format!("Error reading from send socket: {e}"),
                );
                break;
            }
        }

        // 2. Poll UI actions
        engine.poll_ui_actions();

        // 3. Send outgoing frames to Zalo
        let outgoing = engine.poll_outgoing_frames();
        for frame in outgoing {
            let frame_type = frame
                .get("type")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let command = frame
                .get("command")
                .map_or_else(|| "?".into(), std::string::ToString::to_string);
            if frame_type == "sendSignal"
                || frame_type == "request"
                || frame_type == "response"
                || frame_type == "update"
            {
                log(
                    log_file.as_ref(),
                    &format!("outgoing Zalo event: type={frame_type}, command={command}"),
                );
            }
            if verbose {
                log(log_file.as_ref(), &format!("-> Zalo JSON: {frame}"));
            }
            let bytes = format_outgoing_frames(&frame, &key);
            if let Err(e) = mut_recv_stream.write_all(&bytes) {
                log(
                    log_file.as_ref(),
                    &format!("Error writing to recv socket: {e}"),
                );
                break;
            }
            let _ = mut_recv_stream.flush();
        }

        std::thread::sleep(Duration::from_millis(5));
    }

    engine.hangup("engine_exit");
    log(log_file.as_ref(), "zcall engine shutdown complete.");
}

fn log(file_opt: Option<&File>, msg: &str) {
    let time = chrono_timestamp_time();
    let line = format!("{time} [zcall-rust] {msg}\n");
    if let Some(file) = file_opt {
        let mut f = file;
        let _ = f.write_all(line.as_bytes());
        let _ = f.flush();
    } else {
        eprint!("{line}");
    }
}

fn chrono_timestamp_date() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now / 86400;
    // Approximated ISO date format YYYYMMDD
    let mut year = 1970;
    let mut d = days;
    loop {
        let leap = if (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0) {
            366
        } else {
            365
        };
        if d < leap {
            break;
        }
        d -= leap;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let m_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for &md in &m_days {
        if d < md {
            break;
        }
        d -= md;
        month += 1;
    }
    let day = d + 1;
    format!("{year:04}{month:02}{day:02}")
}

fn chrono_timestamp_time() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() % 86400;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let ms = now.subsec_millis();
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
#[path = "tests/main.rs"]
mod tests;
