//! OS and process information for Linux on x86_64 and arm64.
//!
//! The public fields follow `os-sys-info-technical.md`. Linux collection uses
//! `/proc`; no third-party crates are required.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSample {
    pub pid: u32,
    pub ppid: u32,
    pub creation100ns: u64,
    pub kernel_time100ns: u64,
    pub user_time100ns: u64,
    pub working_set: u64,
    pub private_bytes: u64,
    pub wall_unix_ms: u64,
    pub monotonic_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessError {
    pub pid: u32,
    pub op: String,
    pub code: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcessSnapshot {
    pub samples: Vec<ProcessSample>,
    pub errors: Vec<ProcessError>,
}

/// Returns the operating system architecture label for supported targets.
pub fn get_os_architecture() -> &'static str {
    "64-bit"
}

#[allow(non_snake_case)]
pub fn getOsArchitecture() -> &'static str { get_os_architecture() }

/// Returns the machine label (`x64` or `arm64`) reported by Linux.
pub fn get_os_machine() -> Option<String> {
    let machine = uname_machine().ok()?;
    match machine.as_str() {
        "x86_64" => Some("x64".to_owned()),
        "aarch64" => Some("arm64".to_owned()),
        _ => None,
    }
}

#[allow(non_snake_case)]
pub fn getOsMachine() -> Option<String> { get_os_machine() }

/// Collects one snapshot per unique, valid positive PID, preserving input order.
/// Values that are fractional, non-positive, or at least 2^31 are ignored.
pub fn get_process_snapshot(pids: &[f64]) -> ProcessSnapshot {
    let mut result = ProcessSnapshot::default();
    let mut seen = HashSet::new();

    for &value in pids {
        if !value.is_finite() || value.fract() != 0.0 || value <= 0.0 || value >= 2_147_483_648.0 {
            continue;
        }
        let pid = value as u32;
        if !seen.insert(pid) {
            continue;
        }
        match collect_process(pid) {
            Ok(sample) => result.samples.push(sample),
            Err(error) => result.errors.push(error),
        }
    }
    result
}

#[allow(non_snake_case)]
pub fn getProcessSnapshot(pids: &[f64]) -> ProcessSnapshot { get_process_snapshot(pids) }

fn collect_process(pid: u32) -> Result<ProcessSample, ProcessError> {
    let base = format!("/proc/{pid}");
    let stat = fs::read_to_string(format!("{base}/stat"))
        .map_err(|e| process_error(pid, "readProcessStat", e))?;
    let fields = parse_stat(&stat).ok_or_else(|| ProcessError {
        pid,
        op: "parseProcessStat".to_owned(),
        code: -1,
    })?;
    let ticks_per_second = unsafe { sysconf(2) };
    if ticks_per_second <= 0 {
        return Err(ProcessError { pid, op: "getClockTicks".to_owned(), code: io::Error::last_os_error().raw_os_error().unwrap_or(-1) });
    }
    let hz = ticks_per_second as u64;
    let kernel = ticks_to_100ns(fields.stime, hz);
    let user = ticks_to_100ns(fields.utime, hz);
    let boot_time = get_boot_time().map_err(|e| process_error(pid, "readBootTime", e))?;
    let creation = boot_time.saturating_mul(10_000_000).saturating_add(ticks_to_100ns(fields.starttime, hz));

    let status = fs::read_to_string(format!("{base}/status"))
        .map_err(|e| process_error(pid, "readProcessStatus", e))?;
    let rss_kib = status_value(&status, "VmRSS:").ok_or_else(|| ProcessError { pid, op: "readWorkingSet".to_owned(), code: -1 })?;
    let working_set = rss_kib.checked_mul(1024).ok_or_else(|| ProcessError { pid, op: "readWorkingSet".to_owned(), code: -1 })?;

    let private_bytes = if let Ok(smaps) = fs::read_to_string(format!("{base}/smaps_rollup")) {
        let clean = status_value(&smaps, "Private_Clean:").unwrap_or(0);
        let dirty = status_value(&smaps, "Private_Dirty:").unwrap_or(0);
        let hugetlb = status_value(&smaps, "Private_Hugetlb:").unwrap_or(0);
        clean.saturating_add(dirty).saturating_add(hugetlb).saturating_mul(1024)
    } else {
        status_value(&status, "RssAnon:")
            .map(|anon_kib| anon_kib.saturating_mul(1024))
            .unwrap_or(working_set)
    };
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    static MONOTONIC_START: LazyLock<Instant> = LazyLock::new(Instant::now);
    let monotonic_ms = MONOTONIC_START.elapsed().as_millis() as u64;

    Ok(ProcessSample {
        pid,
        ppid: fields.ppid,
        creation100ns: creation,
        kernel_time100ns: kernel,
        user_time100ns: user,
        working_set: working_set,
        private_bytes: private_bytes,
        wall_unix_ms: now.as_millis() as u64,
        monotonic_ms: monotonic_ms,
    })
}

struct StatFields { ppid: u32, utime: u64, stime: u64, starttime: u64 }

fn parse_stat(stat: &str) -> Option<StatFields> {
    // The command name in field 2 may contain spaces and parentheses.
    let close = stat.rfind(") ")?;
    let fields: Vec<&str> = stat[close + 2..].split_whitespace().collect();
    Some(StatFields {
        ppid: fields.get(1)?.parse().ok()?,
        utime: fields.get(11)?.parse().ok()?,
        stime: fields.get(12)?.parse().ok()?,
        starttime: fields.get(19)?.parse().ok()?,
    })
}

fn status_value(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?;
        rest.split_whitespace().next()?.parse().ok()
    })
}

fn ticks_to_100ns(ticks: u64, hz: u64) -> u64 {
    ((ticks as u128 * 10_000_000u128) / hz as u128).min(u64::MAX as u128) as u64
}

fn process_error(pid: u32, op: &str, error: io::Error) -> ProcessError {
    ProcessError { pid, op: op.to_owned(), code: error.raw_os_error().unwrap_or(-1) }
}
static BOOT_TIME: LazyLock<io::Result<u64>> = LazyLock::new(|| {
    let content = fs::read_to_string("/proc/stat")?;
    content
        .lines()
        .find_map(|line| line.strip_prefix("btime ")?.parse::<u64>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "btime not found in /proc/stat"))
});

fn get_boot_time() -> io::Result<u64> {
    match &*BOOT_TIME {
        Ok(t) => Ok(*t),
        Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
    }
}


#[repr(C)]
struct UtsName { sysname: [i8; 65], nodename: [i8; 65], release: [i8; 65], version: [i8; 65], machine: [i8; 65], domainname: [i8; 65] }

fn uname_machine() -> io::Result<String> {
    let mut name = std::mem::MaybeUninit::<UtsName>::zeroed();
    let rc = unsafe { uname(name.as_mut_ptr()) };
    if rc != 0 { return Err(io::Error::last_os_error()); }
    let name = unsafe { name.assume_init() };
    let bytes: Vec<u8> = name.machine.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(not(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]
compile_error!("os-sys-info supports only Linux x86_64 and Linux arm64");

unsafe extern "C" {
    fn uname(name: *mut UtsName) -> i32;
    fn sysconf(name: i32) -> isize;
}

// Node.js bindings. These wrappers keep the Rust API above available while
// exposing the names and object shapes used by the native Node packages.
#[napi_derive::napi(js_name = "getOsArchitecture")]
pub fn node_get_os_architecture() -> String {
    get_os_architecture().to_owned()
}

#[napi_derive::napi(js_name = "getOsMachine")]
pub fn node_get_os_machine() -> Option<String> {
    get_os_machine()
}

#[napi_derive::napi(object)]
pub struct NodeProcessSample {
    pub pid: u32,
    pub ppid: u32,
    #[napi(js_name = "creation100ns")]
    pub creation100ns: f64,
    #[napi(js_name = "kernelTime100ns")]
    pub kernel_time100ns: f64,
    #[napi(js_name = "userTime100ns")]
    pub user_time100ns: f64,
    #[napi(js_name = "workingSet")]
    pub working_set: f64,
    #[napi(js_name = "privateBytes")]
    pub private_bytes: f64,
    #[napi(js_name = "wallUnixMs")]
    pub wall_unix_ms: f64,
    #[napi(js_name = "monotonicMs")]
    pub monotonic_ms: f64,
}

#[napi_derive::napi(object)]
pub struct NodeProcessError {
    pub pid: u32,
    pub op: String,
    pub code: i32,
}

#[napi_derive::napi(object)]
pub struct NodeProcessSnapshot {
    pub samples: Vec<NodeProcessSample>,
    pub errors: Vec<NodeProcessError>,
}

pub struct NodeProcessSnapshotTask {
    pids: Vec<f64>,
}

impl napi::Task for NodeProcessSnapshotTask {
    type Output = ProcessSnapshot;
    type JsValue = NodeProcessSnapshot;

    fn compute(&mut self) -> napi::Result<Self::Output> {
        Ok(get_process_snapshot(&self.pids))
    }

    fn resolve(
        &mut self,
        _env: napi::Env,
        snapshot: Self::Output,
    ) -> napi::Result<Self::JsValue> {
        Ok(NodeProcessSnapshot {
            samples: snapshot.samples.into_iter().map(|sample| NodeProcessSample {
                pid: sample.pid,
                ppid: sample.ppid,
                creation100ns: sample.creation100ns as f64,
                kernel_time100ns: sample.kernel_time100ns as f64,
                user_time100ns: sample.user_time100ns as f64,
                working_set: sample.working_set as f64,
                private_bytes: sample.private_bytes as f64,
                wall_unix_ms: sample.wall_unix_ms as f64,
                monotonic_ms: sample.monotonic_ms as f64,
            }).collect(),
            errors: snapshot.errors.into_iter().map(|error| NodeProcessError {
                pid: error.pid,
                op: error.op,
                code: error.code,
            }).collect(),
        })
    }
}

#[napi_derive::napi(js_name = "getProcessSnapshot")]
pub fn node_get_process_snapshot(pids: Vec<f64>) -> napi::Result<napi::bindgen_prelude::AsyncTask<NodeProcessSnapshotTask>> {
    Ok(napi::bindgen_prelude::AsyncTask::new(NodeProcessSnapshotTask { pids }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_os_metadata() {
        assert_eq!(get_os_architecture(), "64-bit");
        let machine = get_os_machine();
        assert!(machine.is_some());
        let m = machine.unwrap();
        assert!(m == "x64" || m == "arm64");
    }

    #[test]
    fn test_parse_stat_format() {
        let sample = "128776 (Web Content (1)) S 4751 128776 4751 34816 128776 4194304 4046613 7210034 34104 2554 38072 3779 20710 4986 20 0 80 0 2594714 7845638144 114561 18446744073709551615";
        let parsed = parse_stat(sample).expect("parse_stat should succeed");
        assert_eq!(parsed.ppid, 4751);
        assert_eq!(parsed.utime, 38072);
        assert_eq!(parsed.stime, 3779);
        assert_eq!(parsed.starttime, 2594714);
    }

    #[test]
    fn test_get_process_snapshot_self() {
        let pid = std::process::id() as f64;
        let snapshot = get_process_snapshot(&[pid]);
        assert_eq!(snapshot.samples.len(), 1);
        assert_eq!(snapshot.errors.len(), 0);
        let sample = &snapshot.samples[0];
        assert_eq!(sample.pid, pid as u32);
        assert!(sample.working_set > 0);
        assert!(sample.creation100ns > 0);
    }

    #[test]
    fn test_get_process_snapshot_invalid_pids() {
        let snapshot = get_process_snapshot(&[-1.0, 0.0, 2.5, 999999999.0]);
        assert_eq!(snapshot.samples.len(), 0);
        assert_eq!(snapshot.errors.len(), 1);
        assert_eq!(snapshot.errors[0].pid, 999999999);
    }
}
