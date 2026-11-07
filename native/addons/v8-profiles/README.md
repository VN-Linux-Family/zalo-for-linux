# v8-profiles (native, Linux)

Native CPU profiler for Zalo's `native/nativelibs/v8-profiles`, built from source for
Linux x64 against the Electron 22.3.x ABI (raw V8 `CpuProfiler`, via NAN — not N-API,
so it must be rebuilt per Electron version).

Exposes the exact `binding.cpu.{ startProfiling, stopProfiling, setSamplingInterval,
profiles }` shape and the head-based CpuProfile format Zalo's `index.js` wrapper
expects (`{typeId,uid,title,head,startTime,endTime,samples,timestamps}`; node
`{functionName,url,scriptId,lineNumber,callUID,bailoutReason,hitCount,children}`).

Vendored from **v8-profiler-next** (MIT — see `LICENSE-v8-profiler-next`), trimmed so
the legacy (type 0) node serialization matches Zalo's field set byte-for-byte
(dropped `columnNumber`/`id`/`lineTicks`; `callUID` uses `GetNodeId()` since
`GetCallUid()` was removed in V8 8).

## What it's for

Zalo uses the V8 CPU profiler for performance diagnostics. On Linux the shipped macOS
`profiler_electron1.8_mac.node` cannot load (wrong platform + a dead Electron-1.8 ABI),
so `index.js` returned nothing usable. This is the first Linux build that actually runs;
there is **no fallback stub** — if it fails to load, the module throws (by request).

## Dependencies

None beyond V8/Electron itself (via **NAN**). Because it uses the raw V8 `CpuProfiler`
API (not N-API), it is **tied to the exact Electron/V8 ABI** and must be rebuilt when
Electron changes — unlike the N-API modules here.

## Build

```bash
# from repo root:
node nativelibs/builder.js nativelibs/v8-profiles
# → nativelibs/v8-profiles/build/Release/... .node
```

C++17. `patch-v8-profiles.js` copies the built `.node` into
`app/native/nativelibs/v8-profiles/` as `profiler_electron_linux_x64.node` and splices
`index.js` to load it on Linux (before the mac path), with no defensive fallback.

## Use (in the app)

Application code calls Zalo's `v8-profiles` API unchanged; on Linux `index.js` loads our
`profiler_electron_linux_x64.node`. The returned `binding.cpu` object exposes
`startProfiling`/`stopProfiling`/`setSamplingInterval`/`profiles` in the head-based
format described above.

## Updating when the macOS build changes

This module has **no third-party library pins** — it links V8/Electron, so it won't
appear in the version-drift report. What matters is the **Electron/V8 ABI**:

- **Electron version bump** (root `package.json`) → **must rebuild** (raw-V8 ABI).
  SETUP does this automatically.
- If a future Electron/V8 removes or changes a `CpuProfiler`/`CpuProfileNode` API used
  in `src/`, the build will fail to compile — update the vendored source (mirror the
  upstream v8-profiler-next fix) and re-verify the `binding.cpu` node shape still matches
  Zalo's `index.js` wrapper.

## When / how to rebuild

- **Every `npm run setup`** — rebuilt from source automatically.
- **Electron/V8 bump** — **required**; rebuilt automatically on next SETUP.
- **Edited `src/*.cc`** — `node nativelibs/builder.js nativelibs/v8-profiles`.

## Test

Under Electron's Node ABI (no display needed):

```bash
ELECTRON_RUN_AS_NODE=1 node_modules/.bin/electron -e "require('./app/native/nativelibs/v8-profiles/index.js')"
```
