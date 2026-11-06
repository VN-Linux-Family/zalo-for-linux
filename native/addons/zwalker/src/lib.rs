//! Linux port of Zalo's macOS `zwalker` NAPI-RS addon — a storage garbage collector
//! for the media download directory. Reconstructed from the mac binary's leaked crate
//! layout (`src/crawler/*`, `src/garbage_collector/*`), struct field names, dependency
//! set, and the JS facade + orchestrator that drives it. See RE-PARAMS.md.
//!
//! Exposed to JS (napi auto-camelCases the fn + field names):
//!   scanDirectory, updateReferenceMessageId, statUnmarkedFiles,
//!   deleteHomelessFiles, deleteEmptyFolders
//!
//! Divergence from mac, documented for honesty: the mac build drives these through a
//! tokio runtime (async). We run them synchronously — the JS facade `await`s the
//! result either way, so the caller-visible contract (returned object shape) is
//! identical; only the threading differs. The feature is server-gated OFF by default
//! (`cleanup.enable`), so this addon stays dormant until Zalo enables it.

mod crawler;
mod garbage_collector;
mod model;
mod util;

use napi_derive::napi;

use crawler::scan_directory::scan_directory as scan_directory_impl;
use crawler::stat_unmarked_files::stat_unmarked_files as stat_unmarked_impl;
use crawler::update_file_info::{update_reference_message_id as update_ref_impl, ReferenceUpdate};
use garbage_collector::delete_empty_folders::delete_empty_folders as delete_empty_impl;
use garbage_collector::delete_homeless_files::delete_homeless_files as delete_homeless_impl;
use model::FolderBasicInfo;
use std::collections::HashMap;

fn tracking_json(map: &HashMap<String, FolderBasicInfo>) -> Option<String> {
    serde_json::to_string(map).ok()
}

fn tracking_atime_json(map: &HashMap<String, Vec<FolderBasicInfo>>) -> Option<String> {
    serde_json::to_string(map).ok()
}

// ---- result objects (napi camelCases these fields) -------------------------------

#[napi(object)]
pub struct ScanResult {
    pub file_number: u32,
    pub size: i64,
    pub tracking_path: Option<String>,
}

#[napi(object)]
pub struct UpdateResult {
    /// The renderer maps this onto `updateCount`.
    pub file_number: u32,
}

#[napi(object)]
pub struct StatResult {
    pub file_number: u32,
    pub size: i64,
    pub tracking_path: Option<String>,
    /// `tracking_a_time` -> JS `trackingATime` (napi camelCase: tracking + A + Time).
    pub tracking_a_time: Option<String>,
}

#[napi(object)]
pub struct DeleteResult {
    pub file_number: u32,
    pub size: i64,
    pub failed_file_number: u32,
    pub failed_size: i64,
    pub tracking_path: Option<String>,
}

#[napi(object)]
pub struct EmptyFolderResult {
    pub deleted_count: u32,
    pub deleted_dirs: Vec<String>,
}

// ---- exported functions ----------------------------------------------------------

#[napi]
pub fn scan_directory(root: String, tracking_folder_paths: Vec<String>) -> ScanResult {
    let o = scan_directory_impl(&root, &tracking_folder_paths);
    ScanResult {
        file_number: o.file_number,
        size: o.size as i64,
        tracking_path: tracking_json(&o.tracking),
    }
}

#[napi]
pub fn update_reference_message_id(_root: String, updates: Vec<ReferenceUpdate>) -> UpdateResult {
    UpdateResult {
        file_number: update_ref_impl(&updates),
    }
}

#[napi]
pub fn stat_unmarked_files(
    _root: String,
    ignore_folder_paths: Vec<String>,
    tracking_folder_paths: Vec<String>,
    age_thresholds: Vec<i64>,
) -> StatResult {
    let o = stat_unmarked_impl(&ignore_folder_paths, &tracking_folder_paths, &age_thresholds);
    StatResult {
        file_number: o.file_number,
        size: o.size as i64,
        tracking_path: tracking_json(&o.tracking),
        tracking_a_time: tracking_atime_json(&o.tracking_atime),
    }
}

#[napi]
pub fn delete_homeless_files(
    _root: String,
    ignore_folder_paths: Vec<String>,
    tracking_folder_paths: Vec<String>,
    delete_stat_cache: Option<bool>,
) -> DeleteResult {
    // The 4th arg is `deleteStatCache` (del_stat_cache), NOT a "should delete" switch:
    // this function is only invoked from the delete phase (gated by enable_del upstream),
    // so it always deletes the homeless files. The flag only concerns the stat cache,
    // which our in-RAM model does not persist -> no-op here.
    let o = delete_homeless_impl(
        &ignore_folder_paths,
        &tracking_folder_paths,
        delete_stat_cache.unwrap_or(true),
    );
    DeleteResult {
        file_number: o.file_number,
        size: o.size as i64,
        failed_file_number: o.failed_file_number,
        failed_size: o.failed_size as i64,
        tracking_path: tracking_json(&o.tracking),
    }
}

#[napi]
pub fn delete_empty_folders(root: String) -> EmptyFolderResult {
    let o = delete_empty_impl(&root);
    EmptyFolderResult {
        deleted_count: o.deleted_count,
        deleted_dirs: o.deleted_dirs,
    }
}
