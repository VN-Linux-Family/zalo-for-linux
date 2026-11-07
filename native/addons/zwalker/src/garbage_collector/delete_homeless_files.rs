//! `deleteHomelessFiles(rootPath, ignoreFolderPaths, trackingFolderPaths, deleteStatCache)`
//! — remove the homeless (unmarked) files from disk, skipping anything under an ignore
//! pattern. Successfully removed files are dropped from the global tree so a following
//! stat/delete pass sees a consistent world.
//!
//! This function is only ever invoked from the RRC *delete* phase (`executeDeletionPhase`),
//! which the orchestrator runs only when `config.enableDelete` (`enable_del`) is true —
//! the read-only alternative is `statUnmarkedFiles`. So deleting the homeless files is the
//! function's whole purpose: it ALWAYS deletes when called. The 4th boolean is NOT a
//! "should I delete" switch — it is `deleteStatCache` (`del_stat_cache`, default 1), a
//! separate toggle for whether the persisted stat cache is also cleared. Our model keeps
//! no on-disk stat cache (the tree is process-global RAM, replaced each scan), so it is a
//! no-op here — but it must NOT gate the homeless deletion (that was an earlier RE
//! misread). See RE-PARAMS.md.

use crate::crawler::scan_directory::aggregate_tracking;
use crate::model::{FileInfo, FolderBasicInfo, STATE};
use crate::util::build_globset;
use std::collections::{HashMap, HashSet};

pub struct DeleteOutcome {
    pub file_number: u32,
    pub size: u64,
    pub failed_file_number: u32,
    pub failed_size: u64,
    pub tracking: HashMap<String, FolderBasicInfo>,
}

pub fn delete_homeless_files(
    ignore_globs: &[String],
    tracking_globs: &[String],
    _delete_stat_cache: bool,
) -> DeleteOutcome {
    let mut state = STATE.lock();
    let ignore = build_globset(ignore_globs);

    let candidates: Vec<FileInfo> = state
        .files
        .iter()
        .filter(|f| f.is_homeless() && !ignore.is_match(&f.file_path))
        .cloned()
        .collect();

    let mut deleted: Vec<FileInfo> = Vec::new();
    let mut removed_paths: HashSet<String> = HashSet::new();
    let mut failed_file_number = 0u32;
    let mut failed_size = 0u64;

    for f in candidates {
        {
            match std::fs::remove_file(&f.file_path) {
                Ok(()) => {
                    removed_paths.insert(f.file_path.clone());
                    deleted.push(f);
                }
                Err(_) => {
                    failed_file_number += 1;
                    failed_size += f.size;
                }
            }
        }
    }

    let file_number = deleted.len() as u32;
    let size: u64 = deleted.iter().map(|f| f.size).sum();
    let tracking = aggregate_tracking(&deleted, tracking_globs);

    // Keep the global tree consistent with the filesystem: drop what we unlinked.
    if !removed_paths.is_empty() {
        let st = &mut *state;
        st.files.retain(|f| !removed_paths.contains(&f.file_path));
        st.index.clear();
        for (i, f) in st.files.iter().enumerate() {
            st.index.insert(f.file_path.clone(), i);
        }
    }

    DeleteOutcome {
        file_number,
        size,
        failed_file_number,
        failed_size,
        tracking,
    }
}
