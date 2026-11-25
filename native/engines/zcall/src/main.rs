//! Standalone Native Call Engine for Zalo Linux (Rust implementation).

use zcall::engine::EngineCore;
use zcall::protocol::callv2_frame::{format_outgoing_frames, FrameEvent, FrameParser};
use zcall::ui::TcpUiTransport;

use signal_hook::consts::{SIGINT, SIGTERM};

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

struct RuntimeOptions {
    recv_path: String,
    send_path: String,
    send_udp: bool,
    is_group: bool,
    verbose: bool,
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if handle_early_args(&args) {
        return;
    }

    let options = parse_runtime_options(&args);
    let log_file = setup_log_file();
    log_startup(log_file.as_ref(), &options);
    zcall::logging::init(log_file.as_ref().and_then(|file| file.try_clone().ok()));
    log_runtime_environment(log_file.as_ref());

    let main_js = zalo_main_js_path();
    let key = load_transport_key(&main_js, log_file.as_ref());
    let recv_stream = connect_socket(&options.recv_path, "recv", log_file.as_ref());
    let send_stream = connect_socket(&options.send_path, "send", log_file.as_ref());

    run_engine(&options, key, recv_stream, send_stream, log_file.as_ref());
}

fn handle_early_args(args: &[String]) -> bool {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("zcall - native Zalo call signaling/media engine");
        println!("Usage: zcall [--recv PATH] [--send PATH] [--send-udp] [--group] [--verbose]");
        println!("       zcall --check");
        return true;
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("zcall {}", env!("CARGO_PKG_VERSION"));
        return true;
    }
    if args.iter().any(|arg| arg == "--check") {
        let main_js = zalo_main_js_path();
        match load_callv2_key(&main_js) {
            Ok(_) => {
                println!(
                    "zcall preflight OK; call-v2 transport key loaded from {}",
                    main_js.display()
                );
                return true;
            }
            Err(error) => {
                eprintln!("zcall preflight failed: {error}");
                std::process::exit(2);
            }
        }
    }
    false
}

fn parse_runtime_options(args: &[String]) -> RuntimeOptions {
    let mut recv_target = None;
    let mut send_target = None;
    let mut send_udp = env::var("ZCALL_SEND_UDP").is_ok_and(|value| value == "1");
    let mut is_group = env::var("ZCALL_GROUP").map_or(true, |value| value != "0");
    let mut verbose = env::var("ZCALL_VERBOSE").is_ok_and(|value| !value.is_empty());

    let default_dir = env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let default_recv = format!("{}/socketzalorecv2021", default_dir.trim_end_matches('/'));
    let default_send = format!("{}/socketzalosend2021", default_dir.trim_end_matches('/'));

    let mut index = 1;
    let mut positionals = Vec::new();
    while index < args.len() {
        let arg = &args[index];
        if arg == "--recv" && index + 1 < args.len() {
            recv_target = Some(args[index + 1].clone());
            index += 2;
        } else if arg == "--send" && index + 1 < args.len() {
            send_target = Some(args[index + 1].clone());
            index += 2;
        } else if arg == "--send-udp" {
            send_udp = true;
            index += 1;
        } else if arg == "--group" {
            is_group = true;
            index += 1;
        } else if arg == "--verbose" {
            verbose = true;
            index += 1;
        } else if !arg.starts_with("--") {
            positionals.push(arg.clone());
            index += 1;
        } else {
            index += 1;
        }
    }

    let recv_path = recv_target
        .or_else(|| positionals.first().cloned())
        .unwrap_or(default_recv);
    let send_path = send_target
        .or_else(|| positionals.get(1).cloned())
        .unwrap_or(default_send);

    RuntimeOptions {
        recv_path,
        send_path,
        send_udp,
        is_group,
        verbose,
    }
}

fn setup_log_file() -> Option<File> {
    let log_dir = env::var("ZCALL_LOG_DIR").ok()?;
    let filename = format!("native-engine-{}.log", chrono_timestamp_date());
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(PathBuf::from(log_dir).join(filename))
        .ok()
}

fn log_startup(log_file: Option<&File>, options: &RuntimeOptions) {
    log(
        log_file,
        &format!(
            "Starting native zcall Rust engine: send_udp={}, group={}, sockets: recv={}, send={}",
            options.send_udp, options.is_group, options.recv_path, options.send_path
        ),
    );
}

fn log_runtime_environment(log_file: Option<&File>) {
    log(
        log_file,
        &format!(
            "runtime env: session_type={:?}, wayland_display={}, display={}, ui_port_present={}, ui_token_present={}",
            env::var("XDG_SESSION_TYPE").ok(),
            env::var_os("WAYLAND_DISPLAY").is_some(),
            env::var_os("DISPLAY").is_some(),
            env::var_os("ZCALL_UI_PORT").is_some(),
            env::var_os("ZCALL_UI_TOKEN").is_some()
        ),
    );
}

fn load_transport_key(main_js: &Path, log_file: Option<&File>) -> [u8; 16] {
    match load_callv2_key(main_js) {
        Ok(key) => key,
        Err(error) => {
            log(
                log_file,
                &format!("Failed to load callV2TransportAesKey: {error}"),
            );
            log(log_file, &format!("FATAL: {error}"));
            std::process::exit(1);
        }
    }
}

fn connect_socket(path: &str, socket_name: &str, log_file: Option<&File>) -> UnixStream {
    match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            log(
                log_file,
                &format!("Failed to connect to {socket_name} socket {path}: {error}"),
            );
            log(log_file, &format!("FATAL: {error}"));
            std::process::exit(1);
        }
    }
}

fn register_shutdown_signals(log_file: Option<&File>) -> Arc<AtomicBool> {
    let shutdown = Arc::new(AtomicBool::new(false));
    for (signal, name) in [(SIGINT, "SIGINT"), (SIGTERM, "SIGTERM")] {
        if let Err(error) = signal_hook::flag::register(signal, Arc::clone(&shutdown)) {
            log(
                log_file,
                &format!("Failed to register {name} handler: {error}"),
            );
            std::process::exit(1);
        }
    }
    shutdown
}

fn run_engine(
    options: &RuntimeOptions,
    key: [u8; 16],
    recv_stream: UnixStream,
    send_stream: UnixStream,
    log_file: Option<&File>,
) {
    let _ = recv_stream.set_nonblocking(true);
    let _ = send_stream.set_nonblocking(true);

    let running = Arc::new(AtomicBool::new(true));
    let shutdown = register_shutdown_signals(log_file);
    let ui = TcpUiTransport::connect_from_env()
        .map(|transport| Box::new(transport) as Box<dyn zcall::ui::UiTransport>);
    let mut engine = EngineCore::new(options.send_udp, options.is_group, ui);
    engine.start();

    let mut parser = FrameParser::new(key, false);
    let mut send_stream = send_stream;
    let mut recv_stream = recv_stream;
    let mut read_buf = [0u8; 8192];

    while running.load(Ordering::Relaxed) && !shutdown.load(Ordering::Relaxed) {
        if !read_zalo_frames(
            &mut send_stream,
            &mut read_buf,
            &mut parser,
            &mut engine,
            options.verbose,
            log_file,
        ) {
            break;
        }

        engine.poll_ui_actions();
        write_outgoing_frames(
            &mut engine,
            &mut recv_stream,
            &key,
            options.verbose,
            log_file,
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    engine.hangup("engine_exit");
    log(log_file, "zcall engine shutdown complete.");
}

fn read_zalo_frames(
    stream: &mut UnixStream,
    read_buf: &mut [u8; 8192],
    parser: &mut FrameParser,
    engine: &mut EngineCore,
    verbose: bool,
    log_file: Option<&File>,
) -> bool {
    match stream.read(read_buf) {
        Ok(0) => {
            log(log_file, "send socket EOF from Zalo, shutting down.");
            false
        }
        Ok(bytes_read) => {
            for event in parser.push(&read_buf[..bytes_read]) {
                match event {
                    FrameEvent::Json(value) => {
                        if verbose {
                            log(log_file, &format!("<- Zalo JSON: {value}"));
                        }
                        engine.on_zalo_frame(&value);
                    }
                    FrameEvent::Raw(raw) => {
                        if verbose {
                            log(log_file, &format!("<- Zalo RAW: {raw}"));
                        }
                    }
                    FrameEvent::Error(error) => {
                        log(log_file, &format!("Frame parse error: {error}"));
                    }
                    FrameEvent::Token(_) => {}
                }
            }
            true
        }
        Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(error) => {
            log(
                log_file,
                &format!("Error reading from send socket: {error}"),
            );
            false
        }
    }
}

fn write_outgoing_frames(
    engine: &mut EngineCore,
    stream: &mut UnixStream,
    key: &[u8; 16],
    verbose: bool,
    log_file: Option<&File>,
) {
    for frame in engine.poll_outgoing_frames() {
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
                log_file,
                &format!("outgoing Zalo event: type={frame_type}, command={command}"),
            );
        }
        if verbose {
            log(log_file, &format!("-> Zalo JSON: {frame}"));
        }

        let bytes = format_outgoing_frames(&frame, key);
        if let Err(error) = stream.write_all(&bytes) {
            log(log_file, &format!("Error writing to recv socket: {error}"));
            break;
        }
        let _ = stream.flush();
    }
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
