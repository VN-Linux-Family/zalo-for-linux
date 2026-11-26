//! Shared per-call Rust logger. Main, engine, transport and capture workers
//! all write to the same append-only native-engine log.
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

static LOG_FILE: OnceLock<Mutex<Option<File>>> = OnceLock::new();

pub fn init(file: Option<File>) {
    let slot = LOG_FILE.get_or_init(|| Mutex::new(None));
    if let Ok(mut current) = slot.lock() {
        *current = file;
    }
}

pub fn log(message: fmt::Arguments<'_>) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let line = format!(
        "{}.{:03} [zcall-rust] {}\n",
        now.as_secs(),
        now.subsec_millis(),
        message
    );
    if let Some(slot) = LOG_FILE.get() {
        if let Ok(mut file) = slot.lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
                return;
            }
        }
    }
    eprint!("{line}");
}
