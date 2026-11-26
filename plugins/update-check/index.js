'use strict';

// Tells the user when a newer AppImage is published on the release repository
// this build came from and, after they confirm, downloads it, checks its
// SHA-256 against the release metadata and swaps it in place of the running
// AppImage (the previous file is kept as <name>.AppImage.old).

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

const ENV_DISABLE = 'ZALO_UPDATE_CHECK';
const FIRST_CHECK_DELAY_MS = 30 * 1000;
const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1000;
const REQUEST_TIMEOUT_MS = 20 * 1000;
const DOWNLOAD_IDLE_TIMEOUT_MS = 60 * 1000;
// If Zalo is still running this long after app.quit(), the quit was cancelled.
const QUIT_GUARD_MS = 30 * 1000;
const SETTINGS_KEY = 'updateCheckNotified';
const HIDDEN_FLAGS = new Set(['--hidden', '--start-hidden']);
// Hidden windows Zalo uses as background processes (see main.js).
const BACKGROUND_WINDOW_TITLES = new Set(['Shared Worker', 'SQLite']);

function readBuildInfo(appDir) {
  try {
    const info = JSON.parse(fs.readFileSync(path.join(appDir, 'pc-dist', 'build-info.json'), 'utf8'));
    if (!info || typeof info !== 'object') return null;
    if (typeof info.version !== 'string' || typeof info.commit !== 'string' || !info.commit) return null;
    if (typeof info.releaseRepo !== 'string' || !/^[\w.-]+\/[\w.-]+$/.test(info.releaseRepo)) return null;
    return info;
  } catch (_) {
    return null;
  }
}

function archSuffix(arch = process.arch) {
  if (arch === 'x64') return 'x86_64';
  if (arch === 'arm64') return 'aarch64';
  return null;
}

// Artifact names come from scripts/build.js:
//   Zalo-<ver>-Original-<commit>-<arch>.AppImage
//   Zalo-<ver>+ZaDark-<zadark>-<commit>-<arch>.AppImage
function assetPattern(variant, arch) {
  const middle = variant === 'zadark' ? '\\+ZaDark-[^-]+' : '-Original';
  return new RegExp(`^Zalo-(\\d+(?:\\.\\d+)*)${middle}-([0-9a-f]{7,40})-${arch}\\.AppImage$`);
}

function compareVersions(a, b) {
  const pa = String(a).split('.').map(Number);
  const pb = String(b).split('.').map(Number);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa[i] || 0) - (pb[i] || 0);
    if (d !== 0) return d > 0 ? 1 : -1;
  }
  return 0;
}

function parseDigest(digest) {
  const match = typeof digest === 'string' && digest.match(/^sha256:([0-9a-f]{64})$/i);
  return match ? match[1].toLowerCase() : null;
}

/**
 * Picks the asset matching this build's variant and architecture from a
 * GitHub release and decides whether it is newer than the running build.
 * Returns null when there is nothing to report.
 */
function findUpdate(release, buildInfo, arch = archSuffix()) {
  if (!release || release.draft || release.prerelease || !Array.isArray(release.assets) || !arch) return null;
  const variant = buildInfo.zadarkVersion ? 'zadark' : 'original';
  const pattern = assetPattern(variant, arch);
  for (const asset of release.assets) {
    const match = asset && typeof asset.name === 'string' && asset.name.match(pattern);
    if (!match) continue;
    const [, version, commit] = match;
    if (commit === buildInfo.commit) return null;
    const order = compareVersions(version, buildInfo.version);
    if (order < 0) return null;
    if (order === 0) {
      // Same Zalo version, different build: only newer if uploaded after we
      // were built. The release workflow reuses the tag and swaps the assets,
      // so the release's published_at stays at the first build; the asset's
      // own timestamp is the one that moves.
      const published = Date.parse(asset.updated_at || asset.created_at || release.published_at);
      const built = Date.parse(buildInfo.buildDate);
      if (!(published > built)) return null;
    }
    return {
      version,
      commit,
      assetName: asset.name,
      downloadUrl: typeof asset.browser_download_url === 'string' ? asset.browser_download_url : null,
      size: Number.isFinite(asset.size) ? asset.size : null,
      sha256: parseDigest(asset.digest),
      url: typeof release.html_url === 'string' ? release.html_url : `https://github.com/${buildInfo.releaseRepo}/releases/latest`
    };
  }
  return null;
}

function fetchLatestRelease(repo, net) {
  return new Promise((resolve, reject) => {
    const request = net.request({
      method: 'GET',
      url: `https://api.github.com/repos/${repo}/releases/latest`
    });
    request.setHeader('Accept', 'application/vnd.github+json');
    request.setHeader('User-Agent', 'zalo-for-linux-update-check');
    const timer = setTimeout(() => {
      request.abort();
      reject(new Error('timeout'));
    }, REQUEST_TIMEOUT_MS);
    request.on('response', (response) => {
      const chunks = [];
      response.on('data', (chunk) => chunks.push(Buffer.from(chunk)));
      response.on('end', () => {
        clearTimeout(timer);
        if (response.statusCode !== 200) {
          reject(new Error(`HTTP ${response.statusCode}`));
          return;
        }
        try {
          resolve(JSON.parse(Buffer.concat(chunks).toString('utf8')));
        } catch (error) {
          reject(error);
        }
      });
      response.on('error', (error) => {
        clearTimeout(timer);
        reject(error);
      });
    });
    request.on('error', (error) => {
      clearTimeout(timer);
      reject(error);
    });
    request.end();
  });
}

// Streams url into dest with Electron's net module (follows the GitHub
// redirect to the object store and honours the system proxy).
function netDownload(net, url, dest, onProgress, signal) {
  return new Promise((resolve, reject) => {
    if (signal && signal.aborted) {
      reject(cancelledError());
      return;
    }
    const out = fs.createWriteStream(dest, { mode: 0o600 });
    const request = net.request({ method: 'GET', url, redirect: 'follow' });
    request.setHeader('User-Agent', 'zalo-for-linux-update-check');
    let done = false;
    let idle = null;
    const finish = (error) => {
      if (done) return;
      done = true;
      clearTimeout(idle);
      if (error) {
        try { request.abort(); } catch (_) {}
        out.destroy();
        reject(error);
      } else {
        out.end(() => resolve());
      }
    };
    const touch = () => {
      clearTimeout(idle);
      idle = setTimeout(() => finish(new Error('download stalled')), DOWNLOAD_IDLE_TIMEOUT_MS);
    };
    out.on('error', finish);
    if (signal) signal.addEventListener('abort', () => finish(cancelledError()), { once: true });
    request.on('response', (response) => {
      if (response.statusCode !== 200) {
        finish(new Error(`HTTP ${response.statusCode}`));
        return;
      }
      const total = Number(response.headers['content-length']) || null;
      let received = 0;
      touch();
      response.on('data', (chunk) => {
        touch();
        received += chunk.length;
        if (!out.write(Buffer.from(chunk)) && typeof response.pause === 'function') {
          response.pause();
          out.once('drain', () => response.resume());
        }
        if (onProgress) onProgress(received, total);
      });
      response.on('end', () => finish());
      response.on('error', finish);
    });
    request.on('error', finish);
    touch();
    request.end();
  });
}

function cancelledError() {
  const error = new Error('cancelled');
  error.code = 'CANCELLED';
  return error;
}

// Waits for the given PID to exit (or turn into a zombie), then execs the
// command. Gives up after ~120 s so a cancelled quit never starts a second Zalo.
// /proc is used instead of `kill -0`, which reports EPERM (and so "gone") for
// a process owned by another user.
//
// It first closes every inherited fd above 2: Node/libuv only sets up stdio,
// and Chromium leaves some fds without CLOEXEC (the zygote control socket,
// GPUCache/DawnCache files). Passed on through exec they would orphan the old
// zygotes and keep the old cache files open in the new Zalo. This happens
// before the wait so the waiting shell does not hold them either.
const CLOSE_FDS_BASH = 'for fd in /proc/$$/fd/*; do fd=${fd##*/}; if [ "$fd" -gt 2 ]; then { exec {fd}>&-; } 2>/dev/null; fi; done';
// dash cannot close fds above 9 by number; best effort when bash is missing.
const CLOSE_FDS_SH = 'exec 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-';
const RELAUNCH_BODY = [
  'pid="$1"; shift; i=0',
  'while [ -d "/proc/$pid" ] && ! grep -qs "^State:[[:space:]]*Z" "/proc/$pid/status"; do',
  '  i=$((i+1)); if [ "$i" -gt 1200 ]; then exit 1; fi; sleep 0.1',
  'done',
  'exec "$@"'
].join('\n');

function relaunchShell(exists = fs.existsSync) {
  const bash = ['/bin/bash', '/usr/bin/bash'].find((candidate) => exists(candidate));
  return bash
    ? { command: bash, script: CLOSE_FDS_BASH + '\n' + RELAUNCH_BODY }
    : { command: '/bin/sh', script: CLOSE_FDS_SH + '\n' + RELAUNCH_BODY };
}

// The relaunched AppImage must not inherit variables pointing into the old
// AppImage mount, which disappears once this process exits.
function relaunchEnv(env) {
  const result = { ...env };
  const mount = env.APPDIR;
  for (const key of ['APPDIR', 'APPIMAGE', 'ARGV0', 'OWD']) delete result[key];
  if (mount) {
    for (const [key, value] of Object.entries(result)) {
      if (typeof value !== 'string' || !value.includes(mount)) continue;
      const kept = value.split(':').filter((part) => part && !part.startsWith(mount));
      if (kept.length) result[key] = kept.join(':');
      else delete result[key];
    }
  }
  return result;
}

/**
 * Starts the new AppImage once this process has exited.
 *
 * Not app.relaunch(): Chromium launches its children (the relauncher too)
 * with PR_SET_NO_NEW_PRIVS, so the AppImage runtime's setuid fusermount3
 * cannot mount and the new AppImage dies at once. A Node child process does
 * not get that flag.
 */
function spawnRelauncher({ spawn, pid, execPath, args = [], env = process.env, shell = relaunchShell() }) {
  const child = spawn(shell.command, ['-c', shell.script, 'zalo-relaunch', String(pid), execPath, ...args], {
    detached: true,
    stdio: 'ignore',
    env: relaunchEnv(env)
  });
  if (child && typeof child.unref === 'function') child.unref();
  return child;
}

/**
 * Small window with a progress bar and a Cancel button. Returns
 * { update(state), close() }; onCancel runs on the button or the window's
 * close button. The page state is replayed once it has loaded.
 */
function createProgressWindow({ BrowserWindow, parent = null, title, onCancel }) {
  const win = new BrowserWindow({
    parent: parent || undefined,
    width: 440,
    height: 150,
    resizable: false,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    title,
    show: false,
    webPreferences: { contextIsolation: true, nodeIntegration: false, sandbox: true }
  });
  if (typeof win.setMenuBarVisibility === 'function') win.setMenuBarVisibility(false);
  if (typeof win.removeMenu === 'function') win.removeMenu();
  let closing = false;
  let loaded = false;
  let state = {};
  const apply = () => {
    if (!loaded || win.isDestroyed()) return;
    win.webContents.executeJavaScript(`window.updateProgress(${JSON.stringify(state)})`).catch(() => {});
  };
  win.webContents.on('will-navigate', (event) => event.preventDefault());
  win.webContents.on('did-finish-load', () => { loaded = true; apply(); });
  win.on('page-title-updated', (event, pageTitle) => {
    event.preventDefault();
    if (pageTitle === 'cancel' && !closing) onCancel();
  });
  win.on('close', (event) => {
    if (closing) return;
    event.preventDefault();
    onCancel();
  });
  win.once('ready-to-show', () => { if (!win.isDestroyed()) win.show(); });
  win.loadFile(path.join(__dirname, 'progress.html'));
  return {
    update(next) {
      state = { ...state, ...next };
      apply();
    },
    close() {
      closing = true;
      if (!win.isDestroyed()) win.destroy();
    }
  };
}

function formatMb(bytes) {
  return (bytes / (1024 * 1024)).toFixed(1);
}

function sha256File(file) {
  return new Promise((resolve, reject) => {
    const hash = crypto.createHash('sha256');
    fs.createReadStream(file)
      .on('data', (chunk) => hash.update(chunk))
      .on('end', () => resolve(hash.digest('hex')))
      .on('error', reject);
  });
}

function canReplace(appImage) {
  try {
    if (!appImage || !fs.statSync(appImage).isFile()) return false;
    fs.accessSync(path.dirname(appImage), fs.constants.W_OK);
    return true;
  } catch (_) {
    return false;
  }
}

/**
 * Downloads the update next to the AppImage, verifies it and swaps it in.
 * The running AppImage stays valid: the process keeps the old inode open.
 */
async function installUpdate(update, appImage, download, { onProgress = null, onVerify = null, signal = null } = {}) {
  const dir = path.dirname(appImage);
  const temporary = path.join(dir, `.${path.basename(appImage)}.download`);
  const backup = appImage + '.old';
  const checkCancelled = () => { if (signal && signal.aborted) throw cancelledError(); };
  try {
    fs.rmSync(temporary, { force: true });
    await download(update.downloadUrl, temporary, onProgress, signal);
    checkCancelled();
    if (onVerify) onVerify();
    const size = fs.statSync(temporary).size;
    if (update.size !== null && size !== update.size) {
      throw new Error(`size mismatch (${size} != ${update.size})`);
    }
    if (update.sha256) {
      const actual = await sha256File(temporary);
      if (actual !== update.sha256) throw new Error('SHA-256 mismatch');
    } else if (update.size === null) {
      throw new Error('release has neither a digest nor a size to verify against');
    }
    checkCancelled();
    fs.chmodSync(temporary, 0o755);
    fs.rmSync(backup, { force: true });
    fs.renameSync(appImage, backup);
    try {
      fs.renameSync(temporary, appImage);
    } catch (error) {
      fs.renameSync(backup, appImage);
      throw error;
    }
  } catch (error) {
    fs.rmSync(temporary, { force: true });
    throw error;
  }
}

function register({
  app,
  appDir,
  dialog,
  onTrayMenuChanged,
  env = process.env,
  electron = null,
  fetchRelease = null,
  download = null,
  spawn = null,
  createProgressUi = null,
  pid = process.pid,
  argv = process.argv,
  quitGuardMs = QUIT_GUARD_MS,
  setTimer = setTimeout,
  setRepeat = setInterval
} = {}) {
  if (!app || typeof app.getPath !== 'function') {
    throw new TypeError('Update check requires an Electron app instance.');
  }

  const messages = require('./locale.json').messages;
  let currentLocale = 'vi';
  let available = null;
  // idle | downloading | ready
  let state = 'idle';
  let progress = null;
  let prompting = false;
  let timers = [];

  const translate = (key, vars = {}) => {
    const entry = messages['update-check.' + key];
    const text = entry ? entry[currentLocale] : key;
    return text.replace(/\{(\w+)\}/g, (_, name) => (name in vars ? String(vars[name]) : ''));
  };

  function setLocale(locale) {
    if (locale !== 'vi' && locale !== 'en') return false;
    currentLocale = locale;
    return true;
  }

  const getElectron = () => electron || require('electron');
  const getDialog = () => dialog || getElectron().dialog;

  // A visible Zalo window to attach dialogs to, or null when the app sits in the tray.
  function getParentWindow() {
    try {
      const { BrowserWindow } = getElectron();
      if (!BrowserWindow || typeof BrowserWindow.getAllWindows !== 'function') return null;
      const visible = (win) => !win.isDestroyed() && win.isVisible() && !win.isMinimized()
        && !BACKGROUND_WINDOW_TITLES.has(win.getTitle());
      const focused = typeof BrowserWindow.getFocusedWindow === 'function' ? BrowserWindow.getFocusedWindow() : null;
      if (focused && visible(focused)) return focused;
      return BrowserWindow.getAllWindows().find(visible) || null;
    } catch (_) {
      return null;
    }
  }

  function showBox(options) {
    const parent = getParentWindow();
    return parent ? getDialog().showMessageBox(parent, options) : getDialog().showMessageBox(options);
  }
  const appImage = env.APPIMAGE || null;
  const buildInfo = appDir ? readBuildInfo(appDir) : null;
  // Only an AppImage is updated from these releases; Flatpak, Nix and source
  // runs get their updates elsewhere.
  const enabled = Boolean(buildInfo) && Boolean(appImage) && env[ENV_DISABLE] !== '0';
  const trayChanged = () => { if (typeof onTrayMenuChanged === 'function') onTrayMenuChanged(); };

  const settingsPath = path.join(app.getPath('userData'), 'zalo-linux-settings.json');
  function readSettings() {
    try {
      const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));
      return settings && typeof settings === 'object' && !Array.isArray(settings) ? settings : {};
    } catch (_) {
      return {};
    }
  }
  function rememberNotified(key) {
    try {
      const settings = readSettings();
      fs.mkdirSync(path.dirname(settingsPath), { recursive: true });
      const temporary = settingsPath + '.tmp';
      fs.writeFileSync(temporary, JSON.stringify({ ...settings, [SETTINGS_KEY]: key }, null, 2), { mode: 0o600 });
      fs.renameSync(temporary, settingsPath);
    } catch (error) {
      console.error('[update-check] Cannot save settings:', error.message);
    }
  }

  function openReleasePage(update = available) {
    if (!update) return;
    try {
      getElectron().shell.openExternal(update.url);
    } catch (error) {
      console.error('[update-check] Cannot open release page:', error);
    }
  }

  function showNotification(title, body, onClick) {
    const { Notification } = getElectron();
    if (!Notification || (typeof Notification.isSupported === 'function' && !Notification.isSupported())) return false;
    const notification = new Notification({ title, body });
    if (onClick) notification.on('click', onClick);
    notification.show();
    return true;
  }

  function setWindowProgress(value) {
    try {
      const { BrowserWindow } = getElectron();
      if (!BrowserWindow || typeof BrowserWindow.getAllWindows !== 'function') return;
      for (const win of BrowserWindow.getAllWindows()) {
        if (!win.isDestroyed() && typeof win.setProgressBar === 'function') win.setProgressBar(value);
      }
    } catch (_) {}
  }

  function relaunch() {
    const args = argv.slice(1).filter((arg) => !HIDDEN_FLAGS.has(arg));
    let child;
    try {
      child = spawnRelauncher({ spawn: spawn || require('child_process').spawn, pid, execPath: appImage, args, env });
    } catch (error) {
      console.error('[update-check] Cannot start the relauncher:', error);
      showNotification(translate('title'), translate('restartManually'));
      return false;
    }
    // A window or plugin may refuse to quit. Then stop the waiting relauncher
    // (it would otherwise start a second Zalo once this one exits later) and
    // leave the restart offer in the tray.
    let committed = false;
    const guard = setTimeout(() => {
      if (committed) return;
      try {
        process.kill(-child.pid, 'SIGTERM');
      } catch (_) {
        try { child.kill('SIGTERM'); } catch (__) {}
      }
      console.warn('[update-check] Quit was cancelled; relaunch aborted.');
      trayChanged();
    }, quitGuardMs);
    if (typeof guard.unref === 'function') guard.unref();
    if (typeof app.once === 'function') {
      app.once('will-quit', () => {
        committed = true;
        clearTimeout(guard);
      });
    }
    app.quit();
    return true;
  }

  async function askRestart() {
    const { response } = await showBox({
      type: 'info',
      title: translate('title'),
      message: translate('readyMessage', { version: available.version, commit: available.commit }),
      buttons: [translate('restartNow'), translate('later')],
      defaultId: 0,
      cancelId: 1,
      noLink: true
    });
    if (response === 0) relaunch();
  }

  async function startUpdate() {
    const update = available;
    if (!update || state !== 'idle') return false;
    if (!update.downloadUrl || !canReplace(appImage)) {
      await showBox({
        type: 'warning',
        title: translate('title'),
        message: translate('cannotReplace', { path: appImage || '' }),
        buttons: ['OK'],
        noLink: true
      });
      openReleasePage(update);
      return false;
    }
    state = 'downloading';
    progress = 0;
    trayChanged();

    const controller = new AbortController();
    const cancel = () => controller.abort();
    const versionVars = { version: update.version, commit: update.commit };
    let ui = null;
    try {
      ui = createProgressUi
        ? createProgressUi({ title: translate('title'), onCancel: cancel })
        : createProgressWindow({
          BrowserWindow: getElectron().BrowserWindow,
          parent: getParentWindow(),
          title: translate('title'),
          onCancel: cancel
        });
    } catch (error) {
      console.error('[update-check] Cannot open the progress window:', error);
    }
    if (ui) {
      ui.update({
        message: translate('progressDownloading', versionVars),
        detail: '',
        percent: update.size ? 0 : null,
        cancelLabel: translate('cancel'),
        cancellable: true
      });
    } else {
      // No window: at least say that something is happening.
      showNotification(translate('title'), translate('progressDownloading', versionVars));
    }

    let lastTray = -1;
    let lastUi = -1;
    const onProgress = (received, total) => {
      const size = total || update.size;
      if (!size) return;
      progress = Math.min(100, Math.floor((received / size) * 100));
      setWindowProgress(progress / 100);
      if (ui && (progress !== lastUi || received === size)) {
        lastUi = progress;
        ui.update({
          percent: progress,
          detail: translate('progressDetail', { percent: progress, received: formatMb(received), total: formatMb(size) })
        });
      }
      if (progress >= lastTray + 5) {
        lastTray = progress;
        trayChanged();
      }
    };
    const onVerify = () => {
      if (ui) ui.update({ message: translate('progressVerifying'), percent: null, detail: '' });
    };
    const downloader = download || ((url, dest, cb, signal) => netDownload(getElectron().net, url, dest, cb, signal));
    const closeUi = () => { if (ui) { ui.close(); ui = null; } };
    try {
      await installUpdate(update, appImage, downloader, { onProgress, onVerify, signal: controller.signal });
      closeUi();
      state = 'ready';
      progress = null;
      setWindowProgress(-1);
      trayChanged();
      await askRestart();
      return true;
    } catch (error) {
      closeUi();
      state = 'idle';
      progress = null;
      setWindowProgress(-1);
      trayChanged();
      if (error && error.code === 'CANCELLED') return 'cancelled';
      console.error('[update-check] Update failed:', error);
      const { response } = await showBox({
        type: 'error',
        title: translate('title'),
        message: translate('failed', { error: error.message }),
        buttons: [translate('openPage'), translate('later')],
        defaultId: 0,
        cancelId: 1,
        noLink: true
      });
      if (response === 0) openReleasePage(update);
      return false;
    }
  }

  // Entry point for the notification and the tray item.
  async function promptUpdate() {
    if (!available || prompting) return null;
    if (state === 'downloading') return null;
    prompting = true;
    try {
      if (state === 'ready') {
        await askRestart();
        return 'ready';
      }
      const { response } = await showBox({
        type: 'question',
        title: translate('title'),
        message: translate('confirm', { version: available.version, commit: available.commit }),
        detail: translate('confirmDetail'),
        buttons: [translate('update'), translate('openPage'), translate('later')],
        defaultId: 0,
        cancelId: 2,
        noLink: true
      });
      if (response === 1) openReleasePage();
      if (response !== 0) return response === 1 ? 'page' : 'later';
    } finally {
      prompting = false;
    }
    const result = await startUpdate();
    if (result === 'cancelled') return 'cancelled';
    return result ? 'updated' : 'failed';
  }

  // Once per release: ask right away in the Zalo window when it is on screen,
  // otherwise (started hidden in the tray) fall back to a desktop notification.
  function notify(update) {
    const key = update.assetName;
    if (readSettings()[SETTINGS_KEY] === key) return false;
    const prompt = () => { promptUpdate().catch((error) => console.error('[update-check]', error)); };
    if (getParentWindow()) {
      rememberNotified(key);
      prompt();
      return true;
    }
    const shown = showNotification(
      translate('title'),
      translate('body', { version: update.version, commit: update.commit }),
      prompt
    );
    if (shown) rememberNotified(key);
    return shown;
  }

  async function check() {
    if (!enabled || state !== 'idle') return null;
    try {
      const release = fetchRelease
        ? await fetchRelease(buildInfo.releaseRepo)
        : await fetchLatestRelease(buildInfo.releaseRepo, getElectron().net);
      if (state !== 'idle') return available;
      const update = findUpdate(release, buildInfo);
      const changed = (update && update.assetName) !== (available && available.assetName);
      available = update;
      if (changed) trayChanged();
      if (update) notify(update);
      return update;
    } catch (error) {
      console.warn('[update-check] Check failed:', error.message);
      return null;
    }
  }

  if (enabled) {
    timers.push(setTimer(check, FIRST_CHECK_DELAY_MS));
    timers.push(setRepeat(check, CHECK_INTERVAL_MS));
    for (const timer of timers) if (timer && typeof timer.unref === 'function') timer.unref();
  }

  return {
    isEnabled: () => enabled,
    check,
    promptUpdate,
    startUpdate,
    getAvailable: () => available,
    getState: () => state,
    getTrayMenuItems() {
      if (!available) return [];
      if (state === 'downloading') {
        return [{ label: translate('trayDownloading', { percent: progress || 0 }), enabled: false }];
      }
      const label = state === 'ready'
        ? translate('trayReady')
        : translate('trayItem', { version: available.version, commit: available.commit });
      return [{
        label,
        click: () => { promptUpdate().catch((error) => console.error('[update-check]', error)); }
      }];
    },
    setLocale,
    onLanguageUpdateRequest(request) {
      if (request && setLocale(request.locale) && available) trayChanged();
    },
    dispose() {
      for (const timer of timers) {
        clearTimeout(timer);
        clearInterval(timer);
      }
      timers = [];
    }
  };
}

module.exports = {
  register,
  findUpdate,
  readBuildInfo,
  compareVersions,
  installUpdate,
  spawnRelauncher,
  relaunchEnv,
  relaunchShell,
  createProgressWindow
};
