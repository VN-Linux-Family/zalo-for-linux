//! `statUnmarkedFiles(rootPath, ignoreFolderPaths, trackingFolderPaths, ageThresholds)`
//! — report (WITHOUT deleting) the "unmarked" files: those left homeless after the
//! reference-marking pass, excluding anything under an ignore pattern. Results are
//! aggregated per tracking folder and, additionally, bucketed by access-time age so
//! the renderer can show "how much could be freed, split by staleness".

use crate::crawler::scan_directory::aggregate_tracking;
use crate::model::{FileInfo, FolderBasicInfo, STATE};
use crate::util::{build_globset, now_secs};
use std::collections::HashMap;

pub struct StatOutcome {
    pub file_number: u32,
    pub size: u64,
    pub tracking: HashMap<String, FolderBasicInfo>,
    /// tracking-glob -> per-age-bucket aggregate. The bucket vector has
    /// `thresholds.len() + 1` entries: [ < t0, [t0,t1), [t1,t2), … , >= t_last ].
    pub tracking_atime: HashMap<String, Vec<FolderBasicInfo>>,
}

/// Index of the age bucket a file falls into for ascending `thresholds`.
fn bucket_for(age: i64, thresholds: &[i64]) -> usize {
    for (i, &t) in thresholds.iter().enumerate() {
        if age < t {
            return i;
        }
    }
    thresholds.len()
}

pub fn stat_unmarked_files(
    ignore_globs: &[String],
    tracking_globs: &[String],
    age_thresholds: &[i64],
) -> StatOutcome {
    let state = STATE.lock();
    let ignore = build_globset(ignore_globs);

    // Homeless AND not protected by an ignore pattern.
    let unmarked: Vec<FileInfo> = state
        .files
        .iter()
        .filter(|f| f.is_homeless() && !ignore.is_match(&f.file_path))
        .cloned()
        .collect();

    let file_number = unmarked.len() as u32;
    let size: u64 = unmarked.iter().map(|f| f.size).sum();
    let tracking = aggregate_tracking(&unmarked, tracking_globs);

    // Ascending thresholds so the bucket boundaries are well defined regardless of the
    // order JS passed them in.
    let mut thresholds = age_thresholds.to_vec();
    thresholds.sort_unstable();
    let now = now_secs();
    let n_buckets = thresholds.len() + 1;

    let sets: Vec<_> = tracking_globs
        .iter()
        .map(|g| (g.clone(), build_globset(std::slice::from_ref(g))))
        .collect();
    let mut tracking_atime: HashMap<String, Vec<FolderBasicInfo>> = HashMap::new();
    for (pat, set) in &sets {
        let buckets = tracking_atime
            .entry(pat.clone())
            .or_insert_with(|| vec![FolderBasicInfo::default(); n_buckets]);
        for f in &unmarked {
            if set.is_match(&f.file_path) {
                let age = (now - f.atime).max(0);
                let b = &mut buckets[bucket_for(age, &thresholds)];
                b.file_number += 1;
                b.size += f.size;
            }
        }
    }

    StatOutcome {
        file_number,
        size,
        tracking,
        tracking_atime,
    }
}
