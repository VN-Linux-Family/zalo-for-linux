//! `scanDirectory(rootPath, trackingFolderPaths)` — walk the whole download directory,
//! build the process-global file tree (every file starts unmarked), and return the
//! totals plus a per-tracking-folder aggregate.

use crate::model::{FileInfo, FolderBasicInfo, ScanState, STATE};
use crate::util::{atime_secs, build_globset};
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

pub struct ScanOutcome {
    pub file_number: u32,
    pub size: u64,
    /// tracking-glob -> aggregate over the files that match it
    pub tracking: HashMap<String, FolderBasicInfo>,
}

/// Enumerate every regular file under `root`. All standard filters are OFF: this is a
/// media cache, not a source tree, so `.gitignore`/hidden/parent rules must not hide
/// files (they would make cleanup under-count and under-delete).
fn collect_files(root: &str) -> Vec<String> {
    let mut out = Vec::new();
    let walker = WalkBuilder::new(root)
        .standard_filters(false)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .follow_links(false)
        .build();
    for dent in walker.flatten() {
        if dent.file_type().map(|t| t.is_file()).unwrap_or(false) {
            out.push(dent.path().to_string_lossy().into_owned());
        }
    }
    out
}

/// Stat one path into a fresh (unmarked) FileInfo. Returns None if it vanished/denied.
fn stat_file(path: &str) -> Option<FileInfo> {
    let md = std::fs::metadata(path).ok()?;
    if !md.is_file() {
        return None;
    }
    let file_name = Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Some(FileInfo {
        file_name,
        file_path: path.to_string(),
        size: md.len(),
        atime: atime_secs(&md),
        reference_message_id: String::new(),
    })
}

/// Aggregate a file list into the per-tracking-folder map. Each tracking pattern gets
/// one `FolderBasicInfo` summing the files that match that pattern.
pub fn aggregate_tracking(
    files: &[FileInfo],
    tracking_globs: &[String],
) -> HashMap<String, FolderBasicInfo> {
    let mut map: HashMap<String, FolderBasicInfo> = HashMap::new();
    // One single-pattern GlobSet per tracking pattern keeps the pattern->files mapping
    // unambiguous (a file may match several patterns and is counted under each).
    let sets: Vec<_> = tracking_globs
        .iter()
        .map(|g| (g.clone(), build_globset(std::slice::from_ref(g))))
        .collect();
    for (pat, set) in &sets {
        let entry = map.entry(pat.clone()).or_default();
        for f in files {
            if set.is_match(&f.file_path) {
                entry.file_number += 1;
                entry.size += f.size;
            }
        }
    }
    map
}

pub fn scan_directory(root: &str, tracking_globs: &[String]) -> ScanOutcome {
    let paths = collect_files(root);
    let files: Vec<FileInfo> = paths.par_iter().filter_map(|p| stat_file(p)).collect();

    let file_number = files.len() as u32;
    let size: u64 = files.iter().map(|f| f.size).sum();
    let tracking = aggregate_tracking(&files, tracking_globs);

    // Commit the fresh tree to the process-global state, replacing any prior scan.
    let mut index = HashMap::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        index.insert(f.file_path.clone(), i);
    }
    *STATE.lock() = ScanState {
        root: root.to_string(),
        files,
        index,
    };

    ScanOutcome {
        file_number,
        size,
        tracking,
    }
}
