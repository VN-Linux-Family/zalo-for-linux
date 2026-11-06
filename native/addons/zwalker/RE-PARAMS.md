# zwalker — reverse-engineering parameters

Everything below was recovered from the shipped macOS artifacts:
`app/native/nativelibs/zwalker/darwin-x64/{zwalker.darwin-x64.node, libzwalker.dylib}`
(strings/`nm`), the auto-generated `zwalker/index.js` (napi loader), the renderer facade
`$zFeatures.zwalker` in `app/main-dist/main.js`, and the orchestrator
`ResourceCleanupManager` in `app/pc-dist/shared-worker.*.js`.

## Crate identity

- NAPI-RS Rust addon. Leaked own-crate modules:
  `src/crawler/{scan_directory,stat_unmarked_files,update_file_info}.rs`,
  `src/garbage_collector/{delete_empty_folders,delete_homeless_files}.rs`.
- Author cargo path: `/Users/thanhvo/.cargo/…`.
- Dependency versions (from `…/index.crates.io-*/<crate>-<ver>/…` panic paths), pinned in
  `Cargo.toml` with `=`:
  `napi 2.16.17` (feature `napi8` → N-API 8, runs on Electron 39 = N-API 10),
  `ignore 0.4.25`, `globset 0.4.18`, `rayon 1.11.0`, `serde_json 1.0.149`,
  `once_cell 1.21.3`, `parking_lot 0.12.5`, `same-file 1.0.6`, plus transitive
  `aho-corasick`, `regex-automata/-syntax`, `memchr`, `crossbeam-*`, `smallvec`,
  `itoa`, `tokio 1.49` and `zmij 1.0.12`.
  - `zmij` = dtolnay's Schubfach double→string crate (a `ryu` sibling); a **transitive**
    float-formatting dep (via serde_json), irrelevant to GC behaviour. Not a Zalo module.
  - `tokio` present because the mac build drives the exports through an async runtime.
    We run synchronously — see "Divergences".

## Exported functions (napi auto-camelCases snake_case)

| JS name | Rust fn | Args | Returns |
|---|---|---|---|
| `scanDirectory` | `scan_directory` | `(root, trackingFolderPaths[])` | `{fileNumber, size, trackingPath}` |
| `updateReferenceMessageId` | `update_reference_message_id` | `(root, updates[])` | `{fileNumber}` |
| `statUnmarkedFiles` | `stat_unmarked_files` | `(root, ignore[], tracking[], ageThresholds[])` | `{fileNumber, size, trackingPath, trackingATime}` |
| `deleteHomelessFiles` | `delete_homeless_files` | `(root, ignore[], tracking[], delete?)` | `{fileNumber, size, failedFileNumber, failedSize, trackingPath}` |
| `deleteEmptyFolders` | `delete_empty_folders` | `(root)` | `{deletedCount, deletedDirs[]}` |

Argument order confirmed against the orchestrator's real calls, e.g.
`deleteUnmarkedFiles(root, getIgnoreFolderPaths(root), getTrackingFolderPaths(root), e)`
and `statUnmarkedFiles(root, getIgnoreFolderPaths(root), getTrackingFolderPaths(root), [259200,604800,1209600])`.
The renderer's `getTrackingFolderPaths` returns globs `[<root>/Cache/**, <root>/zcloud/**, **<suffix>]`.

## Structs (leaked serde field lists)

- `struct FileInfo` (5): `file_name, file_path, size, atime, reference_message_id`.
  `reference_message_id == ""` ⇒ homeless (deletion candidate).
- `struct NodeData` (8): `files, folder_name, folder_path, folder_parent_path,
  file_number, size, update_count, sub_folders`. The mac in-RAM tree; kept as
  documentation (`src/model.rs`), not on our hot path.
- `struct FolderBasicInfo` (2): `file_number, size` — the value type inside the
  `trackingPath`/`trackingATime` JSON maps. Confirmed by the orchestrator reading
  `Object.entries(trackingFolderData).map(([glob,{size,file_number}]) => …)` — **snake_case**
  keys, so `trackingPath` serializes with serde's default field names.

## Storage model

Process-global `static STATE: Lazy<Mutex<ScanState>>` (once_cell + parking_lot, exactly
the mac deps). `scanDirectory` replaces it; `updateReferenceMessageId` mutates ref ids in
place; `statUnmarkedFiles`/`deleteHomelessFiles` read it. **No on-disk persistence** — the
marks are lost on process restart (mac panic strings only ever mention an in-memory
"tree"; there is no sqlite dep and no xattr syscall import). The stray `.zwalker` /
`output.json` strings are an unused internal debug-dump path, not the marking store.

## Field-name mapping (napi camelCase)

`file_number→fileNumber`, `failed_file_number→failedFileNumber`, `failed_size→failedSize`,
`tracking_path→trackingPath`, `deleted_count→deletedCount`, `deleted_dirs→deletedDirs`,
and crucially **`tracking_a_time→trackingATime`** (napi splits on `_`: tracking + A + Time).
Sizes are `i64` (JS reads them through `Number(...)`); counts are `u32`.

## Behaviour decisions

- **Walk:** `ignore::WalkBuilder` with *all* standard filters OFF (`.standard_filters(false)`,
  hidden/ignore/git/parents off). This is a media cache, not a repo — gitignore/hidden
  rules must not hide files (that would under-count and under-delete).
- **atime:** `Metadata::accessed()` (mac `st_atime`), fallback mtime → 0.
- **age buckets** (`statUnmarkedFiles`): thresholds sorted ascending; a file of age
  `now-atime` falls into `[ <t0, [t0,t1), [t1,t2), … , >=t_last ]` ⇒ `thresholds.len()+1`
  buckets. `trackingATime` = `{ [glob]: FolderBasicInfo[] }`. (Reporting only; never drives
  deletion. Exact JSON shape is low-risk — the feature is server-gated OFF.)
- **homeless filter:** `reference_message_id.is_empty()` AND not matched by any `ignore` glob.
- **4th arg of `deleteHomelessFiles` = `deleteStatCache`, NOT a delete switch.** Traced
  precisely from the orchestrator: `executeDeletionPhase(e,t)` calls
  `this.deleteResources.bind(this, !!s.deleteStatCache)`, and `deleteResources(e)` forwards
  `e` as the 4th native arg — so it is `config.deleteStatCache` (`del_stat_cache`, default
  `1`). The decision to delete lives ONE level up: `run()` does
  `stage<=DELETE_RESOURCES && r.enableDelete ? executeDeletionPhase : calculateStatsAfterScanning`,
  so `deleteHomelessFiles` is only ever called when the app already wants to delete (gated
  by `enable_del`), and the read-only path is the separate `statUnmarkedFiles`. Therefore
  **`deleteHomelessFiles` ALWAYS deletes the homeless files when called**; the 4th boolean
  only concerns clearing the persisted *stat cache* (a no-op in our RAM-only model). An
  earlier reconstruction misread this arg as "should delete" and gated the unlink on it —
  which only worked because `del_stat_cache` defaults to 1; it would have silently skipped
  the whole cleanup under a server config of `enable_del:1 + del_stat_cache:0`. Fixed:
  deletion no longer depends on the flag.

## Divergences from mac (documented for honesty)

- **Sync vs async:** mac drives these through a tokio runtime; we run synchronously. The JS
  facade `await`s the result either way, so the caller-visible return shape is identical —
  only the threading differs (the `shared-worker` may block briefly during a GC pass).
- **Flat list vs literal tree:** we compute the same aggregates from a flat `Vec<FileInfo>`
  + path index instead of a recursive `NodeData`. Outputs are identical; internals differ.
- **No byte-identical oracle:** unlike `file-utils`/`file-utilities` (numeric outputs vs
  `du`/`stat`) or `mp4thumb` (JPEG bytes), a stateful file-deleting GC has no external
  oracle. Fidelity is anchored on the RE'd contract + a full-lifecycle fixture test.
