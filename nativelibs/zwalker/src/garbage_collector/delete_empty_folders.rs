//! `deleteEmptyFolders(rootPath)` — remove every empty directory under `root`,
//! deepest-first so that a directory which becomes empty only after its (empty)
//! children are removed is itself removed in the same pass. `root` itself is never
//! removed. Returns the count and the list of removed directories.

use ignore::WalkBuilder;
use std::path::Path;

pub struct EmptyFolderOutcome {
    pub deleted_count: u32,
    pub deleted_dirs: Vec<String>,
}

pub fn delete_empty_folders(root: &str) -> EmptyFolderOutcome {
    // Enumerate all directories (filters off — same rationale as the file walk).
    let mut dirs: Vec<String> = Vec::new();
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
        if dent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            let p = dent.path();
            // Never the root itself.
            if p != Path::new(root) {
                dirs.push(p.to_string_lossy().into_owned());
            }
        }
    }

    // Deepest first: more path separators == deeper. Removing a child before its parent
    // lets the parent be seen as empty on this same pass.
    dirs.sort_by(|a, b| {
        b.matches(std::path::MAIN_SEPARATOR).count().cmp(&a.matches(std::path::MAIN_SEPARATOR).count())
    });

    let mut deleted_dirs = Vec::new();
    for d in dirs {
        // remove_dir only succeeds on an empty directory — exactly the semantics we want.
        if std::fs::remove_dir(&d).is_ok() {
            deleted_dirs.push(d);
        }
    }

    EmptyFolderOutcome {
        deleted_count: deleted_dirs.len() as u32,
        deleted_dirs,
    }
}
