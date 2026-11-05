//! Glob + filesystem-metadata helpers shared by the crawler and GC modules.

use globset::{Glob, GlobSet, GlobSetBuilder};
use std::fs::Metadata;
use std::time::{SystemTime, UNIX_EPOCH};

/// Compile a list of glob patterns (e.g. `<root>/Cache/**`, `**/*.tmp`) into a
/// GlobSet. Patterns that fail to compile are skipped (the mac binary is likewise
/// tolerant — a bad pattern must not abort a whole cleanup). An empty input yields a
/// set that matches nothing.
pub fn build_globset(patterns: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        if let Ok(g) = Glob::new(p) {
            b.add(g);
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Whole seconds since the unix epoch, now.
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// File access time as whole seconds since the unix epoch (mac reads `st_atime`).
/// Falls back to mtime, then 0, when a platform/filesystem denies atime.
pub fn atime_secs(md: &Metadata) -> i64 {
    let t = md
        .accessed()
        .or_else(|_| md.modified())
        .unwrap_or(UNIX_EPOCH);
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
