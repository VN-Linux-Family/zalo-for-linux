use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use napi::bindgen_prelude::*;
use napi_derive::napi;

static CANCEL_COPY: AtomicBool = AtomicBool::new(false);

#[napi(object)]
pub struct DiskEntry {
    pub name: String,
    pub label: String,
    pub path: String,
    pub is_external: bool,
    pub total_space: f64,
    pub used_space: f64,
}

#[napi(object)]
pub struct FileInfo {
    pub size: f64,
    pub is_directory: bool,
    pub name: String,
    pub last_write_time: f64,
    pub file_count: Option<f64>,
}

#[napi(js_name = "canRead")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "napi takes ownership of the argument"
)]
pub fn can_read(path: String) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.as_bytes().contains(&0) {
        return false;
    }
    rustix::fs::access(std::path::Path::new(&path), rustix::fs::Access::READ_OK).is_ok()
}

#[napi(js_name = "canWrite")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "napi takes ownership of the argument"
)]
pub fn can_write(path: String) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.as_bytes().contains(&0) {
        return false;
    }
    rustix::fs::access(std::path::Path::new(&path), rustix::fs::Access::WRITE_OK).is_ok()
}

#[napi(js_name = "canReadAndWrite")]
#[allow(
    clippy::needless_pass_by_value,
    reason = "napi takes ownership of the argument"
)]
pub fn can_read_and_write(path: String) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.as_bytes().contains(&0) {
        return false;
    }
    rustix::fs::access(
        std::path::Path::new(&path),
        rustix::fs::Access::READ_OK | rustix::fs::Access::WRITE_OK,
    )
    .is_ok()
}

fn is_real_fs(fstype: &str, dev: &str) -> bool {
    const PSEUDO: &[&str] = &[
        "proc",
        "sysfs",
        "tmpfs",
        "devtmpfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "overlay",
        "squashfs",
        "mqueue",
        "debugfs",
        "tracefs",
        "securityfs",
        "pstore",
        "bpf",
        "configfs",
        "hugetlbfs",
        "autofs",
        "fusectl",
        "binfmt_misc",
        "ramfs",
        "nsfs",
        "rpc_pipefs",
        "fuse.gvfsd-fuse",
        "fuse.portal",
        "efivarfs",
    ];
    const ALLOW: &[&str] = &[
        "ext2", "ext3", "ext4", "btrfs", "xfs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk",
        "f2fs", "zfs", "jfs", "reiserfs",
    ];
    if PSEUDO.contains(&fstype) {
        return false;
    }
    if dev.starts_with("/dev/") {
        return true;
    }
    ALLOW.contains(&fstype)
}

fn unescape_octal(s: &str) -> String {
    let mut res = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(&next) = chars.peek() {
                    if ('0'..='7').contains(&next) {
                        octal.push(chars.next().unwrap());
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(b) = u8::from_str_radix(&octal, 8) {
                    res.push(b as char);
                    continue;
                }
            }
            res.push('\\');
            res.push_str(&octal);
        } else {
            res.push(c);
        }
    }
    res
}

#[napi(js_name = "getDiskInfo")]
#[allow(
    clippy::unused_async,
    reason = "napi async signature is part of the JS API"
)]
#[allow(
    clippy::cast_precision_loss,
    reason = "disk sizes in bytes as f64; precision below 2^52 is irrelevant"
)]
pub async fn get_disk_info() -> Result<HashMap<String, DiskEntry>> {
    let mounts_content = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut seen = HashSet::new();
    let mut result = HashMap::new();

    for line in mounts_content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let dev = parts[0];
        let raw_dir = parts[1];
        let fstype = parts[2];

        let dir = unescape_octal(raw_dir);
        if seen.contains(&dir) || !is_real_fs(fstype, dev) || dir.as_bytes().contains(&0) {
            continue;
        }

        let Ok(vfs) = rustix::fs::statvfs(std::path::Path::new(&dir)) else {
            continue;
        };

        let frsize = if vfs.f_frsize != 0 {
            vfs.f_frsize
        } else {
            vfs.f_bsize
        };

        let total = vfs.f_blocks.saturating_mul(frsize);
        let used = if vfs.f_blocks >= vfs.f_bfree {
            (vfs.f_blocks - vfs.f_bfree) * frsize
        } else {
            0
        };

        if total == 0 {
            continue;
        }

        seen.insert(dir.clone());
        let is_external = dir.starts_with("/media/")
            || dir.starts_with("/run/media/")
            || dir.starts_with("/mnt/");

        result.insert(
            dir.clone(),
            DiskEntry {
                name: dir.clone(),
                label: dir.clone(),
                path: dir,
                is_external,
                total_space: total as f64,
                used_space: used as f64,
            },
        );
    }

    Ok(result)
}

fn walk_dir(path: &Path, total_size: &mut u64, file_count: &mut u64) {
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    walk_dir(&entry.path(), total_size, file_count);
                } else if file_type.is_file() {
                    if let Ok(meta) = entry.metadata() {
                        *total_size += meta.len();
                        *file_count += 1;
                    }
                }
            }
        }
    }
}

#[napi(js_name = "getInfo")]
#[allow(
    clippy::unused_async,
    reason = "napi async signature is part of the JS API"
)]
#[allow(
    clippy::cast_precision_loss,
    reason = "file sizes/time as f64; precision below 2^52 is irrelevant"
)]
pub async fn get_info(path: String, is_folder: Option<bool>) -> Result<FileInfo> {
    let p = Path::new(&path);
    let Ok(meta) = fs::metadata(p) else {
        return Err(Error::from_reason(format!(
            "ENOENT: no such file or directory: {path}"
        )));
    };

    let is_dir = meta.is_dir();
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&path)
        .to_string();

    let mtime_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0);

    let is_folder_flag = is_folder.unwrap_or(false);
    let (size, file_count) = if is_folder_flag {
        let mut total_size = 0u64;
        let mut count = 0u64;
        walk_dir(p, &mut total_size, &mut count);
        (total_size as f64, Some(count as f64))
    } else {
        (meta.len() as f64, None)
    };

    Ok(FileInfo {
        size,
        is_directory: is_dir,
        name,
        last_write_time: mtime_ms,
        file_count,
    })
}

fn copy_dir_rec(src: &Path, dst: &Path) -> Result<()> {
    if CANCEL_COPY.load(Ordering::SeqCst) {
        return Err(Error::from_reason("CANCELLED"));
    }
    if !dst.exists() {
        fs::create_dir_all(dst)
            .map_err(|e| Error::from_reason(format!("copyFolder failed: {e}")))?;
    }
    let entries =
        fs::read_dir(src).map_err(|e| Error::from_reason(format!("copyFolder failed: {e}")))?;

    for entry in entries.flatten() {
        if CANCEL_COPY.load(Ordering::SeqCst) {
            return Err(Error::from_reason("CANCELLED"));
        }
        let file_type = entry
            .file_type()
            .map_err(|e| Error::from_reason(format!("copyFolder failed: {e}")))?;
        let target_path = dst.join(entry.file_name());

        if file_type.is_dir() {
            copy_dir_rec(&entry.path(), &target_path)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &target_path)
                .map_err(|e| Error::from_reason(format!("copyFolder failed: {e}")))?;
        }
    }
    Ok(())
}

#[napi(js_name = "copyFolder")]
#[allow(
    clippy::unused_async,
    reason = "napi async signature is part of the JS API"
)]
pub async fn copy_folder(src: String, dst: String) -> Result<String> {
    CANCEL_COPY.store(false, Ordering::SeqCst);
    let src_path = Path::new(&src);
    let dst_path = Path::new(&dst);

    copy_dir_rec(src_path, dst_path)?;
    Ok(dst)
}

#[napi(js_name = "cancelCopy")]
pub fn cancel_copy() {
    CANCEL_COPY.store(true, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unescape_octal() {
        assert_eq!(unescape_octal(r"hello\040world"), "hello world");
        assert_eq!(unescape_octal("plain_path"), "plain_path");
    }

    #[test]
    fn test_is_real_fs() {
        assert!(is_real_fs("ext4", "/dev/sda1"));
        assert!(is_real_fs("btrfs", "/dev/nvme0n1p2"));
        assert!(!is_real_fs("proc", "proc"));
        assert!(!is_real_fs("sysfs", "sysfs"));
        assert!(!is_real_fs("tmpfs", "tmpfs"));
    }

    #[test]
    fn test_can_read_and_write() {
        assert!(can_read("/etc/passwd".to_string()));
        assert!(!can_write("/etc/passwd".to_string()));
        assert!(!can_read(String::new()));
    }
}
