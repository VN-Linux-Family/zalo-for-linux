#![allow(unsafe_code, reason = "onnxruntime FFI")]

#[cfg(not(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
compile_error!("zocr-host supports Linux x86_64 and aarch64 only");
mod config;
mod engine;
mod image;
mod ipc;
mod platform;
mod runtime;
mod vault;
use anyhow::{bail, ensure, Context, Result};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc, Mutex},
};
fn main() {
    if let Err(error) = run() {
        eprintln!("[zocr-host] FATAL: {error:#}");
        std::process::exit(1);
    }
}
fn kernel_supported(release: &str) -> bool {
    let mut parts = release.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    major > 5 || (major == 5 && minor >= 11)
}

fn run() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        eprintln!("zocr-host [--socket-dir DIR]\nBinary IPC on stdin/stdout. Requires ZOCR_AUTH_TOKEN.\nPlace seed_key and text_digest_key (64 hex characters each), libonnxruntime.so and zocr/models.zmdl beside binary.");
        return Ok(());
    }
    let socket_dir = if args.is_empty() {
        std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
    } else if args.len() == 2 && args[0] == "--socket-dir" {
        PathBuf::from(&args[1])
    } else {
        bail!("usage: zocr-host [--socket-dir DIR]");
    };
    let stop = platform::lifecycle()?;
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    let release = rustix::system::uname()
        .release()
        .to_string_lossy()
        .into_owned();
    ensure!(
        kernel_supported(&release),
        "zocr-host requires Linux 5.11 or newer (running {release})"
    );
    // Restrict socket permissions from creation, including the bind/chmod interval.
    rustix::process::umask(rustix::fs::Mode::from_bits_truncate(0o077));
    let cpu = platform::Cpu::discover()?;
    let cfg = config::Config::from_env(cpu.physical)?;
    platform::Cpu::gate(cfg.skip_cpu_gate)?;
    cpu.apply_affinity(&cfg.affinity)?;
    let dir = std::env::current_exe()?
        .parent()
        .context("binary has no parent directory")?
        .to_path_buf();
    let engine = Arc::new(Mutex::new(engine::Engine::new(dir, cfg.clone(), cpu)?));
    let maintenance = platform::maintenance(engine.clone(), stop.clone());
    let result = ipc::run(&engine, &cfg, &stop, &socket_dir);
    stop.store(true, Ordering::Relaxed);
    let _ = maintenance.join();
    result
}

#[cfg(test)]
mod tests {
    use super::kernel_supported;

    #[test]
    fn kernel_gate_requires_linux_5_11() {
        assert!(!kernel_supported("5.10.99"));
        assert!(!kernel_supported("4.19"));
        assert!(kernel_supported("5.11"));
        assert!(kernel_supported("5.11.0"));
        assert!(kernel_supported("6.1.0"));
    }
}
