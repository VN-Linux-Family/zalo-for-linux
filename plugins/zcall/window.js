/**
 * plugins/zcall/window.js
 *
 * The call window. The Rust helper connects to the authenticated loopback
 * socket opened here, sends call state and binary H.264 previews, and receives
 * user actions/device requests. Zalo itself owns the helper process and the
 * encrypted call-v2 Unix sockets. This plugin remains the Electron frontend
 * during the transition; media capture, encoding and device discovery are Rust.
 *
 * The first line from the helper is ZCALL_UI_TOKEN. Control messages are
 * JSON-lines. Video/local-preview frames use a {bin:N} header followed by N
 * raw Annex-B bytes. The existing TCP protocol is retained unchanged.
 */

'use strict';

const net = require('net');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const incoming = require('./incoming');
const { pathToFileURL } = require('url');
const { createStore } = require('../advanced-settings/store');

// Zalo's macOS call window, measured (CSS px = pt): voice 450x672, video and
// group 640x400 (content, under the desktop's own title bar).
const VOICE_WIDTH = 450;
const VOICE_HEIGHT = 672;
const VIDEO_WIDTH = 640;
const VIDEO_HEIGHT = 400;
const COMPACT_WIDTH = 320; // compact: the picture (180) and the bar (50) in the screen's corner
const COMPACT_HEIGHT = 230;
const COMPACT_MARGIN = 16;
// Video and group windows can be resized, maximized and made full screen;
// below this content size the bar's tools would overlap the pills.
const VIDEO_MIN_WIDTH = 560;
const VIDEO_MIN_HEIGHT = 350;
const STATE_WAIT_MS = 1000; // leaving full screen / maximized is async on Linux
const CLOSE_DELAY_MS = 1500; // keep "Đã kết thúc" visible a moment

// Zalo's own call icons, sounds and fonts (scripts/extract-zcall-assets.js):
// app/ next to the root main.js in a checkout, next to the executable when packaged.
const ASSETS_DIR = (() => {
  const dev = path.join(__dirname, '..', '..', 'app', 'zcall-assets');
  return fs.existsSync(dev) ? dev : path.join(path.dirname(process.execPath), 'app', 'zcall-assets');
})();

let server = null;
let serverReadyPromise = null;
let client = null;
let win = null;
let winReady = false;
let lastState = null;
let closeTimer = null;
let engineReconnectTimer = null;
let rendererLoadedOnce = false;
let videoMode = false;
let compact = null; // the normal bounds while the window is compact
let camStartOff = false; // "Trả lời không mở camera": the call window starts with the camera off
let sharingScreen = false;
let screenActionEpoch = 0;
let screenActionPending = false;
let screenActionStarted = false;
let screenStopPending = false;
let screenRequestId = null;
let languageLocale = 'vi';
let expandedAt = 0;

// Device discovery and preferences are owned by the Rust engine. Requests use
// the same authenticated loopback TCP connection as call state and video.
const pendingEngineRequests = new Map();
const queuedEngineActions = [];
let engineRequestSequence = 0;

function send(message) {
  if (client && !client.destroyed) {
    try { client.write(JSON.stringify(message) + '\n'); return true; } catch (_) { /* reconnect below */ }
  }
  if (queuedEngineActions.length < 128) queuedEngineActions.push(message);
  return false;
}

function requestEngine(action) {
  const requestId = 'zcall-ui-' + process.pid + '-' + (++engineRequestSequence);
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pendingEngineRequests.delete(requestId);
      const queuedIndex = queuedEngineActions.findIndex((message) => message.requestId === requestId);
      if (queuedIndex >= 0) queuedEngineActions.splice(queuedIndex, 1);
      reject(new Error('ZCall engine request timed out: ' + action.action));
    }, 5000);
    pendingEngineRequests.set(requestId, { resolve, reject, timer });
    send({ ...action, requestId });
  });
}

function rejectPendingEngineRequests(error) {
  for (const [requestId, pending] of pendingEngineRequests) {
    clearTimeout(pending.timer);
    pending.reject(error);
    pendingEngineRequests.delete(requestId);
  }
}

async function shouldBlockIncoming() {
  try {
    const devices = await requestEngine({ action: 'listDevices' });
    const prefs = devices && devices.prefs || {};
    return Boolean(
      (prefs.noMicBlock && (!devices.mics || devices.mics.length === 0)) ||
      (prefs.noSpeakerBlock && (!devices.speakers || devices.speakers.length === 0))
    );
  } catch (error) {
    console.error('[zcall-window] device query:', error.message);
    return false;
  }
}

function pushState(state) {
  if (camStartOff && state && !state.camStartOff) state = { ...state, camStartOff: true };
  lastState = state;
  if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-state', state);
}

function ensureWindow() {
  const { BrowserWindow } = require('electron');
  clearTimeout(closeTimer);
  if (win && !win.isDestroyed()) return win;
  winReady = false;
  rendererLoadedOnce = false;
  win = new BrowserWindow({
    width: VOICE_WIDTH,
    height: VOICE_HEIGHT,
    useContentSize: true,
    resizable: false,
    maximizable: false,
    fullscreenable: false,
    center: true,
    alwaysOnTop: true,
    skipTaskbar: false,
    title: 'Zalo Call',
    backgroundColor: '#000000',
    // Local static page; the only remote content is the avatar image.
    webPreferences: { contextIsolation: false, nodeIntegration: true },
  });
  win.zcallWindow = true; // main.js: closing it ends the call, it does not hide to the tray
  // No menu bar at all: a hidden one comes back when full screen ends (25 px).
  win.removeMenu();
  // The size above counted the (now removed) menu bar in: 25 px too tall without this.
  win.setContentSize(VOICE_WIDTH, VOICE_HEIGHT);
  win.webContents.on('did-finish-load', () => {
    winReady = true;
    win.webContents.send('zcall-ui-language', languageLocale);
    if (lastState) {
      const state = rendererLoadedOnce && lastState.phase === 'connected'
        ? { ...lastState, resume: true }
        : lastState;
      win.webContents.send('zcall-ui-state', state);
    }
    rendererLoadedOnce = true;
  });
  // Closing the window during a call ends it.
  win.on('close', () => {
    // Invalidate a portal start that is still waiting for the compact job;
    // the engine's hangup path owns cancellation once a worker was dispatched.
    screenActionEpoch++;
    screenActionPending = false;
    screenActionStarted = false;
    screenStopPending = false;
    sharingScreen = false;
    if (lastState && lastState.phase !== 'ended') send({ action: 'hangup' });
    lastState = null;
  });
  win.on('enter-full-screen', sendFullScreen);
  win.on('leave-full-screen', sendFullScreen);
  let blurTimer = null;
  win.on('blur', () => {
    clearTimeout(blurTimer);
    if (!sharingScreen || compact !== null || compacting || Date.now() - expandedAt < 500) return;
    blurTimer = setTimeout(() => {
      if (!win || win.isDestroyed() || !sharingScreen || compact !== null || compacting) return;
      setCompact(true).catch((e) => console.error('[zcall-window] compact on blur:', e.message));
    }, 50);
  });
  win.on('focus', () => {
    clearTimeout(blurTimer);
  });
  win.on('closed', () => { win = null; winReady = false; rendererLoadedOnce = false; videoMode = false; compact = null; compacting = false; sharingScreen = false; screenStopPending = false; clearTimeout(blurTimer); });
  // From a file, not a data: URL: WebCodecs (video) needs a secure context.
  win.loadFile(path.join(__dirname, 'ui', 'call.html'), {
    query: { assets: ASSETS_DIR, test: process.env.ZCALL_TEST_VIDEO === '1' ? '1' : '0' },
  });
  return win;
}

// Content size, keeping the window's centre.
function setContent(w, h) {
  const [x, y] = win.getPosition();
  const [cw, ch] = win.getContentSize();
  setBounds({ x: Math.round(x + (cw - w) / 2), y: Math.round(y + (ch - h) / 2), width: w, height: h });
}

// Video or group call (or its first frame): the 640x400 landscape window.
function enterVideoMode() {
  if (videoMode || !win || win.isDestroyed()) return;
  videoMode = true;
  if (compact) { // back to the video size when expanded
    compact = { x: Math.round(compact.x + (compact.width - VIDEO_WIDTH) / 2), y: Math.round(compact.y + (compact.height - VIDEO_HEIGHT) / 2), width: VIDEO_WIDTH, height: VIDEO_HEIGHT };
    return;
  }
  setContent(VIDEO_WIDTH, VIDEO_HEIGHT);
  applySizing();
}

// Voice and compact: a fixed size. Video and group: the user's size, the
// title bar's maximize button, full screen (F11, double click, the bar's button).
function applySizing() {
  // Linux: setResizable(false) saves the size limits and setResizable(true)
  // puts them back, so the minimum is cleared before and set after. 1x1, not
  // 0x0: Electron 22 ignores setMinimumSize(0, 0).
  const free = videoMode && !compact;
  win.setMaximizable(free); // macOS / Windows; Linux follows resizable
  win.setFullScreenable(free);
  if (free) {
    win.setResizable(true);
    const [ow, oh] = win.getSize();
    const [cw, ch] = win.getContentSize();
    win.setMinimumSize(VIDEO_MIN_WIDTH + ow - cw, VIDEO_MIN_HEIGHT + oh - ch);
  } else {
    win.setMinimumSize(1, 1);
    win.setResizable(false);
  }
}

function sendFullScreen() {
  if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-fullscreen', win.isFullScreen());
}

// Resolves once the window has left full screen / maximized (or after STATE_WAIT_MS).
function leaveState(isOn, event, leave) {
  if (!isOn()) return Promise.resolve();
  return new Promise((resolve) => {
    const w = win;
    const done = () => { clearTimeout(t); w.removeListener(event, done); resolve(); };
    const t = setTimeout(done, STATE_WAIT_MS);
    w.once(event, done);
    leave();
  });
}

// Content size at a position from getPosition(). Known issue: under X11 with
// the window manager's title bar, getPosition() gives the content's corner but
// setPosition() places the frame's, so a compact / expand cycle leaves the
// window one title bar (32 px on GNOME) lower; reading the position back on
// 'move' did not fix it reliably.
function setBounds(b) {
  win.setResizable(true);
  win.setContentSize(b.width, b.height);
  win.setResizable(videoMode && !compact);
  win.setPosition(b.x, b.y);
}

// Compact: a small tile in the bottom-right corner of the window's screen,
// so it hides little of a shared screen; expanding restores the bounds.
// A full screen or maximized window leaves that state first and gets it back
// on expand.
// compact: { x, y } outer position, { width, height } content size, of the
// normal window; full / max: its state before.
let compacting = false;
let compactJob = Promise.resolve(); // the last compact / expand, awaited before the screen picker
async function setCompact(on) {
  if (!win || win.isDestroyed() || compacting || !!compact === on) return;
  compacting = true;
  try {
    if (on) {
      const full = win.isFullScreen();
      const max = win.isMaximized();
      await leaveState(() => win.isFullScreen(), 'leave-full-screen', () => win.setFullScreen(false));
      if (win.isDestroyed()) return;
      await leaveState(() => win.isMaximized(), 'unmaximize', () => win.unmaximize());
      if (win.isDestroyed()) return;
      const [x, y] = win.getPosition();
      const [width, height] = win.getContentSize();
      compact = { x, y, width, height, full, max };
      applySizing();
      const wa = require('electron').screen.getDisplayMatching(win.getBounds()).workArea;
      win.setResizable(true);
      win.setContentSize(COMPACT_WIDTH, COMPACT_HEIGHT);
      win.setResizable(false);
      const [ow, oh] = win.getSize();
      win.setPosition(wa.x + wa.width - ow - COMPACT_MARGIN, wa.y + wa.height - oh - COMPACT_MARGIN);
    } else {
      const before = compact;
      compact = null;
      setBounds(before);
      applySizing();
      if (before.max) win.maximize();
      if (before.full) win.setFullScreen(true);
      expandedAt = Date.now();
    }
  } finally {
    compacting = false;
  }
  win.webContents.send('zcall-ui-compact', on);
  win.focus();
}

async function onEngineMessage(m) {
  if (m && m.requestId && pendingEngineRequests.has(m.requestId)) {
    const pending = pendingEngineRequests.get(m.requestId);
    pendingEngineRequests.delete(m.requestId);
    clearTimeout(pending.timer);
    pending.resolve(m.data);
    return;
  }
  if (m.type === 'state') {
    // Incoming: the notice in the screen's corner, not the call window.
    if (m.phase === 'incoming') {
      if (await shouldBlockIncoming()) {
        send({ action: 'reject', why: 'no-device' });
        return;
      }
      lastState = m;
      if (!incoming.isOpen()) camStartOff = false;
      incoming.show(m, ASSETS_DIR);
      return;
    }
    if (incoming.isOpen()) {
      incoming.close();
      // Missed or declined before answering: nothing else to show.
      if (m.phase === 'ended') { lastState = null; camStartOff = false; return; }
    }
    if (m.phase === 'outgoing' || m.phase === 'ended') {
      camStartOff = false; sharingScreen = false;
      screenActionEpoch++; screenActionPending = false; screenActionStarted = false; screenStopPending = false; screenRequestId = null;
    }
    const previous = lastState;
    const windowAlive = !!(win && !win.isDestroyed());
    const resume = !windowAlive && previous && previous.phase === 'connected' && m.phase === 'connected';
    const state = {
      ...m,
      ...(resume ? { resume: true } : {}),
      ...(camStartOff ? { camStartOff: true } : {}),
    };
    const first = !windowAlive || !previous || previous.phase === 'ended' || previous.phase === 'incoming';
    const w = ensureWindow();
    pushState(state);
    w.setTitle('Zalo Call - ' + (m.name || 'Zalo'));
    if (m.video || m.group) enterVideoMode();
    if (first) {
      w.show();
      w.focus();
    }
  } else if (m.type === 'audioFault') {
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-audio-fault', m.kind);
  } else if (m.type === 'speaking') {
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-speaking', m);
  } else if (m.type === 'micLevel') {
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-mic-level', m.level);
  } else if (m.type === 'cameraError') {
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-camera-error', { message: m.message });
  } else if (m.type === 'screenSources') {
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-screen-sources', m.windows || []);
  } else if (m.type === 'screenSourceMode') {
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-screen-source-mode', m.source || 'window');
  } else if (m.type === 'screenCaptureAccepted') {
    if (m.requestId && screenRequestId && m.requestId !== screenRequestId) return;
    screenActionPending = false;
    screenActionStarted = true;
    console.info('[zcall-window] Rust accepted screen capture; compacting call window');
    compactJob = compactJob.then(async () => {
      if (screenRequestId !== m.requestId || screenStopPending) return;
      if (!compact) await setCompact(true);
      if (!compact) throw new Error('Could not compact the call window after Rust accepted screen capture');
    }).catch((error) => {
      console.error('[zcall-window] screen-share compact after Rust acceptance:', error.message);
      send({ action: 'screen', on: false, requestId: m.requestId });
      if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-screen-error', { message: error.message });
    });
  } else if (m.type === 'screenStarted') {
    if (m.requestId && screenRequestId && m.requestId !== screenRequestId) return;
    screenActionPending = false;
    screenActionStarted = false;
    screenStopPending = false;
    sharingScreen = true;
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-screen-started', m);
  } else if (m.type === 'screenStopped') {
    // Terminal notifications are tagged with the capture transaction that
    // produced them. Ignore a delayed stop from the previous portal session.
    if (m.requestId && screenRequestId && m.requestId !== screenRequestId) return;
    // Legacy untagged stop events may race with a newer start. Tagged events
    // are safe to apply only when their requestId matches the current attempt.
    if (!m.requestId && (screenActionPending || screenActionStarted)) return;
    screenStopPending = false;
    screenActionEpoch++;
    screenActionPending = false;
    screenActionStarted = false;
    sharingScreen = false;
    screenRequestId = null;
    if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-screen-stopped');
  } else if (m.type === 'screenError') {
    if (m.requestId && screenRequestId && m.requestId !== screenRequestId) return;
    const stoppedByUser = screenStopPending;
    // Every terminal event clears the old transaction. A newer retry is only
    // queued by Rust after the old worker has exited, so its own start remains
    // authoritative and must not inherit these flags.
    screenActionEpoch++;
    screenActionPending = false;
    screenActionStarted = false;
    screenStopPending = false;
    sharingScreen = false;
    screenRequestId = null;
    if (win && !win.isDestroyed() && winReady) {
      if (stoppedByUser) win.webContents.send('zcall-ui-screen-stopped');
      else win.webContents.send('zcall-ui-screen-error', { message: m.message });
    }
  } else if (m.type === 'video') {
    if (!win || win.isDestroyed() || !winReady || !lastState || lastState.phase !== 'connected') return;
    enterVideoMode();
    win.webContents.send('zcall-ui-video', m);
  } else if (m.type === 'localVideo') {
    if (!win || win.isDestroyed() || !winReady || !lastState || lastState.phase !== 'connected') return;
    win.webContents.send('zcall-ui-local-video', m);
  } else if (m.type === 'close') {
    camStartOff = false;
    if (incoming.isOpen()) incoming.close();
    if (!win || win.isDestroyed()) { lastState = null; return; }
    pushState({ ...(lastState || {}), phase: 'ended', statusCode: m.statusCode || 'ended', statusParams: m.statusParams || {} });
    closeTimer = setTimeout(() => {
      lastState = null;
      if (win && !win.isDestroyed()) win.destroy();
    }, CLOSE_DELAY_MS);
  }
}

// Control lines are JSON. Video and local-preview frames carry a {bin:N}
// header followed by N raw bytes, preserving the existing binary socket framing.
function readEngineMessages(state, chunk, token) {
  const piece = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
  state.buf = state.buf.length ? Buffer.concat([state.buf, piece]) : piece;
  const messages = [];
  let authed = false;
  for (;;) {
    if (state.pending) {
      if (state.buf.length < state.pending.bin) break;
      const n = state.pending.bin;
      const m = state.pending;
      m.data = Buffer.from(state.buf.subarray(0, n));
      delete m.bin;
      state.buf = state.buf.subarray(n);
      state.pending = null;
      messages.push(m);
      continue;
    }
    const i = state.buf.indexOf(0x0a);
    if (i < 0) break;
    const line = state.buf.subarray(0, i).toString('utf8');
    state.buf = state.buf.subarray(i + 1);
    if (!state.authed) {
      if (line !== token) return { messages, authed, fatal: true };
      state.authed = true;
      authed = true;
      continue;
    }
    let m;
    try { m = JSON.parse(line); } catch (_) { continue; }
    if (m && (m.type === 'video' || m.type === 'localVideo') && Number.isInteger(m.bin) && m.bin > 0 && m.bin <= 8 * 1024 * 1024) {
      state.pending = m;
      continue;
    }
    messages.push(m);
  }
  return { messages, authed, fatal: false };
}

function onConnection(sock) {
  const token = process.env.ZCALL_UI_TOKEN;
  const state = { buf: Buffer.alloc(0), pending: null, authed: false };
  sock.on('data', (d) => {
    const r = readEngineMessages(state, d, token);
    if (r.fatal) { sock.destroy(); return; }
    if (r.authed) {
      if (client && client !== sock) client.destroy(); // engine restarted
      client = sock;
      clearTimeout(engineReconnectTimer);
      engineReconnectTimer = null;
      if (win && !win.isDestroyed() && winReady) {
        win.webContents.send('zcall-ui-connection', { connected: true });
      }
      while (queuedEngineActions.length && client === sock && !sock.destroyed) {
        const message = queuedEngineActions.shift();
        try { sock.write(JSON.stringify(message) + '\n'); } catch (_) { queuedEngineActions.unshift(message); break; }
      }
    }
    for (const m of r.messages) onEngineMessage(m);
  });
  sock.on('error', () => {});
  sock.on('close', () => {
    if (client !== sock) return;
    client = null;
    rejectPendingEngineRequests(new Error('ZCall engine disconnected'));
    if (win && !win.isDestroyed() && winReady) {
      win.webContents.send('zcall-ui-connection', { connected: false });
    }
    // Keep the call window and latest call state during a transient helper/UI
    // socket restart. The Rust transport retries and replays the current state.
    clearTimeout(engineReconnectTimer);
    if (lastState && lastState.phase !== 'ended') {
      engineReconnectTimer = setTimeout(() => {
        engineReconnectTimer = null;
        if (!client && lastState) {
          queuedEngineActions.length = 0;
          onEngineMessage({ type: 'close', statusCode: 'disconnected' });
        }
      }, 10000);
    }
  });
}

/** Opens the socket and exports ZCALL_UI_PORT / ZCALL_UI_TOKEN. Call before the engine starts. */
function start() {
  if (server) return serverReadyPromise || Promise.resolve();
  const { ipcMain, app } = require('electron');
  const soundStore = createStore(app.getPath('userData'));
  const soundPages = ['call.html', 'incoming.html'].map((file) =>
    pathToFileURL(path.join(__dirname, 'ui', file)).href);
  ipcMain.handle('zcall-ui-sounds', (event) => {
    const frame = event.senderFrame;
    if (!frame || frame !== event.sender.mainFrame ||
        !soundPages.includes(frame.url.split(/[?#]/)[0])) {
      throw new Error('Call sound request was rejected.');
    }
    return soundStore.soundUrls();
  });
  delete process.env.ZCALL_UI_PORT;
  process.env.ZCALL_UI_TOKEN = crypto.randomBytes(16).toString('hex');
  server = net.createServer(onConnection);
  server.on('error', (e) => console.error('[zcall-window]', e.message));
  serverReadyPromise = new Promise((resolve, reject) => {
    const listening = () => {
      process.env.ZCALL_UI_PORT = String(server.address().port);
      resolve();
    };
    server.once('error', reject);
    server.listen(0, '127.0.0.1', listening);
  }).catch((error) => {
    server = null;
    serverReadyPromise = null;
    delete process.env.ZCALL_UI_PORT;
    delete process.env.ZCALL_UI_TOKEN;
    throw error;
  });
  ipcMain.handle('zcall-ui-devices', () => requestEngine({ action: 'listDevices' }));
  ipcMain.on('call-send-to-native', (event, msg) => {
    if (!msg || typeof msg !== 'object') return;
    if (msg.command === 'listDevice') {
      if (!msg.data) {
        requestEngine({ action: 'zaloListDevices' }).then((data) => {
          if (event.sender && !event.sender.isDestroyed()) event.sender.send('call-response-listDevice', data);
        }).catch((error) => console.error('[zcall-window] Zalo device list:', error.message));
      } else {
        send({ action: 'zaloDeviceConfig', data: msg.data });
      }
    } else if (msg.command === 'advancedOptions') {
      send({ action: 'advancedOptions', data: msg.data || {} });
    } else if (msg.command === 'init' && msg.data) {
      send({ action: 'zaloDeviceConfig', data: msg.data });
      send({ action: 'advancedOptions', data: msg.data });
    }
  });
  // Compact / expand, from the page. Picking the screen to share happens with
  // the window compact and focused: GNOME puts the portal's dialog right under
  // the focused window, so above Zalo's main window (see ui/call.js).
  ipcMain.on('zcall-ui-window', (_e, op) => {
    if (!win || win.isDestroyed()) return;
    if (op === 'compact' || op === 'expand') {
      if (win.isMinimized()) win.restore();
      win.show();
      compactJob = compactJob
        .then(() => setCompact(op === 'compact'))
        .catch((e) => console.error('[zcall-window] compact:', e.message));
    } else if (op === 'fullscreen') { // toggle, video / group only
      if (videoMode && !compact && !compacting) win.setFullScreen(!win.isFullScreen());
    } else if (op === 'leave-fullscreen') {
      if (win.isFullScreen()) win.setFullScreen(false);
    }
  });
  // The notice's answer: accept, maybe with the camera off; the call window opens with the next state.
  ipcMain.on('zcall-incoming-answer', (_e, opts) => {
    camStartOff = !!(opts && opts.camStartOff);
    send({ action: 'accept', camStartOff });
  });
  ipcMain.on('zcall-ui-action', (_e, action) => {
    if (!action || typeof action.action !== 'string') return;
    if (action.action === 'screen') {
      if (action.on) {
        // Ignore duplicate renderer IPC while the chooser, PipeWire stream,
        // or an established share is already active.
        if (screenActionPending || screenActionStarted || sharingScreen) return;
        const epoch = ++screenActionEpoch;
        action.requestId = String(epoch);
        screenRequestId = action.requestId;
        screenStopPending = false;
        screenActionPending = true;
        // Rust owns call validation, backend selection and capture startup.
        // The window is compacted only after Rust acknowledges the worker spawn.
        if (!client || client.destroyed) {
          screenActionPending = false;
          screenActionStarted = false;
          console.error('[zcall-window] screen-share startup: ZCall engine disconnected');
          if (win && !win.isDestroyed() && winReady) {
            win.webContents.send('zcall-ui-screen-error', { message: 'ZCall engine disconnected' });
          }
          return;
        }
        console.info('[zcall-window] forwarding screen capture request to Rust');
        send(action);
      } else {
        // Stop is sent to Rust even while startup is pending. The authenticated
        // TCP stream preserves ordering, and Rust owns cancellation/cleanup.
        screenActionEpoch++;
        action.requestId = screenRequestId || String(cancelEpoch);
        screenActionPending = false;
        screenActionStarted = false;
        screenStopPending = true;
        // TCP preserves action order: Rust receives stop after the in-flight
        // start and owns cancellation/terminal state for that request.
        send(action);
      }
      return;
    }
    if (lastState && (action.action === 'mute' || action.action === 'speaker')) {
      lastState = { ...lastState, [action.action === 'mute' ? 'muted' : 'speakerOff']: !!action.on };
      pushState(lastState);
    }
    send(action);
  });
  return serverReadyPromise;
}

function setLocale(locale) {
  if (locale !== 'vi' && locale !== 'en') return false;
  languageLocale = locale;
  incoming.setLocale(locale);
  if (win && !win.isDestroyed() && winReady) win.webContents.send('zcall-ui-language', locale);
  return true;
}

function stop() {
  if (win && !win.isDestroyed()) win.destroy();
  incoming.close();
  rejectPendingEngineRequests(new Error('ZCall frontend stopped'));
  clearTimeout(engineReconnectTimer);
  engineReconnectTimer = null;
  queuedEngineActions.length = 0;
  if (server) server.close();
  server = null;
  serverReadyPromise = null;
  delete process.env.ZCALL_UI_PORT;
  delete process.env.ZCALL_UI_TOKEN;
}

module.exports = { start, stop, setLocale, readEngineMessages };
