# zwalker (Linux) — media-cache garbage collector

Linux reconstruction of Zalo's `zwalker` NAPI-RS Rust addon: the storage garbage
collector that scans the media download directory, tracks which cached files are still
referenced by a message, and deletes the "homeless" ones (plus empty folders). The
mac/Windows build ships no Linux binary; the app's own auto-generated napi loader would
otherwise fall through to a `throw`, so Linux ran with an empty stub and **cache was
never reclaimed**. This addon restores the real behaviour.

## API (full — 5 functions, JS camelCase)

```ts
// Build the process-global file tree of `root`; every file starts unmarked.
scanDirectory(root: string, trackingFolderPaths: string[])
  : { fileNumber: number, size: number, trackingPath: string /*JSON*/ }

// Mark files referenced by a message (keeps them alive). Returns #files marked.
updateReferenceMessageId(root: string, updates: { filePath: string, id: string }[])
  : { fileNumber: number /* = updateCount */ }

// Report (no delete) the unmarked/homeless files, bucketed by access-time age.
statUnmarkedFiles(root, ignoreFolderPaths: string[], trackingFolderPaths: string[],
                  ageThresholds: number[])
  : { fileNumber, size, trackingPath: string, trackingATime: string /*JSON*/ }

// Delete homeless files from disk. Only ever called from the RRC delete phase (gated
// upstream by enable_del), so it ALWAYS deletes when invoked; the 4th arg is
// deleteStatCache (del_stat_cache), a separate stat-cache toggle, not a delete switch.
deleteHomelessFiles(root, ignoreFolderPaths, trackingFolderPaths, deleteStatCache?: boolean)
  : { fileNumber, size, failedFileNumber, failedSize, trackingPath: string }

// Remove empty directories under `root`, deepest-first. `root` itself is never removed.
deleteEmptyFolders(root)
  : { deletedCount: number, deletedDirs: string[] }
```

The renderer's `$zFeatures.zwalker` facade wraps these as `collectDirectoryStats`,
`updateFileStats`, `statUnmarkedFiles`, `deleteUnmarkedFiles`, `deleteEmptyFolders` and
JSON-parses `trackingPath` → `trackingFolderData` (`{ [glob]: { file_number, size } }`).

## Model (reverse-engineered)

zwalker keeps **one tree of the last-scanned directory alive in process memory**, behind
a `Mutex`, for the whole lifetime of the `shared-worker` process (the mac panic strings
name exactly this: *"Mutex is poisoned, unable to retrieve inner value from tree"*,
*"Error locking tree"*). Nothing is written to disk — the marks set by
`updateReferenceMessageId` live only until the process exits, which is why the app
re-scans + re-marks on every cleanup cycle. A file is a deletion candidate ("homeless")
when no `updateReferenceMessageId` call gave it a `reference_message_id`, and it is not
matched by any `ignoreFolderPaths` glob.

Our reconstruction computes the identical JS-facing aggregates from a flat file list +
path index (O(1) marking) rather than a literal recursive `NodeData` tree; the mac
`NodeData`/`FileInfo`/`FolderBasicInfo` shapes are kept in `src/model.rs` as
documentation. See `RE-PARAMS.md` for every field, argument order, and reconstruction
decision.

## Build / deploy

`cargo build --release` → `target/release/libzwalker.so`, deployed by
`scripts/patches/patch-zwalker.js` as `zwalker.linux-x64-gnu.node` (the slot the addon's
own napi loader already probes — no `index.js` splice needed). Dependencies are pinned to
the exact versions leaked by the mac binary. `ldd` shows only libc/libgcc.

## Safety

`deleteHomelessFiles`/`deleteEmptyFolders` delete for real (matching mac). The feature is
server-gated by `zalo_cloud…cleanup.enable`, which defaults **OFF** — this addon is loaded
and ready but the app does not run the GC until Zalo enables the flag, so nothing is
deleted unexpectedly. The full-lifecycle test (`__tests__/zwalker.test.js`) exercises real
deletion only on its own throwaway fixture.
