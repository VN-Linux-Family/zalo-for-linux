/**
 * plugins/zcall-bridge/index.js
 *
 * zcall call-v2 Wine environment manager.
 *
 * Zalo 26.x runs calls through a Qt helper (ZaloCall.exe on Windows). On
 * Linux, the patched main-dist code (scripts/patches/patch-zcall-callv2.js)
 * spawns the Windows ZaloCall.exe under Wine, bridged over named pipes ->
 * TCP by pipebridge.exe (see app/native/qt-call-and-cap/).
 *
 * This plugin prepares the environment before any call can happen:
 *   1. Locates a wine binary, best first: env ZCALL_WINE -> user-picked
 *      custom wine (config) -> downloaded runtime (userData) -> `wine` in
 *      PATH -> Bottles kron4ek runner. Every candidate is validated by
 *      actually running a 32-bit exe (pipebridge --version).
 *   2. If no usable wine exists on first run, ASKS the user (always-on-top
 *      window with browse/download options) and downloads a portable wine
 *      (classic 11.17, ~96MB) into <userData>/zcall-wine-runtime/ AND a
 *      64-bit GStreamer tree (release asset, ~120MB) into
 *      <userData>/zcall-gst-runtime/ with a progress window — no root
 *      needed, no host 32-bit libs, works on any distro.
 *   3. Ensures the wine prefix exists (wineboot), exports
 *      ZCALL_WINE / ZCALL_WINEPREFIX / WINEDEBUG into process.env.
 *   4. On quit, kills the whole wine session of our prefix; on launch,
 *      sweeps stale wine processes of unclean previous exits.
 *   5. Tray menu "Cài đặt gọi điện…" opens a settings window: browse/clear/
 *      remove wine (+ downloaded GStreamer).
 *
 * Configuration (env vars):
 *   ZCALL_WINE                 wine binary (highest priority)
 *   ZCALL_WINEPREFIX           wine prefix (default: <userData>/zcall-wine)
 *   ZCALL_DISABLE              set to anything to skip entirely
 *   ZCALL_AUTO_SETUP           '1' to download wine silently (no dialog)
 *   ZCALL_WINE_DOWNLOAD_URL    override the portable wine download URL
 *   ZCALL_GST_DOWNLOAD_URL     override the GStreamer release-asset URL
 *   ZCALL_GST_RUNTIME          override the bundled 64-bit GStreamer tree
 *                              (Full variants; consumed by the patched
 *                              ZaloCall spawn — never set globally)
 *   ZCALL_GST_REGISTRY         private gst registry file (default:
 *                              <userData>/gst-registry-64.bin)
 *   ZCALL_GST_REGISTRY_BRIDGE  separate registry for the bridge's bundled
 *                              gst-launch/gst-inspect (default:
 *                              <userData>/gst-registry-bridge-64.bin)
 */

'use strict';

const { spawn, spawnSync, execSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const https = require('https');

// The ONLY wine the app downloads: 11.17 classic amd64 (~96MB / ~652MB
// extracted after the dev-kit prune — headers, import libs, winegcc/widl
// toolchain removed). Video calls verified working on it (Mint 2026-09-10).
// The wow64 build crashes ZaloCall's DirectShow camera path on some hosts
// (wine qcap WoW64 media-type marshaling bug, upstream MR10269/10377
// unmerged as of 11.17 — minimal repro in zcall-bridge/camtest.c), so it is
// NOT offered as a download. Classic has no WoW64 boundary and is immune —
// it only needs the host's 32-bit libraries, which the app guides the user
// through when validation fails (getI386InstallHint).
// NOTE: this is the SAME classic build the Full variants bundle (see
// WINE_DOWNLOAD_URL_CLASSIC in scripts/build.js). Keep the two in sync.
const WINE_DOWNLOAD_URL =
  'https://github.com/Kron4ek/Wine-Builds/releases/download/11.17/wine-11.17-amd64.tar.xz';
const RUNTIME_DIRNAME = 'zcall-wine-runtime';
const CONFIG_FILENAME = 'zcall-config.json';
const GST_DIRNAME = 'zcall-gst-runtime';
const GST_TARBALL_NAME = 'zcall-gst-download.tar.xz';
const GST_RUNTIME_MARKER = path.join('usr', 'lib', 'x86_64-linux-gnu', 'libgstreamer-1.0.so.0');
// Ask-window/progress text; measured from a real CI build (dist asset).
// 64-bit gst tree for the Wayland screen-share bridge only (estimate after
// the libav/blas/lapack/plugins-good and python-closure drops — the old
// 105MB served the retired wow64 call path too).
const GST_DOWNLOAD_MB = 18;

let dialogModule = null;
let BrowserWindowModule = null;
let NotificationModule = null;

function getElectronModules() {
  try {
    const electron = require('electron');
    dialogModule = electron.dialog;
    BrowserWindowModule = electron.BrowserWindow;
    NotificationModule = electron.Notification;
  } catch (e) { /* not running inside Electron (unit tests) */ }
}

// ---------------------------------------------------------------------------
// Config (per-user choices, stored in userData)
// ---------------------------------------------------------------------------

function readConfig(userDataDir) {
  try {
    return JSON.parse(fs.readFileSync(path.join(userDataDir, CONFIG_FILENAME), 'utf8'));
  } catch (e) {
    return {};
  }
}

function writeConfig(userDataDir, cfg) {
  try {
    fs.mkdirSync(userDataDir, { recursive: true });
    fs.writeFileSync(path.join(userDataDir, CONFIG_FILENAME), JSON.stringify(cfg, null, 2));
  } catch (e) {
    console.error('[zcall-bridge] could not write config:', e.message);
  }
}

// ---------------------------------------------------------------------------
// Wine discovery
// ---------------------------------------------------------------------------

function findWine() {
  if (process.env.ZCALL_WINE) return process.env.ZCALL_WINE;

  // 1. wine from PATH
  const which = spawnSync('which', ['wine'], { encoding: 'utf8' });
  if (which.status === 0 && which.stdout.trim()) return which.stdout.trim();

  // 2. Bottles flatpak kron4ek runners (known-good on this setup)
  const runnersRoot = path.join(os.homedir(), '.var', 'app', 'com.usebottles.bottles', 'data', 'bottles', 'runners');
  if (fs.existsSync(runnersRoot)) {
    const entries = fs.readdirSync(runnersRoot).sort().reverse();
    for (const name of entries) {
      if (!/^kron4ek-wine/.test(name)) continue;
      const candidate = path.join(runnersRoot, name, 'bin', 'wine');
      if (fs.existsSync(candidate)) return candidate;
    }
  }

  return null;
}

function findDownloadedWine(userDataDir) {
  const p = path.join(userDataDir, RUNTIME_DIRNAME, 'bin', 'wine');
  if (!fs.existsSync(p)) return null;
  // The runtime must match the variant the app is configured to download
  // (classic vs wow64). A stale variant from an older release would
  // silently keep the crashing wow64 path alive on affected machines.
  const expected = process.env.ZCALL_WINE_DOWNLOAD_URL || WINE_DOWNLOAD_URL;
  let source = null;
  try { source = fs.readFileSync(path.join(userDataDir, RUNTIME_DIRNAME, '.zcall-wine-source'), 'utf8').trim(); } catch (e) { /* old dir, no marker */ }
  if (source !== expected) {
    // Mismatched or marker-less: discard — the next install pass (which
    // wipes before extracting) or the setup dialog replaces it.
    try { fs.rmSync(path.join(userDataDir, RUNTIME_DIRNAME), { recursive: true, force: true }); } catch (e) { /* locked — treated as absent */ }
    return null;
  }
  return p;
}

/** Classic (non-wow64) wine? The classic build ships i386-unix; pure-wow64
 * does not. Classic has no WoW64 marshaling boundary — the qcap camera bug
 * that crashes ZaloCall on affected hosts cannot trigger there. */
function isClassicWine(winePath) {
  const libWine = path.join(path.dirname(winePath), '..', 'lib', 'wine');
  return fs.existsSync(path.join(libWine, 'i386-unix'));
}

/**
 * Wine bundled inside the "Full" AppImage variant (app/native/wine-runtime).
 * In the packaged app, app/ sits at the AppImage mount root next to the
 * executable; in dev mode it is the repo's app/ directory.
 */
function findBundledWine() {
  const candidates = [
    path.join(path.dirname(process.execPath), 'app', 'native', 'wine-runtime', 'bin', 'wine'),
    path.join(__dirname, '..', '..', 'app', 'native', 'wine-runtime', 'bin', 'wine')
  ];
  return candidates.find((p) => fs.existsSync(p)) || null;
}

/**
 * Bundled 64-bit GStreamer tree. Resolution order:
 *  1. env ZCALL_GST_RUNTIME (exported by launch()/promptAndInstall — being
 *     env-first is what lets bridgeTools(), which passes no userDataDir,
 *     pick up a downloaded tree automatically)
 *  2. Full variants: app/native/gst-runtime (packaged/dev layouts)
 *  3. standard variants: <userData>/zcall-gst-runtime (first-run download)
 * Returns null when none exists — the call spawn then falls back to the
 * host's 64-bit GStreamer.
 */
function findBundledGstRuntime(userDataDir) {
  if (process.env.ZCALL_GST_RUNTIME) return process.env.ZCALL_GST_RUNTIME;
  const hasMarker = (root) => fs.existsSync(path.join(root, GST_RUNTIME_MARKER));
  const bundled = [
    path.join(path.dirname(process.execPath), 'app', 'native', 'gst-runtime'),
    path.join(__dirname, '..', '..', 'app', 'native', 'gst-runtime')
  ].find(hasMarker);
  if (bundled) return bundled;
  if (userDataDir && hasMarker(path.join(userDataDir, GST_DIRNAME))) {
    return path.join(userDataDir, GST_DIRNAME);
  }
  return null;
}

/**
 * Which LD_PRELOAD shim matches this wine build?
 *  - classic wine keeps 32-bit unixlibs — the ZaloCall process is 32-bit,
 *    only a 32-bit shim can intercept.
 *  - pure wow64 hosts the 32-bit PE in ONE 64-bit process — only a 64-bit
 *    shim can intercept there.
 * Decision rule: 64-bit ONLY on positive wow64 evidence — an adjacent
 * lib/wine tree that HAS x86_64-unix and has NO i386-unix (Kron4ek wow64
 * layout of the bundled/downloaded runtimes). Everything else (classic
 * trees, distro system wines whose unixlibs live elsewhere like
 * /usr/lib/i386-linux-gnu/wine, unknown paths) keeps the 32-bit shim —
 * exactly today's behavior. A wrong-class preload never breaks the app
 * (loader refuses it), but a wrong 64-bit choice silently disables screen
 * proxying, so defaulting conservative is the right trade.
 */
function selectProxySo(winePath) {
  const name = isClassicWine(winePath) ? 'streamproxy.so' : 'streamproxy-x86_64.so';
  return zcallBridgePath(name);
}

/**
 * Single assignment point for "which wine + which shim is live". Used by the
 * launch() winner and all mid-session wine-change sites so ZCALL_PROXY_SO
 * can never go stale when the user switches wine from the settings dialog.
 */
function applyWineEnv(wine, prefix) {
  process.env.ZCALL_WINE = wine;
  process.env.ZCALL_WINEPREFIX = prefix;
  // Wine itself reads WINEPREFIX — without this, the patched pipebridge/
  // ZaloCall spawns (which inherit process.env) would fall back to the
  // DEFAULT prefix and silently create ~/.wine on the first call.
  process.env.WINEPREFIX = prefix;
  if (!process.env.WINEDEBUG) process.env.WINEDEBUG = '-all';
  const proxy = selectProxySo(wine);
  if (fs.existsSync(proxy)) process.env.ZCALL_PROXY_SO = proxy;

  // Bundled 32-bit GStreamer — the ONLY call path. Ubuntu 24.04's i386
  // gst 1.24 + libv4l 1.26 stalls UVC cameras (DQBUF EPIPE) in wine's
  // capture path, so every wine flavor gets the bundled 26.04 stack
  // (gst 1.28 + libv4l 1.32). The 64-bit gst-runtime no longer serves
  // calls at all — it is only the Wayland screen-share bridge's stack.
  const gst32 = zcallBridgePath('gst-i386', 'usr', 'lib', 'i386-linux-gnu');
  if (fs.existsSync(gst32)) {
    process.env.ZCALL_GST_RUNTIME_I386 = gst32;
    process.env.ZCALL_GST_REGISTRY =
      path.join(os.homedir(), '.config', 'ZaloData', 'gst-registry-32.bin');
  }
}

/**
 * Verify that this wine can actually run 32-bit executables (ZaloCall is a
 * PE32 binary). Runs pipebridge.exe --version (a 32-bit exe) with a timeout.
 * Returns true only when it prints the expected output.
 */
function findPipebridgePath() {
  const candidates = [
    // packaged AppImage: app/ sits at the mount root, next to the executable
    path.join(path.dirname(process.execPath), 'app', 'native', 'qt-call-and-cap', 'pipebridge.exe'),
    // packaged alternative: extraFiles under resources/
    path.join(process.resourcesPath || '', 'app', 'native', 'qt-call-and-cap', 'pipebridge.exe'),
    // dev layout: repo/plugins/zcall-bridge -> repo/app/...
    path.join(__dirname, '..', '..', 'app', 'native', 'qt-call-and-cap', 'pipebridge.exe'),
  ];
  for (const p of candidates) {
    if (fs.existsSync(p)) return p;
  }
  return null;
}

/**
 * Real filesystem path into the zcall-bridge directory. These files are
 * spawned or LD_PRELOADed, so they must live OUTSIDE the asar archive: the
 * package config ships zcall-bridge/ next to app/ at the AppImage mount
 * root. Returns the packaged path when present, else the dev layout.
 */
function zcallBridgePath(...parts) {
  const candidates = [
    path.join(path.dirname(process.execPath), 'zcall-bridge', ...parts),
    path.join(__dirname, '..', '..', 'zcall-bridge', ...parts)
  ];
  for (const p of candidates) {
    if (fs.existsSync(p)) return p;
  }
  return candidates[1];
}

// Console output is redirected by Zalo's own logger after bootstrap, so
// diagnostics go to a file the user can inspect.
function debugLog(msg) {
  try {
    const p = path.join(os.homedir(), '.config', 'ZaloData', 'zcall-debug.log');
    fs.appendFileSync(p, new Date().toISOString() + ' ' + msg + '\n');
  } catch (e) { /* ignore */ }
}

function validateWine(winePath, prefix) {
  const pipebridgePath = findPipebridgePath();
  if (!pipebridgePath) {
    debugLog('validate: pipebridge.exe not found (resourcesPath=' + (process.resourcesPath || '') + ')');
    return false;
  }
  // Validate directly against the REAL prefix — it already exists at this
  // point (wineboot ran first in the launch loop, and the settings-dialog
  // path has a prefix from the first launch), so there is no cold-creation
  // cost. A separate throwaway prefix used to be created here, but wine
  // auto-initializes a missing prefix on first process start — that cost
  // ~935MB of writes per launch and the prefix it protected was already
  // touched by wineboot anyway. A version upgrade/downgrade pass, if any,
  // would happen identically at the first real call with that wine.
  const valPrefix = prefix;
  try {
    // Wine version floor: distro wines are often old (Ubuntu 24.04 ships
    // wine 9.0) and video calls are only verified on 11.x. Reject older
    // builds so the incompatible-wine flow offers the app's verified 11.17
    // classic download instead of a silent half-working call feature.
    const ver = spawnSync(winePath, ['--version'], { encoding: 'utf8', timeout: 30000 });
    const vm = /wine-(\d+)\.(\d+)/.exec(ver.stdout || '');
    if (ver.status !== 0 || !vm || parseInt(vm[1], 10) < 11) {
      debugLog('validate REJECT old wine=' + winePath + ' version=' + String(ver.stdout || '').trim());
      console.error('[zcall-bridge] wine quá cũ (cần ≥ 11):', winePath);
      return false;
    }
    const res = spawnSync(winePath, [pipebridgePath, '--version'], {
      env: Object.assign({}, process.env, { WINEPREFIX: valPrefix, WINEDEBUG: '-all' }),
      encoding: 'utf8',
      timeout: 120000
    });
    if (res.status === 0 && /pipebridge/.test(res.stdout || '')) return true;
    debugLog('validate FAILED wine=' + winePath + ' pipebridge=' + pipebridgePath +
      ' prefix=' + valPrefix +
      ' status=' + res.status +
      ' spawnError=' + (res.error ? res.error.message : '') +
      ' stdout=' + String(res.stdout || '').slice(0, 200) +
      ' stderr=' + String(res.stderr || '').split('\n').slice(0, 4).join(' | '));
    console.error('[zcall-bridge] wine validation failed:', winePath, '(xem zcall-debug.log)');
    return false;
  } catch (e) {
    debugLog('validate THREW wine=' + winePath + ' ' + e.message);
    console.error('[zcall-bridge] wine validation threw:', e.message);
    return false;
  }
}

/**
 * Kill leftover wine processes of OUR prefix (e.g. winedevice orphans left
 * by an unclean kill -9 of a previous session). Safe to run at launch: no
 * legit session exists yet.
 */
function sweepStaleProcesses(prefix) {
  // Leftover helper processes from unclean previous exits (kill -9 etc.)
  killProcessesByPattern('qt-call-and-cap');
  killProcessesByPattern('PipeZCall');
  try {
    for (const name of ['wineserver', 'winedevice.exe']) {
      let out = '';
      try { out = execSync('pgrep -x ' + name, { encoding: 'utf8' }); } catch (_) { continue; }
      for (const pid of out.trim().split('\n')) {
        if (!pid) continue;

        // winedevice is legit only when its parent is a live wineserver;
        // orphans get reparented to systemd/init and must be killed.
        if (name === 'winedevice.exe') {
          let ppid = '';
          try {
            const stat = fs.readFileSync('/proc/' + pid + '/stat', 'utf8');
            ppid = (stat.split(') ')[1] || '').split(' ')[1] || '';
          } catch (_) { continue; }
          if (parentIsWineserver(ppid)) continue; // belongs to a live wineserver
          process.kill(Number(pid), 'SIGKILL');
          continue;
        }

        // wineserver: match by the prefix in its environment
        let env = '';
        try { env = fs.readFileSync('/proc/' + pid + '/environ', 'utf8'); } catch (_) { continue; }
        if (env.includes(prefix)) {
          process.kill(Number(pid), 'SIGKILL');
        }
      }
    }
  } catch (_) { /* nothing stale */ }
}

// ---------------------------------------------------------------------------
// Portable wine download + extract
// ---------------------------------------------------------------------------

function downloadFile(url, dest, onProgress) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    https.get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        file.close();
        return downloadFile(res.headers.location, dest, onProgress).then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        file.close();
        try { fs.unlinkSync(dest); } catch (e) { /* ignore */ }
        reject(new Error('HTTP ' + res.statusCode));
        return;
      }
      const total = parseInt(res.headers['content-length'] || '0', 10);
      let got = 0;
      res.on('data', (chunk) => {
        got += chunk.length;
        if (onProgress && total) onProgress(got, total);
      });
      res.pipe(file);
      file.on('finish', () => { file.close(); resolve(); });
    }).on('error', (e) => {
      file.close();
      try { fs.unlinkSync(dest); } catch (_) { /* ignore */ }
      reject(e);
    });
  });
}

async function installDownloadedWine(userDataDir, onProgress) {
  const runtimeDir = path.join(userDataDir, RUNTIME_DIRNAME);
  const tarball = path.join(userDataDir, 'zcall-wine-download.tar.xz');
  const url = process.env.ZCALL_WINE_DOWNLOAD_URL || WINE_DOWNLOAD_URL;

  fs.mkdirSync(userDataDir, { recursive: true });
  await downloadFile(url, tarball, onProgress);

  // Wipe any previous runtime FIRST: extracting over an old tree (e.g. a
  // classic build over a wow64 one) creates a FRANKEN-WINE mixing both
  // loader flavors — subtle, host-dependent failures that are impossible
  // to diagnose (it polluted every classic-vs-wow64 comparison on Mint).
  fs.rmSync(runtimeDir, { recursive: true, force: true });
  fs.mkdirSync(runtimeDir, { recursive: true });
  execSync(`tar -xf "${tarball}" -C "${runtimeDir}" --strip-components=1`, { stdio: 'pipe' });
  fs.unlinkSync(tarball);

  const wine = path.join(runtimeDir, 'bin', 'wine');
  if (!fs.existsSync(wine)) throw new Error('wine binary not found after extract');
  // Prune the kron4ek tree to runtime-only content (mirror of pruneWineTree
  // in scripts/build.js): headers, import libs, .def/.c sources and the
  // winegcc/widl/winemaker toolchain are ~200MB of disk the call engine
  // never touches — only bin/wine, bin/wineserver and bin/wineboot spawn.
  try {
    fs.rmSync(path.join(runtimeDir, 'include'), { recursive: true, force: true });
    for (const sub of ['lib', 'lib64']) {
      const dir = path.join(runtimeDir, sub, 'wine');
      if (!fs.existsSync(dir)) continue;
      (function walk(d) {
        for (const e of fs.readdirSync(d, { withFileTypes: true })) {
          const p = path.join(d, e.name);
          if (e.isDirectory()) walk(p);
          else if (/\.(a|def|c)$/.test(e.name)) fs.rmSync(p, { force: true });
        }
      })(dir);
    }
    const keepBin = new Set(['wine', 'wineserver', 'wineboot']);
    const binDir = path.join(runtimeDir, 'bin');
    for (const e of fs.readdirSync(binDir, { withFileTypes: true })) {
      if (!keepBin.has(e.name)) fs.rmSync(path.join(binDir, e.name), { recursive: true, force: true });
    }
    debugLog('wine runtime pruned to runtime-only content');
  } catch (e) {
    debugLog('wine runtime prune failed (non-fatal): ' + e.message);
  }
  // Variant marker: the runtime dir must match the wine the app is configured
  // for. findDownloadedWine() discards a mismatch (classic vs wow64) so a
  // URL change can never silently keep serving the old variant.
  fs.writeFileSync(path.join(runtimeDir, '.zcall-wine-source'), url);
  return wine;
}

/**
 * Which GitHub repo hosts this build's release assets? Baked into
 * app/pc-dist/build-info.json at build time (CI: github.repository; local
 * builds: parsed from the origin remote) — so forks and the upstream repo
 * each host their own gst-runtime asset automatically, with no hardcoded
 * owner. Missing/old builds -> null -> non-fatal system-gst fallback.
 */
function buildInfoRepository() {
  const candidates = [
    path.join(path.dirname(process.execPath), 'app', 'pc-dist', 'build-info.json'),
    path.join(__dirname, '..', '..', 'app', 'pc-dist', 'build-info.json'),
  ];
  for (const p of candidates) {
    try {
      const info = JSON.parse(fs.readFileSync(p, 'utf8'));
      if (info && info.repository && /^[\w.-]+\/[\w.-]+$/.test(info.repository)) return info.repository;
    } catch (e) { /* none */ }
  }
  return null;
}

function gstDownloadUrl() {
  if (process.env.ZCALL_GST_DOWNLOAD_URL) return process.env.ZCALL_GST_DOWNLOAD_URL;
  const repo = buildInfoRepository();
  if (!repo) {
    debugLog('gst: no release repository in build-info.json — using system 64-bit gst');
    return null;
  }
  try {
    // Safe: installs only run after app 'ready' (main.js); in the packaged
    // app this is the Zalo version = the release tag the asset is attached to.
    const ver = require('electron').app.getVersion();
    if (ver) return `https://github.com/${repo}/releases/download/${ver}/gst-runtime-${ver}.tar.xz`;
  } catch (e) { /* not inside Electron (unit tests) */ }
  return null;
}

/**
 * First-run companion download (standard variants): the same 64-bit
 * GStreamer + Wayland-bridge tree the Full variants ship, published as a
 * per-version release asset. NON-FATAL: dev builds and old releases have no
 * asset — on any failure we return null and calls keep working with the
 * host's 64-bit gst (camera/share degraded, voice/video core unaffected).
 */
async function installDownloadedGst(userDataDir, onProgress) {
  const url = gstDownloadUrl();
  if (!url) {
    debugLog('gst: no download URL (electron/version unavailable) — using system 64-bit gst');
    return null;
  }
  const gstDir = path.join(userDataDir, GST_DIRNAME);
  const tarball = path.join(userDataDir, GST_TARBALL_NAME);
  fs.mkdirSync(userDataDir, { recursive: true });
  try {
    await downloadFile(url, tarball, onProgress);
    fs.mkdirSync(gstDir, { recursive: true });
    execSync(`tar -xf "${tarball}" -C "${gstDir}" --strip-components=1`, { stdio: 'pipe' });
    fs.unlinkSync(tarball);
  } catch (e) {
    try { fs.unlinkSync(tarball); } catch (_) { /* none */ }
    try { fs.rmSync(gstDir, { recursive: true, force: true }); } catch (_) { /* none */ }
    debugLog('gst download FAILED (non-fatal): ' + String((e && e.message) || e));
    return null;
  }
  if (!fs.existsSync(path.join(gstDir, GST_RUNTIME_MARKER))) {
    // Never adopt a partial tree — discard so findBundledGstRuntime stays null.
    try { fs.rmSync(gstDir, { recursive: true, force: true }); } catch (_) { /* none */ }
    debugLog('gst: marker missing after extract — tree discarded');
    return null;
  }
  return gstDir;
}

/** Single assignment point for the bundled/downloaded GStreamer env. */
function exportGstEnv(userDataDir, gstDir) {
  process.env.ZCALL_GST_RUNTIME = gstDir;
  process.env.ZCALL_GST_REGISTRY = path.join(userDataDir, 'gst-registry-64.bin');
  process.env.ZCALL_GST_REGISTRY_BRIDGE = path.join(userDataDir, 'gst-registry-bridge-64.bin');
}

// ---------------------------------------------------------------------------
// Friendly setup UI (ask -> progress window -> notification)
// ---------------------------------------------------------------------------

let askWindowOpen = false;

/**
 * Custom always-on-top ask window (native dialogs can get covered by the
 * Zalo main window on Linux DEs). Resolves with the user's choice.
 */
function showAskWindow(failedWine) {
  const { ipcMain } = require('electron');
  const win = new BrowserWindowModule({
    width: 540,
    height: failedWine ? 360 : 280,
    frame: false,
    resizable: false,
    movable: true,
    center: true,
    alwaysOnTop: true,
    skipTaskbar: false,
    // Tiny internal window with static HTML — node integration is safe here.
    webPreferences: { contextIsolation: false, nodeIntegration: true }
  });
  const downloadUrl = process.env.ZCALL_WINE_DOWNLOAD_URL || WINE_DOWNLOAD_URL;
  const headLine = failedWine
    ? 'Wine trên máy bạn không tương thích với tính năng gọi (cần wine ≥ 11 và chạy được ứng dụng 32-bit). Tải bản Wine tương thích?'
    : 'Tính năng gọi điện cần Wine. Tải và bật ngay bây giờ?';
  // A failed system/custom wine candidate brought us here — show the
  // distro-specific 32-bit lib hint for that manual path.
  let hintHtml = '';
  if (failedWine) {
    try { hintHtml = '<pre id="hint">' + getI386InstallHint().command.replace(/</g, '&lt;') + '</pre>'; } catch (e) { /* none */ }
  }
  const html = `<!DOCTYPE html><html><head><meta charset="utf-8"><style>
    body{font-family:sans-serif;background:#1f1f1f;color:#eee;margin:0;padding:20px 24px;-webkit-app-region:drag}
    h3{margin:0 0 8px;font-size:16px}
    p{font-size:13px;color:#ccc;margin:0 0 10px;line-height:1.45}
    #url{font-size:11px;color:#6ab;text-overflow:ellipsis;overflow:hidden;white-space:nowrap;cursor:pointer;margin-bottom:14px;-webkit-app-region:no-drag}
    #hint{font-size:11px;color:#aaa;background:#262626;padding:8px 10px;border-radius:6px;margin:0 0 12px;white-space:pre-wrap;-webkit-app-region:no-drag}
    label{font-size:13px;color:#ccc;display:block;margin-bottom:16px;-webkit-app-region:no-drag}
    .row{display:flex;justify-content:flex-end;gap:10px;-webkit-app-region:no-drag}
    button{font-size:13px;padding:8px 18px;border-radius:6px;border:none;cursor:pointer}
    #yes{background:#0a6e3c;color:#fff}
    #no{background:#3a3a3a;color:#eee}
  </style></head><body>
    <h3>Zalo — Tính năng gọi điện</h3>
    <p>${headLine}<br>
       Sẽ tải ~96MB (Wine) + ~${GST_DOWNLOAD_MB}MB (GStreamer) về lưu trong dữ liệu
       của Zalo — không cần quyền quản trị, không cài gì vào hệ thống
       (Wine có thể cần thư viện 32-bit; app sẽ hiện hướng dẫn cài khi thiếu).
       Cần ~2GB ổ đĩa trống để giải nén.</p>
    ${hintHtml}
    <div id="url" title="Mở nguồn tải trong trình duyệt">Nguồn tải: ${downloadUrl}</div>
    <label><input type="checkbox" id="never"> Không hỏi lại lần sau nếu không tải</label>
    <div class="row" style="justify-content:space-between">
      <button id="browse">Chọn file wine có sẵn…</button>
      <span>
        <button id="no">Để sau</button>
        <button id="yes">Tải và bật ngay</button>
      </span>
    </div>
    <script>
      const {ipcRenderer, shell} = require('electron');
      function answer(download) {
        ipcRenderer.send('zcall-ask-result', {
          download,
          neverAgain: document.getElementById('never').checked
        });
      }
      document.getElementById('yes').onclick = () => answer(true);
      document.getElementById('no').onclick = () => answer(false);
      document.getElementById('browse').onclick = () => ipcRenderer.send('zcall-ask-browse');
      document.getElementById('url').onclick = () => shell.openExternal('${downloadUrl}');
    </script>
  </body></html>`;
  win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(html));

  return new Promise((resolve) => {
    let resolved = false;
    const finish = (result) => {
      if (resolved) return;
      resolved = true;
      ipcMain.removeListener('zcall-ask-result', onResult);
      ipcMain.removeListener('zcall-ask-browse', onBrowse);
      resolve(result || { download: false, neverAgain: false });
      // destroy AFTER resolve: destroy() emits 'closed' synchronously,
      // which would otherwise let the fallback resolve a wrong value first
      try { win.destroy(); } catch (e) { /* already closed */ }
    };
    const onResult = (_e, result) => finish(result);
    const onBrowse = async () => {
      const picked = await dialogModule.showOpenDialog(win, {
        title: 'Chọn file wine',
        properties: ['openFile']
      });
      const chosen = picked.filePaths && picked.filePaths[0];
      if (!chosen) return;
      if (validateWine(chosen, process.env.ZCALL_WINEPREFIX || path.join(os.homedir(), '.config', 'ZaloData', 'zcall-wine'))) {
        finish({ download: false, neverAgain: false, pickedWine: chosen });
      } else {
        dialogModule.showMessageBox(win, {
          type: 'error',
          title: 'Zalo — Tính năng gọi điện',
          message: 'Wine này không dùng được',
          detail: 'File đã chọn không chạy được ứng dụng 32-bit hoặc không phải wine hợp lệ:\n' + chosen
        });
      }
    };
    ipcMain.on('zcall-ask-result', onResult);
    ipcMain.on('zcall-ask-browse', onBrowse);
    // fallback: user closed the window somehow
    win.on('closed', () => finish({ download: false, neverAgain: false }));
  });
}

function showProgressWindow() {
  const win = new BrowserWindowModule({
    width: 520,
    height: 165,
    frame: false,
    resizable: false,
    movable: true,
    alwaysOnTop: true,
    // Tiny internal window with static HTML — node integration is safe here.
    webPreferences: { contextIsolation: false, nodeIntegration: true }
  });
  const downloadUrl = process.env.ZCALL_WINE_DOWNLOAD_URL || WINE_DOWNLOAD_URL;
  const html = `<!DOCTYPE html><html><head><meta charset="utf-8"><style>
    body{font-family:sans-serif;background:#1f1f1f;color:#eee;margin:0;padding:18px 22px;-webkit-app-region:drag}
    h3{margin:0 0 6px;font-size:15px} p{margin:0 0 12px;font-size:12px;color:#aaa}
    progress{width:100%;height:14px}
    #label{font-size:12px;color:#aaa;margin-top:8px}
    #url{font-size:11px;color:#6ab;margin-top:8px;text-overflow:ellipsis;overflow:hidden;white-space:nowrap;cursor:pointer}
  </style></head><body>
    <h3>Zalo — Tính năng gọi điện</h3>
    <p>Đang chuẩn bị tính năng gọi điện (Wine + GStreamer), vui lòng chờ…</p>
    <progress id="bar" max="100" value="0"></progress>
    <div id="label">0%</div>
    <div id="url" title="Mở nguồn tải trong trình duyệt">${downloadUrl}</div>
    <script>
      const {ipcRenderer, shell} = require('electron');
      ipcRenderer.on('progress', (e, pct, text) => {
        document.getElementById('bar').value = pct;
        document.getElementById('label').textContent = text;
      });
      document.getElementById('url').onclick = () => {
        shell.openExternal('${downloadUrl}');
      };
    </script>
  </body></html>`;
  win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(html));
  return {
    set(pct, text) {
      try { win.webContents.send('progress', pct, text); } catch (e) { /* window closed */ }
    },
    close() {
      try { win.destroy(); } catch (e) { /* already closed */ }
    }
  };
}

async function promptAndInstall(userDataDir, failedWine) {
  getElectronModules();
  if (!BrowserWindowModule) return null;
  if (askWindowOpen) return null; // never show two ask windows
  askWindowOpen = true;

  let result;
  try {
    result = await showAskWindow(failedWine);
  } finally {
    askWindowOpen = false;
  }

  if (!result.download) {
    // User picked an existing wine file: save it and use it.
    if (result.pickedWine) {
      const prefix = process.env.ZCALL_WINEPREFIX || path.join(userDataDir, 'zcall-wine');
      writeConfig(userDataDir, { wineSetup: 'ready', winePath: result.pickedWine });
      applyWineEnv(result.pickedWine, prefix);
      console.log('[zcall-bridge] custom wine selected:', result.pickedWine);
      return result.pickedWine;
    }
    // "Để sau": ask again on next launch; ticked "không hỏi lại" -> never.
    writeConfig(userDataDir, { wineSetup: result.neverAgain ? 'declined-permanent' : 'declined' });
    return null;
  }

  const progress = showProgressWindow();
  try {
    debugLog('install: starting download of portable wine');
    let lastUpdate = 0;
    const stageText = (label) => (got, total) => {
      const now = Date.now();
      if (now - lastUpdate < 500) return; // throttle IPC updates
      lastUpdate = now;
      const pct = total ? Math.round((got / total) * 100) : 0;
      progress.set(pct, `${label}: ${Math.round(got / 1024 / 1024)}MB / ${Math.round(total / 1024 / 1024)}MB (${pct}%)`);
    };
    const wine = await installDownloadedWine(userDataDir, stageText('Đang tải Wine'));
    debugLog('install: wine downloaded, extracting...');
    progress.set(100, 'Đang giải nén Wine…');

    // Companion 64-bit GStreamer tree (standard variants). Skipped when a
    // bundle is already live (Full variant: env exported by launch()) or
    // when the system already has a usable GStreamer — the deb/rpm/pacman
    // packages install it as a dependency, so re-downloading the asset
    // would be pure waste.
    let gstDir = null;
    if (!findBundledGstRuntime(userDataDir) && !hasSystemGst()) {
      progress.set(0, 'Đang tải GStreamer…');
      gstDir = await installDownloadedGst(userDataDir, stageText('Đang tải GStreamer'));
      progress.set(100, gstDir ? 'Đang giải nén GStreamer…' : 'Không tải được GStreamer (sẽ dùng bản hệ thống)');
    } else if (!findBundledGstRuntime(userDataDir)) {
      debugLog('install: hệ thống đã có GStreamer (pipewiresrc) — bỏ qua tải asset');
    }

    // First prefix init (~10-30s, done once)
    const prefix = process.env.ZCALL_WINEPREFIX || path.join(userDataDir, 'zcall-wine');
    progress.set(0, 'Đang khởi tạo lần đầu (wineboot)…');
    spawnSync(wine, ['wineboot', '-u'], {
      env: Object.assign({}, process.env, { WINEPREFIX: prefix, WINEDEBUG: '-all' }),
      stdio: 'ignore',
      timeout: 180000
    });

    // Verify the freshly downloaded wine actually works on this machine.
    // The classic build needs the host's 32-bit libraries — a failure here
    // is usually missing i386 packages (the dialog below shows the exact
    // distro command), a too-old glibc, or a corrupt download.
    if (!validateWine(wine, prefix)) {
      throw new Error(
        'Wine tải về không chạy được trên máy này.\n\n' +
        'Thường do thiếu thư viện 32-bit — cài theo lệnh trong cửa sổ hướng dẫn\n' +
        '(hoặc mục "Cài đặt gọi điện" → xóa rồi tải lại).\n' +
        'Nếu đã cài đủ mà vẫn lỗi: máy thiếu glibc ≥ 2.35 hoặc file tải về bị hỏng.'
      );
    }

    progress.close();
    debugLog('install: SUCCESS wine=' + wine + ' gst=' + (gstDir || 'system') + ' prefix=' + prefix);

    applyWineEnv(wine, prefix);
    if (gstDir) exportGstEnv(userDataDir, gstDir);
    writeConfig(userDataDir, { wineSetup: 'ready' });

    if (NotificationModule && NotificationModule.isSupported()) {
      new NotificationModule({
        title: 'Zalo',
        body: 'Tính năng gọi điện đã sẵn sàng! Hãy thử gọi một cuộc.'
      }).show();
    }
    return wine;
  } catch (e) {
    debugLog('install FAILED: ' + String((e && e.message) || e) + '\n' + String((e && e.stack) || '').split('\n').slice(0, 3).join('\n'));
    progress.close();
    const parent = BrowserWindowModule.getFocusedWindow() || BrowserWindowModule.getAllWindows()[0];
    if (dialogModule) {
      await dialogModule.showMessageBox(parent, {
        type: 'error',
        title: 'Zalo — Tính năng gọi điện',
        message: 'Không thể tải Wine',
        detail: String((e && e.message) || e) + '\n\nBạn có thể thử lại từ menu khay hệ thống, hoặc cài Wine bằng lệnh: sudo apt install wine'
      });
    }
    return null;
  }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Is a usable 64-bit GStreamer already on the system? The deb/rpm/pacman
 * packages install gst + the pipewire plugin as dependencies, so
 * downloading the release asset would be redundant — check what the bridge
 * actually needs (gst-launch present + pipewiresrc registering).
 */
function hasSystemGst() {
  try {
    execSync('command -v gst-launch-1.0', { stdio: 'pipe' });
    const out = execSync('gst-inspect-1.0 pipewiresrc 2>/dev/null | head -5', {
      encoding: 'utf8', stdio: 'pipe'
    });
    return /pipewiresrc/.test(out);
  } catch (e) {
    return false;
  }
}

/**
 * Mirror wine's own prefix-update no-op check: wineboot compares the
 * prefix's .update-timestamp with the mtime of share/wine/wine.inf and
 * skips the update pass when they match (the stamp stores wine.inf's mtime
 * in seconds; "disable" opts out entirely). Returns true when an update
 * would actually run, or when the stamp is unreadable (let wine decide).
 * Needed because `wineboot -u` FORCES the update regardless of the stamp —
 * running it unconditionally re-shows wine's "The wine configuration in ...
 * is being updated, please wait" dialog on every launch.
 */
function winePrefixNeedsUpdate(wineBin, prefix) {
  try {
    const wineTree = path.resolve(path.dirname(wineBin), '..');
    const inf = path.join(wineTree, 'share', 'wine', 'wine.inf');
    const stamp = fs.readFileSync(path.join(prefix, '.update-timestamp'), 'utf8').trim();
    if (stamp === 'disable') return false;
    const infMtime = Math.floor(fs.statSync(inf).mtimeMs / 1000);
    return stamp !== String(infMtime);
  } catch (e) {
    return true; // no stamp / no wine.inf — let wineboot sort it out
  }
}

function launch({ userDataDir }) {
  if (process.env.ZCALL_DISABLE) return false;

  const prefix = process.env.ZCALL_WINEPREFIX || path.join(userDataDir, 'zcall-wine');

  // Bundled/downloaded 64-bit GStreamer (Full bundle or standard-variant
  // first-run download). Wine-independent: exported once here (before the
  // candidate loop so the deferred promptAndInstall path gets it too); the
  // patched ZaloCall spawn composes LD_LIBRARY_PATH and GST_* from it.
  // Never set those vars globally — they would leak into
  // screenbridge/validate/wineboot spawns.
  const gstRuntime = findBundledGstRuntime(userDataDir);
  if (gstRuntime) exportGstEnv(userDataDir, gstRuntime);

  // Clean stale wine processes from unclean previous exits
  sweepStaleProcesses(prefix);

  // A crashed previous run can leave the throwaway validation prefix behind
  // (100-300MB) — its wineserver matched the sweep pattern above, so the
  // directory should be removable now.
  try {
    fs.rmSync(prefix + '-validate', { recursive: true, force: true });
  } catch (e) { /* locked — will be retried on a later launch */ }

  // Candidate wines, best first: explicit env -> user-picked custom wine ->
  // our portable runtime (version we control and test) -> system wine ->
  // Bottles runners.
  const downloadedWine = findDownloadedWine(userDataDir);
  const systemWine = findWine();
  const candidates = [];
  if (process.env.ZCALL_WINE) {
    // resolve relative paths — AppImage runs may change the working directory
    candidates.push(path.resolve(process.env.ZCALL_WINE));
  }
  const cfgSaved = readConfig(userDataDir);
  if (cfgSaved.winePath && fs.existsSync(cfgSaved.winePath) && candidates.indexOf(cfgSaved.winePath) === -1) {
    candidates.push(cfgSaved.winePath);
  }
  // Bundled runtime first among the auto-discovered ones: it is paired with
  // the exact app release (Full variant) and needs no download.
  const bundledWine = findBundledWine();
  if (bundledWine) candidates.push(bundledWine);
  if (downloadedWine && candidates.indexOf(downloadedWine) === -1) candidates.push(downloadedWine);
  if (systemWine && candidates.indexOf(systemWine) === -1) candidates.push(systemWine);

  let wine = null;
  let failedWine = null;
  for (const candidate of candidates) {
    // Ensure the prefix exists before validating (validation needs a booted prefix)
    if (!fs.existsSync(path.join(prefix, 'drive_c'))) {
      console.log('[zcall-bridge] initializing wine prefix:', prefix);
      try {
        spawnSync(candidate, ['wineboot', '-u'], {
          env: Object.assign({}, process.env, { WINEPREFIX: prefix, WINEDEBUG: '-all' }),
          stdio: 'ignore',
          timeout: 180000
        });
      } catch (e) {
        console.error('[zcall-bridge] wineboot failed:', e.message);
      }
    }

    // A wine is only usable if it can run 32-bit executables.
    // ZCALL_NO_VALIDATE=1 (debug): skip validation — take the first
    // candidate. Bisect lever for the Mint video-call crash: the old
    // releases validated against a THROWAWAY prefix, never touching the
    // real one at launch.
    if (process.env.ZCALL_NO_VALIDATE === '1' || validateWine(candidate, prefix)) {
      wine = candidate;
      break;
    }
    console.error('[zcall-bridge] wine cannot run 32-bit apps, skipping:', candidate);
    failedWine = candidate;
  }

  if (!wine) {
    const cfg = readConfig(userDataDir);

    // Downloaded runtime exists but cannot run: re-downloading would loop
    // forever — guide the user instead, once, without nagging every launch.
    // (The classic build needs the host's 32-bit libs; a failure here is
    // usually missing i386 packages — the settings dialog shows the exact
    // distro command — or a too-old glibc / corrupt download.)
    if (downloadedWine && failedWine === downloadedWine) {
      console.error('[zcall-bridge] downloaded wine broken');
      if (cfg.wineSetup !== 'broken' && process.env.ZCALL_AUTO_SETUP !== '1') {
        writeConfig(userDataDir, { wineSetup: 'broken' });
        showBrokenWineDialog(userDataDir, downloadedWine);
      }
      return false;
    }

    // No usable wine: ask the user (async — never block the ready handler).
    // Silent only when the user ticked "không hỏi lại" on a previous
    // decline, unless ZCALL_AUTO_SETUP=1 forces a silent download.
    if (cfg.wineSetup === 'declined-permanent' && process.env.ZCALL_AUTO_SETUP !== '1') {
      console.error('[zcall-bridge] wine setup declined permanently — calls unavailable');
      return false;
    }
    console.log('[zcall-bridge] no usable wine, prompting user to set up...');
    promptAndInstall(userDataDir, failedWine).then((w) => {
      if (w) console.log('[zcall-bridge] portable wine ready:', w);
    }).catch((e) => console.error('[zcall-bridge] setup failed:', e.message));
    return false;
  }

  // Export for the patched main-dist spawn code (also picks the matching
  // shim: 32-bit for classic wines like the bundled Full runtime, 64-bit
  // for pure-wow64 wines).
  applyWineEnv(wine, prefix);

  // Complete any pending prefix update SYNCHRONOUSLY — but ONLY when one is
  // actually pending. `wineboot -u` FORCES the prefix update pass regardless
  // of state (wineboot.c: `if (init || update) update_wineprefix( update )`
  // passes force through), re-running rundll32 wine.inf and showing wine's
  // "The wine configuration in ... is being updated, please wait" dialog on
  // EVERY launch. Mirror wine's own no-op check (.update-timestamp vs
  // wine.inf mtime) and only then run wineboot -u, which waits for
  // completion and persists the stamp — so a lazily-aborted update from a
  // previous session (e.g. the user alternated wine builds) settles here
  // instead of re-running mid-call.
  // ZCALL_SKIP_WINEBOOT=1 (debug): bisect lever — the old releases never
  // touched the real prefix at launch.
  if (process.env.ZCALL_SKIP_WINEBOOT !== '1' && winePrefixNeedsUpdate(wine, prefix)) {
    try {
      spawnSync(wine, ['wineboot', '-u'], {
        env: Object.assign({}, process.env, { WINEPREFIX: prefix, WINEDEBUG: '-all' }),
        stdio: 'ignore',
        timeout: 180000
      });
    } catch (e) {
      debugLog('wineboot -u at launch failed: ' + e.message);
    }
  }

  // NOTE: do NOT pin the Audio driver here. Pinning "pulseaudio" broke the
  // microphone (winepulse capture fails on pipewire-pulse — the mic
  // vanishes from the call); the default "pulseaudio,alsa" order keeps the
  // mic working via the ALSA fallback. The old machine-wide audio break
  // (winealsa grabbing hw:0,0 after a wedged pulse link) is mitigated by
  // the bridge now closing its portal sessions cleanly, so the wedge
  // trigger is gone. If the wedge ever returns, fix the root cause
  // instead of re-pinning.

  // Streamproxy: the capture shim is preloaded into the helper at ALL times.
  // It is inert while the bridge display is down (captures fall through to
  // the real display) and it signals a share request via a file, which the
  // watcher below turns into an automatic bridge start — no tray click
  // needed when the user hits "Share screen".
  if (process.env.ZCALL_PROXY_SO) {
    process.env.ZCALL_PROXY_LOG = path.join(os.homedir(), '.config', 'ZaloData', 'zcall-proxy.log');
    process.env.ZCALL_PROXY_REQUEST = path.join(os.homedir(), '.config', 'ZaloData', 'zcall-share.request');
    // The shim writes the region ZaloCall actually captures ("x y w h") —
    // the bridge parks the gst window there (see trackCaptureRegion).
    process.env.ZCALL_PROXY_REGION = path.join(os.homedir(), '.config', 'ZaloData', 'zcall-share.region');
    // Touched by the shim while captures are live; a stale heartbeat means
    // the share ended and the bridge must be torn down.
    process.env.ZCALL_PROXY_HEARTBEAT = path.join(os.homedir(), '.config', 'ZaloData', 'zcall-share.heartbeat');
    // Warm the resolution cache now so the first share-screen request does
    // not pay the synchronous xrandr call while the user waits.
    try {
      cachedBridgeRes = bridgeScreenRes();
    } catch (e) { /* default */ }
    watchShareRequests();
  }

  // Camera capture: classic wine uses the bundled 32-bit GStreamer stack
  // (see applyWineEnv). Ubuntu 24.04's i386 gst 1.24 + libv4l 1.26 stalls
  // UVC cameras (DQBUF EPIPE) in wine's capture path — the bundled 26.04
  // stack (gst 1.28 + libv4l 1.32) fixes it everywhere, no loopback needed.

  console.log('[zcall-bridge] wine ready:', wine, '(prefix:', prefix + ')');
  return true;
}

/**
 * Watches the share-request file touched by the streamproxy shim when
 * ZaloCall starts capturing while the bridge display is down. Starting the
 * bridge pops the compositor's permission dialog automatically.
 */
let lastAutoBridgeAt = 0;
function watchShareRequests() {
  setInterval(() => {
    const f = process.env.ZCALL_PROXY_REQUEST;
    if (!f || !fs.existsSync(f)) return;
    try { fs.unlinkSync(f); } catch (e) { /* gone */ }
    if (!isWaylandSession() || screenBridgeActive()) return;
    // Cooldown: if the user denied the portal, don't nag again right away
    // (the tray menu remains available for a manual retry).
    if (Date.now() - lastAutoBridgeAt < 90000) return;
    lastAutoBridgeAt = Date.now();
    debugLog('screenbridge: share request detected — starting bridge');
    startScreenBridge();
  }, 200);
}

/**
 * Distro-specific command to install the 32-bit libraries the portable wine
 * needs. Detects the distro from /etc/os-release.
 */
function getI386InstallHint() {
  let idLike = '';
  try {
    const osRelease = fs.readFileSync('/etc/os-release', 'utf8');
    const m = osRelease.match(/^ID(?:_LIKE)?=(.+)$/gm);
    idLike = (m || []).join('\n').toLowerCase();
  } catch (e) { /* unknown distro */ }

  if (idLike.includes('fedora') || idLike.includes('rhel') || idLike.includes('centos')) {
    return {
      title: 'Cài thư viện 32-bit (Fedora/RHEL):',
      command: 'sudo dnf install -y glibc.i686 libX11.i686 libXext.i686 freetype.i686 mesa-libGL.i686 pulseaudio-libs.i686 alsa-lib.i686 zlib-ng-compat.i686\n\n(GStreamer 32-bit đã có sẵn trong app — không cần cài)\n\nHoặc cài wine hệ thống (tự kéo đủ thư viện):\nsudo dnf install wine'
    };
  }
  if (idLike.includes('arch')) {
    return {
      title: 'Cài thư viện 32-bit (Arch):',
      command: 'sudo pacman -S --needed lib32-glibc lib32-libx11 lib32-libxext lib32-freetype2 lib32-mesa lib32-libpulse lib32-alsa-lib lib32-zlib\n\n(GStreamer 32-bit đã có sẵn trong app — không cần cài)\n\nHoặc cài wine hệ thống:\nsudo pacman -S wine'
    };
  }
  // default: Debian/Ubuntu family
  return {
    title: 'Cài thư viện 32-bit (Ubuntu/Debian):',
    command: 'sudo dpkg --add-architecture i386 && sudo apt update\nsudo apt install -y libc6:i386 libx11-6:i386 libfreetype6:i386 libgl1:i386 libpulse0:i386 libasound2:i386 zlib1g:i386\n\n(GStreamer 32-bit đã có sẵn trong app — không cần cài)\n\nHoặc cài wine hệ thống (tự kéo đủ thư viện):\nsudo apt install wine'
  };
}

function showBrokenWineDialog(userDataDir, winePath) {
  getElectronModules();
  if (!dialogModule) return;
  const parent = BrowserWindowModule.getFocusedWindow() || BrowserWindowModule.getAllWindows()[0];
  dialogModule.showMessageBox(parent, {
    type: 'warning',
    title: 'Zalo — Tính năng gọi điện',
    message: 'Wine tải về không chạy được trên máy này',
    detail: 'Bản wine tải về (classic) cần thư viện 32-bit của hệ thống — ' +
      'lỗi thường do thiếu gói i386, hệ thống quá cũ (cần glibc ≥ 2.35) ' +
      'hoặc file bị hỏng khi tải.\n\n' +
      'Cài thư viện 32-bit theo hướng dẫn trong mục Cài đặt gọi điện, ' +
      'hoặc xóa bản cũ và tải lại.\n' +
      'Wine đã tải: ' + winePath,
    buttons: ['Xóa và tải lại', 'Để sau'],
    defaultId: 1,
    cancelId: 1
  }).then(({ response }) => {
    if (response !== 0) return;
    try { fs.rmSync(path.join(userDataDir, RUNTIME_DIRNAME), { recursive: true, force: true }); } catch (_) { /* none */ }
    promptAndInstall(userDataDir);
  });
}

function openSetupDialog({ userDataDir }) {
  getElectronModules();
  if (!BrowserWindowModule) return;
  const { ipcMain } = require('electron');
  const prefix = process.env.ZCALL_WINEPREFIX || path.join(userDataDir, 'zcall-wine');

  const win = new BrowserWindowModule({
    width: 600,
    height: 460,
    frame: false,
    resizable: false,
    center: true,
    alwaysOnTop: true,
    webPreferences: { contextIsolation: false, nodeIntegration: true }
  });
  const html = `<!DOCTYPE html><html><head><meta charset="utf-8"><style>
    body{font-family:sans-serif;background:#1f1f1f;color:#eee;margin:0;padding:20px 24px;-webkit-app-region:drag}
    h3{margin:0 0 10px;font-size:16px}
    #status{font-size:13px;color:#ccc;background:#2a2a2a;border-radius:6px;padding:10px 12px;margin-bottom:14px;line-height:1.5;word-break:break-all}
    button{display:block;width:100%;font-size:13px;padding:10px;margin-bottom:10px;border-radius:6px;border:none;cursor:pointer;background:#3a3a3a;color:#eee;-webkit-app-region:no-drag}
    button:hover{background:#4a4a4a}
    .pathrow{display:flex;gap:8px;margin-bottom:10px;-webkit-app-region:no-drag}
    .pathrow input{flex:1;font-size:13px;padding:9px 10px;border-radius:6px;border:1px solid #4a4a4a;background:#2a2a2a;color:#eee}
    .pathrow button{width:auto;margin:0;white-space:nowrap}
    #close{background:#2a2a2a}
  </style></head><body>
    <h3>Zalo — Cài đặt gọi điện</h3>
    <div id="status">Đang kiểm tra…</div>
    <div class="pathrow">
      <input id="pathInput" placeholder="Nhập đường dẫn wine, ví dụ /usr/bin/wine">
      <button id="setpath">Dùng đường dẫn này</button>
    </div>
    <button id="browse">Chọn file wine khác…</button>
    <button id="download"></button>
    <button id="clear">Bỏ lựa chọn wine đã lưu</button>
    <button id="remove">Xóa Wine + GStreamer đã tải về khỏi máy</button>
    <button id="close">Đóng</button>
    <script>
      const {ipcRenderer} = require('electron');
      // pass the command as the IPC argument so the main handler can match it
      const send = (cmd, arg) => ipcRenderer.send(cmd, arg || cmd);
      const closeWin = () => { send('zcall-cfg-close'); setTimeout(() => window.close(), 80); };
      document.getElementById('browse').onclick = () => send('zcall-cfg-browse');
      document.getElementById('download').onclick = () => send('zcall-cfg-download');
      document.getElementById('clear').onclick = () => send('zcall-cfg-clear');
      document.getElementById('remove').onclick = () => send('zcall-cfg-remove');
      document.getElementById('setpath').onclick = () => send('zcall-cfg-setpath', document.getElementById('pathInput').value.trim());
      document.getElementById('close').onclick = closeWin;
      document.addEventListener('keydown', (e) => { if (e.key === 'Escape') closeWin(); });
      ipcRenderer.on('zcall-cfg-status', (e, text) => {
        document.getElementById('status').textContent = text;
      });
      ipcRenderer.on('zcall-cfg-download-label', (e, text) => {
        document.getElementById('download').textContent = text;
      });
    </script>
  </body></html>`;
  win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(html));

  const currentWine = () => {
    if (process.env.ZCALL_WINE) return { w: process.env.ZCALL_WINE, src: 'biến môi trường' };
    const cfg = readConfig(userDataDir);
    if (cfg.winePath && fs.existsSync(cfg.winePath)) return { w: cfg.winePath, src: 'lựa chọn đã lưu' };
    const bundled = findBundledWine();
    if (bundled) return { w: bundled, src: 'bundle trong app (Full)' };
    const dl = findDownloadedWine(userDataDir);
    if (dl) return { w: dl, src: 'đã tải về' };
    const sys = findWine();
    if (sys) return { w: sys, src: 'wine hệ thống' };
    return null;
  };
  const pushStatus = () => {
    const cw = currentWine();
    let text = cw
      ? 'Wine đang dùng (' + cw.src + '):\n' + cw.w
      : 'Chưa có wine — tính năng gọi chưa hoạt động.';
    if (!findBundledGstRuntime(userDataDir) && !hasSystemGst()) {
      text += '\n(GStreamer 64-bit chưa có — cần cho share screen trên Wayland)';
    }
    // When a working wine is already live, downloading is just an
    // alternative, not a requirement — relabel accordingly.
    const dlLabel = cw
      ? 'Tải wine riêng của app (thay wine đang dùng)'
      : 'Tải Wine + GStreamer về (~' + (96 + GST_DOWNLOAD_MB) + 'MB)';
    try {
      win.webContents.send('zcall-cfg-status', text);
      win.webContents.send('zcall-cfg-download-label', dlLabel);
    } catch (e) { /* closed */ }
  };
  pushStatus();

  const onIpc = (_e, cmd, arg) => {
    if (cmd === 'zcall-cfg-close') { try { win.destroy(); } catch (e) {} return; }
    if (cmd === 'zcall-cfg-browse') {
      dialogModule.showOpenDialog(win, { title: 'Chọn file wine', properties: ['openFile'] }).then((picked) => {
        const chosen = picked.filePaths && picked.filePaths[0];
        if (!chosen) return;
        if (validateWine(chosen, prefix)) {
          writeConfig(userDataDir, { wineSetup: 'ready', winePath: chosen });
          applyWineEnv(chosen, prefix);
          pushStatus();
          new NotificationModule({ title: 'Zalo', body: 'Đã chọn wine: ' + chosen }).show();
        } else {
          dialogModule.showMessageBox(win, {
            type: 'error', title: 'Zalo — Tính năng gọi điện',
            message: 'Wine này không dùng được',
            detail: 'File đã chọn không chạy được ứng dụng 32-bit:\n' + chosen
          });
        }
      });
      return;
    }
    if (cmd === 'zcall-cfg-setpath') {
      const typed = String(arg || '').trim();
      if (!typed) {
        dialogModule.showMessageBox(win, {
          type: 'info', title: 'Zalo — Tính năng gọi điện',
          message: 'Chưa nhập đường dẫn',
          detail: 'Hãy nhập đường dẫn đầy đủ tới file wine, ví dụ /usr/bin/wine'
        });
        return;
      }
      if (!fs.existsSync(typed)) {
        dialogModule.showMessageBox(win, {
          type: 'error', title: 'Zalo — Tính năng gọi điện',
          message: 'Không tìm thấy file',
          detail: 'Đường dẫn không tồn tại:\n' + typed
        });
        return;
      }
      if (!validateWine(typed, prefix)) {
        dialogModule.showMessageBox(win, {
          type: 'error', title: 'Zalo — Tính năng gọi điện',
          message: 'Wine này không dùng được',
          detail: 'File không chạy được ứng dụng 32-bit hoặc không phải wine hợp lệ:\n' + typed
        });
        return;
      }
      writeConfig(userDataDir, { wineSetup: 'ready', winePath: typed });
      applyWineEnv(typed, prefix);
      pushStatus();
      dialogModule.showMessageBox(win, {
        type: 'info', title: 'Zalo — Tính năng gọi điện',
        message: 'Đã lưu đường dẫn wine',
        detail: typed + '\n\nLần mở app sau sẽ dùng wine này (có thể bỏ bằng nút "Bỏ lựa chọn wine đã lưu").'
      });
      return;
    }
    if (cmd === 'zcall-cfg-download') {
      win.destroy();
      promptAndInstall(userDataDir);
      return;
    }
    if (cmd === 'zcall-cfg-clear') {
      const cfg = readConfig(userDataDir);
      if (cfg.winePath) {
        delete cfg.winePath;
        writeConfig(userDataDir, cfg);
        pushStatus();
        dialogModule.showMessageBox(win, {
          type: 'info', title: 'Zalo — Tính năng gọi điện',
          message: 'Đã bỏ lựa chọn wine đã lưu',
          detail: 'Lần mở app sau sẽ tự dò wine lại (wine tải về → wine hệ thống).\nWine đang dùng phiên này: ' + (process.env.ZCALL_WINE || '(không có)')
        });
      } else {
        dialogModule.showMessageBox(win, {
          type: 'info', title: 'Zalo — Tính năng gọi điện',
          message: 'Không có lựa chọn wine nào đang lưu',
          detail: 'App đang dùng wine theo chế độ tự dò. Không có gì để bỏ.'
        });
      }
      return;
    }
    if (cmd === 'zcall-cfg-remove') {
      const runtime = path.join(userDataDir, RUNTIME_DIRNAME);
      const gstRuntime = path.join(userDataDir, GST_DIRNAME);
      if (!fs.existsSync(runtime) && !fs.existsSync(gstRuntime)) {
        dialogModule.showMessageBox(win, {
          type: 'info', title: 'Zalo — Tính năng gọi điện',
          message: 'Không có wine đã tải về',
          detail: 'Chưa có thư mục wine tải về trên máy này.'
        });
        return;
      }
      dialogModule.showMessageBox(win, {
        type: 'warning', buttons: ['Xóa', 'Hủy'], defaultId: 1, cancelId: 1,
        title: 'Zalo — Tính năng gọi điện',
        message: 'Xóa Wine + GStreamer đã tải về?',
        detail: 'Sẽ xóa ' + runtime + '\nvà ' + gstRuntime + '\nBạn có thể tải lại bất cứ lúc nào.'
      }).then(({ response }) => {
        if (response === 0) {
          try { fs.rmSync(runtime, { recursive: true, force: true }); } catch (e) {}
          try { fs.rmSync(gstRuntime, { recursive: true, force: true }); } catch (e) {}
          try { fs.rmSync(path.join(userDataDir, GST_TARBALL_NAME), { force: true }); } catch (e) {}
          const cfg = readConfig(userDataDir);
          delete cfg.winePath;
          writeConfig(userDataDir, cfg);
          pushStatus();
          dialogModule.showMessageBox(win, {
            type: 'info', title: 'Zalo — Tính năng gọi điện',
            message: 'Đã xóa Wine + GStreamer đã tải về',
            detail: 'Các thư mục đã bị xóa. Lần mở app sau sẽ hỏi lại hoặc tự dò wine hệ thống.'
          });
        }
      });
      return;
    }
  };
  const CFG_CHANNELS = ['zcall-cfg-browse', 'zcall-cfg-download', 'zcall-cfg-clear',
                        'zcall-cfg-remove', 'zcall-cfg-close', 'zcall-cfg-setpath'];
  for (const c of CFG_CHANNELS) ipcMain.on(c, onIpc);
  win.on('closed', () => {
    for (const c of CFG_CHANNELS) {
      ipcMain.removeListener(c, onIpc);
    }
  });
}

function killProcessesByPattern(pattern) {
  try {
    const out = execSync('pgrep -f ' + pattern, { encoding: 'utf8' });
    for (const pid of out.trim().split('\n')) {
      if (!pid || Number(pid) === process.pid) continue;
      try { process.kill(Number(pid), 'SIGKILL'); } catch (_) { /* gone */ }
    }
  } catch (_) { /* no matches */ }
}


function parentIsWineserver(ppid) {
  try {
    return fs.readFileSync('/proc/' + ppid + '/comm', 'utf8').trim() === 'wineserver';
  } catch (e) {
    return false;
  }
}

function killWineSession(prefix) {
  // 1. Kill the wine loader + helper processes spawned by the app. Their
  //    cmdlines contain the engine path (dev: .../app/native/qt-call-and-cap,
  //    packaged: /tmp/.mount_zalo*/app/native/qt-call-and-cap). pipebridge
  //    sleeps forever and never exits on its own.
  killProcessesByPattern('qt-call-and-cap');
  killProcessesByPattern('PipeZCall');

  // 2. Kill the wineserver serving our prefix.
  const wine = process.env.ZCALL_WINE;
  if (wine) {
    const wineserverPath = path.join(path.dirname(wine), 'wineserver');
    if (fs.existsSync(wineserverPath)) {
      try {
        spawnSync(wineserverPath, ['-k'], {
          env: Object.assign({}, process.env, { WINEPREFIX: prefix, WINEDEBUG: '-all' }),
          stdio: 'ignore',
          timeout: 10000
        });
      } catch (e) {
        console.error('[zcall-bridge] wineserver -k failed:', e.message);
      }
    }
  }

  // 3. Hard-kill lingering wineserver/winedevice of our prefix.
  try {
    for (const name of ['wineserver', 'winedevice.exe']) {
      let out = '';
      try { out = execSync('pgrep -x ' + name, { encoding: 'utf8' }); } catch (_) { continue; }
      for (const pid of out.trim().split('\n')) {
        if (!pid) continue;
        if (name === 'winedevice.exe') {
          // winedevice is legit only when its parent is a live wineserver;
          // orphans get reparented to systemd/init and must be killed.
          let ppid = '';
          try {
            const stat = fs.readFileSync('/proc/' + pid + '/stat', 'utf8');
            ppid = (stat.split(') ')[1] || '').split(' ')[1] || '';
          } catch (_) { continue; }
          if (parentIsWineserver(ppid)) continue;
        } else {
          let env = '';
          try { env = fs.readFileSync('/proc/' + pid + '/environ', 'utf8'); } catch (_) { continue; }
          if (!env.includes(prefix)) continue;
        }
        try { process.kill(Number(pid), 'SIGKILL'); } catch (_) { /* gone */ }
      }
    }
  } catch (_) { /* nothing left */ }

  // 4. Second pass: the wineserver death is async — winedevice processes
  //    whose parent just died are still orphan checkable after a short wait.
  try { execSync('sleep 2'); } catch (_) { /* ignore */ }
  try {
    let out = '';
    try { out = execSync('pgrep -x winedevice.exe', { encoding: 'utf8' }); } catch (_) { return; }
    for (const pid of out.trim().split('\n')) {
      if (!pid) continue;
      let ppid = '';
      try {
        const stat = fs.readFileSync('/proc/' + pid + '/stat', 'utf8');
        ppid = (stat.split(') ')[1] || '').split(' ')[1] || '';
      } catch (_) { continue; }
      if (!parentIsWineserver(ppid)) {
        try { process.kill(Number(pid), 'SIGKILL'); } catch (_) { /* gone */ }
      }
    }
  } catch (_) { /* nothing left */ }
}

function shutdown() {
  const prefix = process.env.ZCALL_WINEPREFIX;
  if (!prefix) return;
  killWineSession(prefix);
  stopScreenBridge();
}

// ---------------------------------------------------------------------------
// Wayland screen-share bridge
//
// On Wayland, wine's X11 screen capture cannot see the desktop (XWayland is
// isolated). This bridge renders the Wayland screen into a headless Xvfb
// display via the XDG ScreenCast portal. ZaloCall keeps running natively on
// the real display; the streamproxy.so shim (LD_PRELOAD, preloaded via
// ZCALL_PROXY_SO in the patched spawn env) redirects its screen-capture
// reads (XGetImage/XShmGetImage/xcb_get_image on the root) to the bridge
// display, so "share screen" captures the bridged content while the call UI
// stays a normal window. The shim also touches a request file when a
// capture starts while the bridge is down — watchShareRequests() turns that
// into an automatic bridge start, so the compositor's permission dialog
// pops by itself when the user hits "Share screen".
// ---------------------------------------------------------------------------

const BRIDGE_DISPLAY = ':99';
let bridgeProcs = [];
let bridgeGranted = false;
let bridgeGstPid = 0;
let bridgeGen = 0;
let sawBridgeHeartbeat = false;
let cachedBridgeRes = null;

function isWaylandSession() {
  return process.env.XDG_SESSION_TYPE === 'wayland';
}

/**
 * Virtual screen size (all monitors) for the bridge display. The FIRST
 * connected mode (the old grep) only covers monitor #1 — ZaloCall captures
 * the FULL root (it grabs each monitor's half), so a bridge screen sized
 * to one monitor leaves the second half out of bounds: the proxied grab
 * fails and ZaloCall dies mid-share. xrandr --current reports the virtual
 * size directly: "Screen 0: ... current 3840 x 1080, ...".
 */
function bridgeScreenRes() {
  try {
    const out = execSync('xrandr --current 2>/dev/null', { encoding: 'utf8' });
    const m = out.match(/current (\d+) x (\d+)/);
    if (m) return m[1] + 'x' + m[2];
  } catch (e) { /* fall back below */ }
  try {
    const out = execSync('xrandr --query 2>/dev/null | grep -m1 "\\*" | awk \'{print $1}\'', { encoding: 'utf8' });
    if (/^\d+x\d+$/.test(out.trim())) return out.trim();
  } catch (e) { /* default */ }
  return '1920x1080';
}

/**
 * Geometry (x/y/w/h) of the monitor the portal stream belongs to, so the
 * bridge can park the gst window exactly where ZaloCall captures it. The
 * stream node's media.name (e.g. "kwin-screencast-HDMI-A-1") names the
 * monitor; xrandr maps that name to its position in the virtual screen.
 * Unknown/all-screens sources fall back to the full virtual size at 0,0.
 */
function nodeGeometryForNode(nodeId, virtualRes) {
  const fallback = { x: 0, y: 0, w: +virtualRes.split('x')[0], h: +virtualRes.split('x')[1] };
  let mediaName = '';
  if (nodeId) {
    try {
      const out = execSync('pw-cli info ' + nodeId, { encoding: 'utf8', timeout: 5000 });
      const m = out.match(/media\.name\s*=\s*"([^"]+)"/);
      if (m) mediaName = m[1];
    } catch (e) { debugLog('screenbridge: pw-cli info failed: ' + e.message); }
  }
  try {
    const out = execSync('xrandr --query 2>/dev/null', { encoding: 'utf8' });
    for (const line of out.split('\n')) {
      const mm = line.match(/^(\S+) connected.*?(\d+)x(\d+)\+(\d+)\+(\d+)/);
      if (!mm) continue;
      // Boundary match: "HDMI-A-1" must not match inside "HDMI-A-10"
      // (the char after "1" is a digit) but must match after the
      // "screencast-" dash in the media.name.
      const re = new RegExp('(^|[^A-Za-z0-9])' + mm[1].replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '($|[^A-Za-z0-9])');
      if (mediaName && re.test(mediaName)) {
        const geom = { x: +mm[4], y: +mm[5], w: +mm[2], h: +mm[3] };
        debugLog('screenbridge: shared monitor ' + mm[1] + ' at ' + geom.x + ',' + geom.y + ' ' + geom.w + 'x' + geom.h);
        return geom;
      }
    }
  } catch (e) { /* fall through */ }
  return fallback;
}

/**
 * One pass: move/resize every window on the bridge display to the given
 * geometry (the gst window is the only window there).
 */
function placeGstWindow(tools, geom) {
  try {
    const xdo = tools.source === 'bundle' ? tools.tools.xdotool : 'xdotool';
    // jammy's xdotool has NO --display flag (the old command failed
    // silently under 2>/dev/null — the resize never ran); the display is
    // selected via the DISPLAY env instead.
    const env = Object.assign({}, tools.env || process.env, { DISPLAY: BRIDGE_DISPLAY });
    execSync(`${xdo} search "" 2>/dev/null | while read wid; do ${xdo} windowsize $wid ${geom.w} ${geom.h} windowmove $wid ${geom.x} ${geom.y}; done`,
      { stdio: 'ignore', env });
  } catch (e) { debugLog('screenbridge xdotool: ' + e.message); }
}

let lastPlacedGeom = null;

/**
 * Read the capture region the shim reports ("x y w h").
 */
function readCaptureRegion() {
  const f = process.env.ZCALL_PROXY_REGION;
  if (!f) return null;
  try {
    const m = fs.readFileSync(f, 'utf8').trim().match(/^(-?\d+) (-?\d+) (\d+) (\d+)$/);
    if (m) return { x: +m[1], y: +m[2], w: +m[3], h: +m[4] };
  } catch (e) { /* not written yet */ }
  return null;
}

/**
 * Keep the gst window parked on the region ZaloCall actually captures.
 * Runs while the bridge is alive: uses the shim-reported capture region
 * when available (the ground truth — see the grant handler comment),
 * falling back to the shared monitor's geometry until the first capture
 * arrives. Re-checking every 2s also covers the window appearing late and
 * the capture region moving mid-session (user drags the call window to
 * another monitor).
 */
function trackCaptureRegion(tools, fallbackGeom) {
  if (!screenBridgeActive()) return;
  const region = readCaptureRegion();
  const geom = region || fallbackGeom;
  if (!lastPlacedGeom || geom.x !== lastPlacedGeom.x || geom.y !== lastPlacedGeom.y ||
      geom.w !== lastPlacedGeom.w || geom.h !== lastPlacedGeom.h) {
    lastPlacedGeom = geom;
    if (region) debugLog('screenbridge: capture region ' + geom.x + ',' + geom.y + ' ' + geom.w + 'x' + geom.h + ' — placing gst window');
    placeGstWindow(tools, geom);
  }
  // Share-ended detection: the shim touches the heartbeat while captures
  // run. Once it goes stale, kill the pipeline — python wakes from its
  // gst wait, closes the portal session (the compositor's recording
  // indicator goes away) and exits, which triggers the bridge cleanup.
  // A MISSING file means "no capture has happened yet", not "expired":
  // the stale clock only starts after the first heartbeat is observed.
  if (bridgeGranted && process.env.ZCALL_PROXY_HEARTBEAT) {
    let hbAge = 0;
    try {
      hbAge = (Date.now() - fs.statSync(process.env.ZCALL_PROXY_HEARTBEAT).mtimeMs) / 1000;
      sawBridgeHeartbeat = true;
    } catch (e) { /* not written yet — skip the stale check this tick */ }
    if (sawBridgeHeartbeat && hbAge > 8) {
      debugLog('screenbridge: capture heartbeat stale — share ended, stopping pipeline');
      bridgeGranted = false;
      if (bridgeGstPid) {
        try { process.kill(bridgeGstPid, 'SIGTERM'); } catch (e) { /* gone */ }
        // python normally exits within a second of gst dying; force the
        // full teardown if it doesn't. Generation guard: a bridge started
        // after this timer was armed must not be killed by it.
        const gen = bridgeGen;
        setTimeout(() => { if (gen === bridgeGen && screenBridgeActive() && !bridgeGranted) stopScreenBridge(); }, 5000);
      } else {
        stopScreenBridge();
      }
      return;
    }
  }
  setTimeout(() => trackCaptureRegion(tools, fallbackGeom), 2000);
}

function screenBridgeActive() {
  return bridgeProcs.some((p) => p && p.exitCode === null && !p.killed);
}

// Per-binary location inside the gst-runtime tree. python3 is deliberately
// absent — screenbridge.py runs on the HOST python3 (see bridgeTools).
const BRIDGE_BIN_RELS = {
  xvfb: 'usr/bin/Xvfb',
  xdotool: 'usr/bin/xdotool',
  'gst-launch-1.0': 'usr/bin/gst-launch-1.0',
  'gst-inspect-1.0': 'usr/bin/gst-inspect-1.0',
};

/**
 * Resolve the bridge executables + their environment, bundle first.
 *
 * source 'bundle': gst-runtime is present AND carries the whole bridge stack
 *   (all-or-nothing — a partial tree, e.g. an older Full build that predates
 *   this feature, falls through to 'system' exactly like non-Full). env holds
 *   the per-spawn environment; callers must pass it to their own
 *   spawn/execSync — this function NEVER mutates process.env.
 * source 'system': today's behavior — bare command names resolved from PATH
 *   with the inherited environment (env === null).
 */
function bridgeTools() {
  const rt = findBundledGstRuntime();
  if (rt) {
    const tools = {};
    let complete = true;
    for (const [name, rel] of Object.entries(BRIDGE_BIN_RELS)) {
      const p = path.join(rt, rel);
      if (!fs.existsSync(p)) { complete = false; break; }
      tools[name] = p;
    }
    if (complete) {
      const libDir = path.join(rt, 'usr', 'lib', 'x86_64-linux-gnu');
      const env = Object.assign({}, process.env, {
        // Prepend, never replace: screenbridge.py launches `gst-launch-1.0`
        // by bare name; the bundle must win that lookup. python3 itself is
        // the HOST interpreter (bundle ships none), so it resolves past the
        // bundle bin dir to /usr/bin.
        PATH: path.join(rt, 'usr', 'bin') + ':' + (process.env.PATH || ''),
        // The gst overrides travel under ZCALL_GST_BUNDLE_* — NOT directly —
        // so the HOST python3 process never loads the bundle's jammy
        // glib/libdbus (mixed-version dlopen = segfault risk). screenbridge.py
        // composes the gst child's env from these when it launches the
        // pipeline.
        ZCALL_GST_BUNDLE_LIBDIR: libDir,
        ZCALL_GST_BUNDLE_PLUGIN_PATH: path.join(libDir, 'gstreamer-1.0'),
        ZCALL_GST_BUNDLE_PLUGIN_SYSTEM_PATH: path.join(rt, 'system'),
        // Pin the scanner to the bundle's own (compiled-in /usr/lib lookups
        // would otherwise hit a host gstreamer of a different version).
        ZCALL_GST_BUNDLE_PLUGIN_SCANNER: path.join(libDir, 'gstreamer1.0', 'gstreamer-1.0', 'gst-plugin-scanner'),
        // No SPA/PIPEWIRE overrides: the bundle deliberately ships NO
        // pipewire client (PW_STRIP in build.js) — the bridge uses the
        // HOST pipewire stack, which always matches the session's daemon
        // (a bundled jammy 0.3.48 client segfaults against 1.x daemons).
      });
      // Own registry file, separate from the wine call's: identical content
      // (same bundle), but no concurrent-scan write contention when a share
      // starts mid-call. Unset -> gst falls back to its default writable
      // cache; on Full it is always set by launch() first.
      if (process.env.ZCALL_GST_REGISTRY_BRIDGE) {
        env.ZCALL_GST_BUNDLE_REGISTRY = process.env.ZCALL_GST_REGISTRY_BRIDGE;
      }
      // The bundled gstpipewiresrc (jammy 0.3.48) also cannot take buffers
      // from modern daemons ("error alloc buffers: Invalid argument" on
      // KWin 6 — no dma-buf modifier support), and a host-built plugin
      // cannot load into the 1.20 core. When the HOST has a complete
      // bridge gst stack, run the pipeline with the host gst-launch
      // instead — host plugins always match the host daemon. The bundle
      // stays the fallback (screenbridge.py strips the bundle overrides
      // from the child env when ZCALL_GST_HOST_LAUNCH is set).
      let hostLaunch = null;
      try {
        const hostInspect = execSync('which gst-inspect-1.0', { encoding: 'utf8' }).trim();
        if (hostInspect) {
          for (const plugin of ['pipewiresrc', 'ximagesink', 'videoconvert']) {
            execSync(hostInspect + ' ' + plugin, { stdio: 'ignore' });
          }
          hostLaunch = execSync('which gst-launch-1.0', { encoding: 'utf8' }).trim();
        }
      } catch (e) { /* host stack incomplete — bundle fallback */ }
      if (hostLaunch) env.ZCALL_GST_HOST_LAUNCH = hostLaunch;
      return { source: 'bundle', tools, env };
    }
    debugLog('screenbridge: gst-runtime present but bridge stack incomplete — using system tools');
  }
  return { source: 'system', tools: null, env: null };
}

function startScreenBridge() {
  getElectronModules();
  if (screenBridgeActive()) {
    if (dialogModule) {
      dialogModule.showMessageBox({
        type: 'info',
        title: 'Zalo — Chia sẻ màn hình',
        message: 'Bridge đang hoạt động',
        detail: 'Màn hình ảo ' + BRIDGE_DISPLAY + ' đã sẵn sàng. Cuộc gọi hoạt động hoàn toàn như bình thường — khi bấm Share screen, hình chia sẻ sẽ được lấy từ màn hình thật qua bridge.'
      });
    }
    return true;
  }

  const pyPath = zcallBridgePath('screenbridge.py');
  if (!fs.existsSync(pyPath)) {
    if (dialogModule) {
      dialogModule.showMessageBox({
        type: 'error', title: 'Zalo — Chia sẻ màn hình',
        message: 'Thiếu thành phần bridge',
        detail: 'Không tìm thấy screenbridge.py tại:\n' + pyPath
      });
    }
    return false;
  }

  // streamproxy.so (LD_PRELOAD shim) redirects ZaloCall's screen-capture
  // reads from the real display root to the bridge display, so the call UI
  // stays 100% native while "share screen" captures the bridged stream.
  // It is compiled by scripts/setup-zcall-bridge.js.
  const proxySoPath = zcallBridgePath('streamproxy.so');
  if (!fs.existsSync(proxySoPath)) {
    if (dialogModule) {
      dialogModule.showMessageBox({
        type: 'error', title: 'Zalo — Chia sẻ màn hình',
        message: 'Thiếu thành phần streamproxy',
        detail: 'Không tìm thấy streamproxy.so tại:\n' + proxySoPath +
          '\n\nChạy "node scripts/setup-zcall-bridge.js" để build lại.'
      });
    }
    return false;
  }

  // Resolve the real screen resolution for the Xvfb screen. Cached from app
  // launch — the synchronous xrandr call would add ~200ms of latency right
  // when the user is waiting for the permission dialog.
  let res = cachedBridgeRes;
  if (!res) {
    res = bridgeScreenRes();
    cachedBridgeRes = res;
  }
  const [w, h] = res.split('x');

  // Missing system deps fail silently otherwise (gst then reports
  // "Could not open display" and the shim cannot reach :99).
  const tools = bridgeTools();
  const missingDeps = [];
  if (tools.source === 'system') {
    // Bundle mode probes nothing — the all-or-nothing resolver already
    // guaranteed every binary exists.
    for (const dep of ['Xvfb', 'python3', 'xdotool', 'gst-launch-1.0']) {
      try {
        const r = execSync('which ' + dep, { encoding: 'utf8' });
        if (!r.trim()) missingDeps.push(dep);
      } catch (e) { missingDeps.push(dep); }
    }
  }
  // Element-level proof runs in BOTH modes (bundle: bundled gst-inspect with
  // the bundle env; a plugin that fails to register must be reported, not
  // assumed away).
  for (const plugin of ['pipewiresrc', 'ximagesink']) {
    try {
      const inspectBin = tools.source === 'bundle' ? tools.tools['gst-inspect-1.0'] : 'gst-inspect-1.0';
      execSync(inspectBin + ' ' + plugin, Object.assign({ stdio: 'ignore' },
        tools.source === 'bundle' ? { env: tools.env } : {}));
    } catch (e) { missingDeps.push('gst plugin ' + plugin); }
  }
  if (missingDeps.length) {
    if (dialogModule) {
      dialogModule.showMessageBox({
        type: 'error', title: 'Zalo — Chia sẻ màn hình',
        message: 'Thiếu thành phần hệ thống: ' + missingDeps.join(', '),
        detail: 'Cài để bật share screen:\n\n' +
          '  Fedora:  sudo dnf install xorg-x11-server-Xvfb xdotool python3-dbus gstreamer1-plugins-base gstreamer1-plugins-bad-free\n' +
          '  Ubuntu:  sudo apt install xvfb xdotool python3-dbus gstreamer1.0-plugins-base gstreamer1.0-plugins-bad\n' +
          '  Arch:    sudo pacman -S xorg-server-xvfb xdotool python-dbus gst-plugins-base gst-plugins-bad'
      });
    }
    debugLog('screenbridge: missing system deps: ' + missingDeps.join(', '));
    return false;
  }

  stopScreenBridge();
  bridgeGranted = false;
  bridgeGstPid = 0;
  bridgeGen++;
  sawBridgeHeartbeat = false;
  // Stale region/heartbeat from a previous share must not steer the first
  // placement (or fake a live share before the first capture).
  try { fs.rmSync(process.env.ZCALL_PROXY_REGION, { force: true }); } catch (e) { /* none */ }
  try { fs.rmSync(process.env.ZCALL_PROXY_HEARTBEAT, { force: true }); } catch (e) { /* none */ }
  lastPlacedGeom = null;
  try {
    // Headless Xvfb holds the bridged stream; ZaloCall keeps running on the
    // real display (native UI) and the streamproxy shim redirects its
    // screen-capture reads to this display.
    const xvfbBin = tools.source === 'bundle' ? tools.tools.xvfb : 'Xvfb';
    // Bundle mode disables GLX: the stripped bundle has no libGL and the
    // extension is never needed (ximagesink renders via XPutImage).
    const xvfbArgs = [BRIDGE_DISPLAY, '-screen', '0', w + 'x' + h + 'x24'];
    if (tools.source === 'bundle') xvfbArgs.push('-extension', 'GLX');
    const xvfb = spawn(xvfbBin, xvfbArgs, Object.assign({ stdio: ['ignore', 'ignore', 'pipe'] },
      tools.source === 'bundle' ? { env: tools.env } : {}));
    xvfb.stderr.on('data', (d) => debugLog('screenbridge xvfb: ' + String(d).trim().slice(0, 200)));
    bridgeProcs.push(xvfb);
    // A wedged .X99-lock or a broken host GL would kill Xvfb silently —
    // python then fails on the display and the share stays black with no
    // explanation. Detect the early death and log it.
    setTimeout(() => {
      if (screenBridgeActive() && xvfb.exitCode !== null) {
        debugLog('screenbridge: Xvfb died right after start (exit ' + xvfb.exitCode + ') — tearing down');
        stopScreenBridge();
      }
    }, 1500);
    // screenbridge.py inherits os.environ from this spawn, so its nested
    // bare `gst-launch-1.0` resolves through the prepended PATH to the
    // bundled binary and picks up the bundle GST_* vars. The host
    // XDG_RUNTIME_DIR / DBUS_SESSION_BUS_ADDRESS pass through untouched —
    // they ARE the session pipewire socket + portal bus the flow needs.
    // detached: python becomes a process-group leader so stopScreenBridge
    // can kill gst (its child) with the group on forced teardown.
    // python3 is ALWAYS the host interpreter (bridgeTools resolves it from
    // PATH) — only the gst env comes from the bundle when present.
    const py = spawn('python3',
      [pyPath, BRIDGE_DISPLAY], Object.assign({ stdio: ['ignore', 'ignore', 'pipe'], detached: true },
        tools.source === 'bundle' ? { env: tools.env } : {}));
    py.isGroupLeader = true;
    py.stderr.on('data', (d) => {
      const s = String(d).trim().slice(0, 300);
      debugLog('screenbridge: ' + s);
      const gm = s.match(/gst pid (\d+)/);
      if (gm) bridgeGstPid = parseInt(gm[1], 10);
      // The user granted the portal and gst is rendering into :99 — from
      // now on the shim's proxied grabs return the stream. (The helper is
      // never restarted: the shim is preloaded from app launch, and its
      // fall-through handles the bridge-down state.)
      if (s.indexOf('got pipewire node') !== -1) {
        bridgeGranted = true;
        debugLog('screenbridge: granted — stream ready on ' + BRIDGE_DISPLAY);
        // Place the gst window at the geometry of the monitor being shared,
        // then FOLLOW THE REAL CAPTURE REGION: ZaloCall captures at the
        // position of the monitor it considers primary — which does NOT
        // change with the portal's source selection. The shim reports the
        // live region via ZCALL_PROXY_REGION; parking the window there
        // makes whatever monitor the user shares land exactly where the
        // captures read (the shared-monitor geometry is only the initial
        // guess before the first capture arrives).
        const m = s.match(/node (\d+)/);
        const nodeId = m ? parseInt(m[1], 10) : 0;
        const geom = nodeGeometryForNode(nodeId, res);
        trackCaptureRegion(tools, geom);
      }
    });
    py.on('exit', (code) => {
      debugLog('screenbridge: python exited code=' + code + ' granted=' + bridgeGranted);
      stopScreenBridge();
    });
    bridgeProcs.push(py);
  } catch (e) {
    debugLog('screenbridge start failed: ' + e.message);
    return false;
  }

  debugLog('screenbridge started on ' + BRIDGE_DISPLAY + ' res=' + res);
  return true;
}

function stopScreenBridge() {
  for (const p of bridgeProcs) {
    try {
      if (p && !p.killed) {
        // python is a detached group leader — kill the whole group so its
        // gst child never orphans and keeps the portal session alive.
        // Xvfb is NOT a group leader: kill(-pid) would target a
        // nonexistent group (ESRCH on every teardown) and, worse, a
        // recycled pid could collide with an unrelated orphaned group —
        // kill it directly.
        if (p.isGroupLeader) process.kill(-p.pid, 'SIGTERM');
        else p.kill('SIGTERM');
      }
    } catch (e) {
      try { if (p && !p.killed) p.kill(); } catch (e2) { /* gone */ }
    }
  }
  bridgeGstPid = 0;
  bridgeProcs = [];
}

module.exports = {
  launch,
  openSetupDialog,
  shutdown,
  // internal (testability)
  _installDownloadedWine: installDownloadedWine,
  _installDownloadedGst: installDownloadedGst,
};
