//! Shared data model + the process-global scan tree.
//!
//! zwalker keeps a single tree of the last-scanned download directory ALIVE in
//! process memory (behind a Mutex) for the whole lifetime of the `shared-worker`
//! process. `scanDirectory` builds it; `updateReferenceMessageId` mutates the
//! per-file `reference_message_id` in place; `statUnmarkedFiles`/`deleteHomelessFiles`
//! read it back. This mirrors the mac binary, whose panic strings reference exactly
//! this design ("Mutex is poisoned, unable to retrieve inner value from tree",
//! "Error locking tree"). Nothing is persisted to disk — restart the process and the
//! marks are gone, which is why the app re-scans + re-marks every cleanup cycle.

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One file leaf. serde field names are snake_case to match the mac `struct FileInfo`
/// (leaked: `file_name file_path atime reference_message_id` + `size`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileInfo {
    pub file_name: String,
    pub file_path: String,
    pub size: u64,
    /// Access time as whole seconds since the unix epoch (mac uses `st_atime`).
    pub atime: i64,
    /// The message id that references this file. Empty string == "homeless"
    /// (no message keeps it alive) == a deletion candidate.
    pub reference_message_id: String,
}

impl FileInfo {
    #[inline]
    pub fn is_homeless(&self) -> bool {
        self.reference_message_id.is_empty()
    }
}

/// Recursive folder node — the mac `struct NodeData` (leaked:
/// `files folder_name folder_path folder_parent_path file_number size update_count sub_folders`).
/// Kept as the documented mac tree shape; our reconstruction computes the same JS-facing
/// aggregates from a flat file list, so this type is descriptive, not on the hot path.
#[allow(dead_code)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeData {
    pub files: Vec<FileInfo>,
    pub folder_name: String,
    pub folder_path: String,
    pub folder_parent_path: String,
    pub file_number: u32,
    pub size: u64,
    pub update_count: u32,
    pub sub_folders: Vec<NodeData>,
}

/// Per-tracking-folder aggregate — the mac `struct FolderBasicInfo` (2 fields).
/// This is the value type inside the `trackingPath` / `trackingATime` JSON maps that
/// the renderer reads as `{ size, file_number }`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FolderBasicInfo {
    pub file_number: u32,
    pub size: u64,
}

/// The process-global scan state. A flat file list + a path index reproduces every
/// JS-facing aggregate the mac tree produces, with O(1) marking by path.
#[derive(Default)]
pub struct ScanState {
    /// The root of the last scan. Retained for diagnostics/parity with the mac tree.
    #[allow(dead_code)]
    pub root: String,
    pub files: Vec<FileInfo>,
    /// absolute file_path -> index into `files`
    pub index: HashMap<String, usize>,
}

pub static STATE: Lazy<Mutex<ScanState>> = Lazy::new(|| Mutex::new(ScanState::default()));
