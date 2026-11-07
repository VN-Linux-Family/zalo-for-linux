# zfile (native, Linux)

Native addon for Zalo's `native/nativelibs/zfile`, reimplemented from source for Linux
x64. N-API (`node-addon-api`), ABI-stable across Electron 22.x.

## What it is / what it's for

`zfile` gives Zalo filesystem/volume information and a few file helpers — used for the
storage-management UI (free space per disk), permission checks, and folder copy with
progress/cancel. The Linux build implements the same API surface using standard POSIX:

- `getDiskInfo()` — enumerate mounted volumes with free/total space, keyed by mount
  point. Reads `/proc/mounts` + `statvfs()`, filtered to real, user-relevant mounts.
- `getInfo(path)` — stat info for a path.
- `canRead` / `canWrite` / `canReadAndWrite` — permission probes (`access()`).
- `copyFolder` / `cancelCopy` — recursive copy with cancellation.

Reverse-engineered from the macOS/Windows binary. This module is **not** byte-identical
by nature (it returns live system state); correctness = matching the JS **API shape**
the renderer expects.

## The JS wrapper — why it exists

Zalo's renderer looks up disk info **by an arbitrary absolute path**
(`diskInfo()[someFile]`) — on Windows it normalizes to a drive letter, which no-ops on
Linux and would return `undefined` (→ a `undefined.label` crash). So the Linux build
ships `zfile-linux.js`: it calls the native `getDiskInfo()` (mount-point-keyed) and
wraps the result in a `Proxy` whose `get` trap resolves any absolute POSIX path to the
**longest-prefix** matching mount point. This means it keeps working even if you move
Zalo's data dir to another mount (e.g. `/mnt/data`).

The patch requires this wrapper `.js` (not the `.node`) so the path-resolution logic is
in JS where it belongs.

## Exported native module

`build/Release/zfile-native.node`. `patch-zfile.js` copies it into
`app/native/nativelibs/zfile/linux/zfile-native.node`, copies `zfile-linux.js`
alongside, and splices `index.js` to `require('./linux/zfile-linux.js')` on Linux.

## Build

```bash
# from repo root:
node nativelibs/builder.js nativelibs/zfile
# → nativelibs/zfile/build/Release/zfile-native.node
```

`binding.gyp` target `zfile-native`, C++17, links only glibc (`statvfs`,
`getmntent_r` — no third-party libs).

## Use (in the app)

Application code calls Zalo's `zfile` API unchanged; on Linux the calls land in
`zfile-linux.js` → native `zfile-native.node`. `getDiskInfo()` returns the
path-resolving Proxy described above.

## Updating when the macOS build changes

`zfile` links **no third-party libraries** (glibc only), so it never appears in the
version-drift report and has no pins to update:

```bash
node nativelibs/scripts/check-native-versions.js   # zfile won't show up — expected
```

The only thing that would require work is Zalo changing the `zfile` **API** (new/renamed
methods or a different `getDiskInfo` shape). If the storage UI breaks after an update,
diff the macOS `zfile` binary's exports against `src/zfile.cc` and the wrapper.

## When / how to rebuild

- **Every `npm run setup`** — rebuilt from source automatically.
- **Electron bump** — rebuilt automatically (N-API stays ABI-stable).
- **Edited `src/zfile.cc` or `zfile-linux.js`** — `node nativelibs/builder.js nativelibs/zfile` (re-run SETUP to re-splice the wrapper).
- **glibc major upgrade** — rebuild from source; no code change expected.
