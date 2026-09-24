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
 *      (~54MB) into <userData>/zcall-wine-runtime/ with a progress window —
 *      no root needed, works on any distro.
 *   3. Ensures the wine prefix exists (wineboot), exports
 *      ZCALL_WINE / ZCALL_WINEPREFIX / WINEDEBUG into process.env.
 *   4. On quit, kills the whole wine session of our prefix; on launch,
 *      sweeps stale wine processes of unclean previous exits.
 *   5. Tray menu "Cài đặt gọi điện…" opens a settings window: browse/clear/
 *      remove wine.
 *
 * Configuration (env vars):
 *   ZCALL_WINE                 wine binary (highest priority)
 *   ZCALL_WINEPREFIX           wine prefix (default: <userData>/zcall-wine)
 *   ZCALL_DISABLE              set to anything to skip entirely
 *   ZCALL_AUTO_SETUP           '1' to download wine silently (no dialog)
 *   ZCALL_WINE_DOWNLOAD_URL    override the portable wine download URL
 */

'use strict';

const { spawn, spawnSync, execSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const https = require('https');

// Recommended build: 11.14 (96MB / ~850MB). Video calls verified working on
// it; wine 8.6 is lighter (54MB) but its msvcp140/ucrtbase lack
// _Throw_C_error, which crashes ZaloCall when the video pipeline hits an
// error (e.g. codec/format negotiation).
const WINE_DOWNLOAD_URL =
  'https://github.com/Kron4ek/Wine-Builds/releases/download/11.14/wine-11.14-amd64.tar.xz';
const RUNTIME_DIRNAME = 'zcall-wine-runtime';
const CONFIG_FILENAME = 'zcall-config.json';

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
  return fs.existsSync(p) ? p : null;
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
  // Use a dedicated throwaway prefix: validating against the real prefix can
  // trigger slow version upgrade/downgrade passes (10-30s+) or corrupt state,
  // and a cold first run needs a generous timeout.
  const valPrefix = prefix + '-validate';
  try {
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

  fs.mkdirSync(runtimeDir, { recursive: true });
  execSync(`tar -xf "${tarball}" -C "${runtimeDir}" --strip-components=1`, { stdio: 'pipe' });
  fs.unlinkSync(tarball);

  const wine = path.join(runtimeDir, 'bin', 'wine');
  if (!fs.existsSync(wine)) throw new Error('wine binary not found after extract');
  return wine;
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
    height: 280,
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
    ? 'Wine trên máy bạn không tương thích với tính năng gọi (không chạy được ứng dụng 32-bit). Tải bản Wine tương thích?'
    : 'Tính năng gọi điện cần Wine. Tải và bật ngay bây giờ?';
  const html = `<!DOCTYPE html><html><head><meta charset="utf-8"><style>
    body{font-family:sans-serif;background:#1f1f1f;color:#eee;margin:0;padding:20px 24px;-webkit-app-region:drag}
    h3{margin:0 0 8px;font-size:16px}
    p{font-size:13px;color:#ccc;margin:0 0 10px;line-height:1.45}
    #url{font-size:11px;color:#6ab;text-overflow:ellipsis;overflow:hidden;white-space:nowrap;cursor:pointer;margin-bottom:14px;-webkit-app-region:no-drag}
    label{font-size:13px;color:#ccc;display:block;margin-bottom:16px;-webkit-app-region:no-drag}
    .row{display:flex;justify-content:flex-end;gap:10px;-webkit-app-region:no-drag}
    button{font-size:13px;padding:8px 18px;border-radius:6px;border:none;cursor:pointer}
    #yes{background:#0a6e3c;color:#fff}
    #no{background:#3a3a3a;color:#eee}
  </style></head><body>
    <h3>Zalo — Tính năng gọi điện</h3>
    <p>${headLine}<br>
       Sẽ tải ~54MB về lưu trong dữ liệu của Zalo — không cần quyền quản trị,
       không ảnh hưởng hệ thống.</p>
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
    <p>Đang tải Wine (~54MB), vui lòng chờ…</p>
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
      process.env.ZCALL_WINE = result.pickedWine;
      process.env.ZCALL_WINEPREFIX = prefix;
      if (!process.env.WINEDEBUG) process.env.WINEDEBUG = '-all';
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
    const wine = await installDownloadedWine(userDataDir, (got, total) => {
      const now = Date.now();
      if (now - lastUpdate < 500) return; // throttle IPC updates
      lastUpdate = now;
      const pct = Math.round((got / total) * 100);
      progress.set(pct, `Đang tải… ${Math.round(got / 1024 / 1024)}MB / ${Math.round(total / 1024 / 1024)}MB (${pct}%)`);
    });
    debugLog('install: downloaded, extracting...');
    progress.set(100, 'Đang giải nén và chuẩn bị…');

    // First prefix init (~10-30s, done once)
    const prefix = process.env.ZCALL_WINEPREFIX || path.join(userDataDir, 'zcall-wine');
    spawnSync(wine, ['wineboot', '-u'], {
      env: Object.assign({}, process.env, { WINEPREFIX: prefix, WINEDEBUG: '-all' }),
      stdio: 'ignore',
      timeout: 180000
    });

    // Verify the freshly downloaded wine actually works on this machine —
    // classic wine builds need 32-bit libraries that some systems do not
    // have.
    if (!validateWine(wine, prefix)) {
      const hint = getI386InstallHint();
      throw new Error(
        'Wine không chạy được trên máy này — cần thư viện 32-bit.\n\n' +
        hint.title + '\n' + hint.command
      );
    }

    progress.close();
    debugLog('install: SUCCESS wine=' + wine + ' prefix=' + prefix);

    process.env.ZCALL_WINE = wine;
    process.env.ZCALL_WINEPREFIX = prefix;
    if (!process.env.WINEDEBUG) process.env.WINEDEBUG = '-all';
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

function launch({ userDataDir }) {
  if (process.env.ZCALL_DISABLE) return false;

  const prefix = process.env.ZCALL_WINEPREFIX || path.join(userDataDir, 'zcall-wine');

  // Clean stale wine processes from unclean previous exits
  sweepStaleProcesses(prefix);

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
    if (validateWine(candidate, prefix)) {
      wine = candidate;
      break;
    }
    console.error('[zcall-bridge] wine cannot run 32-bit apps, skipping:', candidate);
    failedWine = candidate;
  }

  if (!wine) {
    const cfg = readConfig(userDataDir);

    // Downloaded runtime exists but cannot run (machine lacks 32-bit
    // libraries): re-downloading would loop forever — guide the user
    // instead, once, without nagging every launch.
    if (downloadedWine && failedWine === downloadedWine) {
      console.error('[zcall-bridge] downloaded wine broken (missing 32-bit libs?)');
      if (cfg.wineSetup !== 'broken' && process.env.ZCALL_AUTO_SETUP !== '1') {
        writeConfig(userDataDir, { wineSetup: 'broken' });
        showBrokenWineDialog(downloadedWine);
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

  // Export for the patched main-dist spawn code
  process.env.ZCALL_WINE = wine;
  process.env.ZCALL_WINEPREFIX = prefix;
  if (!process.env.WINEDEBUG) process.env.WINEDEBUG = '-all';

  const runtimeDir = process.env.XDG_RUNTIME_DIR || `/run/user/${process.getuid ? process.getuid() : 1000}`;
  if (!process.env.PULSE_SERVER) {
    process.env.PULSE_SERVER = `unix:${runtimeDir}/pulse/native`;
  }

  // Streamproxy: the capture shim is preloaded into the helper at ALL times.
  // It is inert while the bridge display is down (captures fall through to
  // the real display) and it signals a share request via a file, which the
  // watcher below turns into an automatic bridge start — no tray click
  // needed when the user hits "Share screen".
  const proxySo = zcallBridgePath('streamproxy.so');
  if (fs.existsSync(proxySo)) {
    process.env.ZCALL_PROXY_SO = proxySo;
    process.env.ZCALL_PROXY_LOG = path.join(os.homedir(), '.config', 'ZaloData', 'zcall-proxy.log');
    process.env.ZCALL_PROXY_REQUEST = path.join(os.homedir(), '.config', 'ZaloData', 'zcall-share.request');
    // Warm the resolution cache now so the first share-screen request does
    // not pay the synchronous xrandr call while the user waits.
    try {
      const out = execSync('xrandr --query 2>/dev/null | grep -m1 "\\*" | awk \'{print $1}\'', { encoding: 'utf8' });
      if (/^\d+x\d+$/.test(out.trim())) cachedBridgeRes = out.trim();
    } catch (e) { /* default */ }
    watchShareRequests();
  }

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
      command: 'sudo dnf install -y glibc.i686 libX11.i686 libXext.i686 freetype.i686 mesa-libGL.i686 pulseaudio-libs.i686 alsa-lib.i686 libv4l.i686 zlib-ng-compat.i686 gstreamer1.i686 gstreamer1-plugins-base.i686 gstreamer1-plugins-good.i686 gstreamer1-plugins-bad-free.i686\n\n(GStreamer 32-bit cần cho video call; gstreamer1-plugin-libav cần RPM Fusion)\n\nHoặc cài wine hệ thống (tự kéo đủ thư viện):\nsudo dnf install wine'
    };
  }
  if (idLike.includes('arch')) {
    return {
      title: 'Cài thư viện 32-bit (Arch):',
      command: 'sudo pacman -S --needed lib32-glibc lib32-libx11 lib32-libxext lib32-freetype2 lib32-mesa lib32-libpulse lib32-alsa-lib lib32-libv4l lib32-zlib lib32-gstreamer lib32-gst-plugins-base lib32-gst-plugins-good lib32-gst-plugins-bad lib32-gst-libav\n\n(GStreamer 32-bit cần cho video call)\n\nHoặc cài wine hệ thống:\nsudo pacman -S wine'
    };
  }
  // default: Debian/Ubuntu family
  return {
    title: 'Cài thư viện 32-bit (Ubuntu/Debian):',
    command: 'sudo dpkg --add-architecture i386 && sudo apt update\nsudo apt install -y libc6:i386 libx11-6:i386 libfreetype6:i386 libgl1:i386 libpulse0:i386 libasound2:i386 libv4l-0:i386 zlib1g:i386 libgstreamer1.0-0:i386 libgstreamer-plugins-base1.0-0:i386 gstreamer1.0-plugins-good:i386 gstreamer1.0-plugins-bad:i386 gstreamer1.0-libav:i386\n\n(GStreamer 32-bit cần cho video call)\n\nHoặc cài wine hệ thống (tự kéo đủ thư viện):\nsudo apt install wine'
  };
}

function showBrokenWineDialog(winePath) {
  getElectronModules();
  if (!dialogModule) return;
  const hint = getI386InstallHint();
  const parent = BrowserWindowModule.getFocusedWindow() || BrowserWindowModule.getAllWindows()[0];
  dialogModule.showMessageBox(parent, {
    type: 'warning',
    title: 'Zalo — Tính năng gọi điện',
    message: 'Wine không chạy được trên máy này',
    detail: 'Wine cần các thư viện 32-bit mà máy bạn chưa có.\n\n' +
      hint.title + '\n' + hint.command +
      '\n\nSau khi cài xong, khởi động lại Zalo là gọi được.\n\n' +
      'Wine đã tải: ' + winePath
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
    <button id="download">Tải wine về (~54MB)</button>
    <button id="clear">Bỏ lựa chọn wine đã lưu</button>
    <button id="remove">Xóa wine đã tải về khỏi máy</button>
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
    </script>
  </body></html>`;
  win.loadURL('data:text/html;charset=utf-8,' + encodeURIComponent(html));

  const currentWine = () => process.env.ZCALL_WINE || findWine() || findDownloadedWine(userDataDir);
  const pushStatus = () => {
    const w = currentWine();
    const cfg = readConfig(userDataDir);
    let text = w
      ? 'Wine đang dùng: ' + w + (cfg.winePath ? '\n(Lựa chọn đã lưu: ' + cfg.winePath + ')' : '')
      : 'Chưa có wine — tính năng gọi chưa hoạt động.';
    try { win.webContents.send('zcall-cfg-status', text); } catch (e) { /* closed */ }
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
          process.env.ZCALL_WINE = chosen;
          process.env.ZCALL_WINEPREFIX = prefix;
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
      process.env.ZCALL_WINE = typed;
      process.env.ZCALL_WINEPREFIX = prefix;
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
      if (!fs.existsSync(runtime)) {
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
        message: 'Xóa wine đã tải về?',
        detail: 'Sẽ xóa thư mục ' + runtime + '\nBạn có thể tải lại bất cứ lúc nào.'
      }).then(({ response }) => {
        if (response === 0) {
          try { fs.rmSync(runtime, { recursive: true, force: true }); } catch (e) {}
          const cfg = readConfig(userDataDir);
          delete cfg.winePath;
          writeConfig(userDataDir, cfg);
          pushStatus();
          dialogModule.showMessageBox(win, {
            type: 'info', title: 'Zalo — Tính năng gọi điện',
            message: 'Đã xóa wine đã tải về',
            detail: 'Thư mục đã bị xóa. Lần mở app sau sẽ hỏi lại hoặc tự dò wine hệ thống.'
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
let cachedBridgeRes = null;

function isWaylandSession() {
  return process.env.XDG_SESSION_TYPE === 'wayland';
}

function screenBridgeActive() {
  return bridgeProcs.some((p) => p && p.exitCode === null && !p.killed);
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
    res = '1920x1080';
    try {
      const out = execSync('xrandr --query 2>/dev/null | grep -m1 "\\*" | awk \'{print $1}\'', { encoding: 'utf8' });
      if (/^\d+x\d+$/.test(out.trim())) res = out.trim();
    } catch (e) { /* default */ }
    cachedBridgeRes = res;
  }
  const [w, h] = res.split('x');

  // Missing system deps fail silently otherwise (gst then reports
  // "Could not open display" and the shim cannot reach :99).
  const missingDeps = [];
  for (const dep of ['Xvfb', 'python3', 'xdotool', 'gst-launch-1.0']) {
    try {
      const r = execSync('which ' + dep, { encoding: 'utf8' });
      if (!r.trim()) missingDeps.push(dep);
    } catch (e) { missingDeps.push(dep); }
  }
  for (const plugin of ['pipewiresrc', 'ximagesink']) {
    try {
      execSync('gst-inspect-1.0 ' + plugin, { stdio: 'ignore' });
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
  try {
    // Headless Xvfb holds the bridged stream; ZaloCall keeps running on the
    // real display (native UI) and the streamproxy shim redirects its
    // screen-capture reads to this display.
    const xvfb = spawn('Xvfb', [BRIDGE_DISPLAY, '-screen', '0', w + 'x' + h + 'x24'], { stdio: ['ignore', 'ignore', 'pipe'] });
    xvfb.stderr.on('data', (d) => debugLog('screenbridge xvfb: ' + String(d).trim().slice(0, 200)));
    bridgeProcs.push(xvfb);
    const py = spawn('python3', [pyPath, BRIDGE_DISPLAY], { stdio: ['ignore', 'ignore', 'pipe'] });
    py.stderr.on('data', (d) => {
      const s = String(d).trim().slice(0, 300);
      debugLog('screenbridge: ' + s);
      // The user granted the portal and gst is rendering into :99 — from
      // now on the shim's proxied grabs return the stream. (The helper is
      // never restarted: the shim is preloaded from app launch, and its
      // fall-through handles the bridge-down state.)
      if (s.indexOf('got pipewire node') !== -1) {
        bridgeGranted = true;
        debugLog('screenbridge: granted — stream ready on ' + BRIDGE_DISPLAY);
      }
    });
    py.on('exit', (code) => {
      debugLog('screenbridge: python exited code=' + code + ' granted=' + bridgeGranted);
      stopScreenBridge();
    });
    bridgeProcs.push(py);
    // Let gst create its window, then stretch every window on the Xvfb
    // display over the whole screen (the gst window title is
    // "gst-launch-1.0", so match anything).
    setTimeout(() => {
      try {
        execSync(`xdotool search --display ${BRIDGE_DISPLAY} "" 2>/dev/null | while read wid; do xdotool windowsize $wid ${w} ${h} windowmove $wid 0 0; done`, { stdio: 'ignore' });
      } catch (e) { debugLog('screenbridge xdotool: ' + e.message); }
    }, 5000);
  } catch (e) {
    debugLog('screenbridge start failed: ' + e.message);
    return false;
  }

  debugLog('screenbridge started on ' + BRIDGE_DISPLAY + ' res=' + res);
  return true;
}

function stopScreenBridge() {
  for (const p of bridgeProcs) {
    try { if (p && !p.killed) p.kill(); } catch (e) { /* gone */ }
  }
  bridgeProcs = [];
}

module.exports = {
  launch,
  openSetupDialog,
  shutdown,
  // internal (testability)
  _installDownloadedWine: installDownloadedWine,
};
