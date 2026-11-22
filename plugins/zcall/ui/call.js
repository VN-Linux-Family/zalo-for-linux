/**
 * plugins/zcall/ui/call.js (renderer): the call window.
 *
 * State comes from the engine through plugins/zcall/window.js
 * ('zcall-ui-state'); clicks go back as 'zcall-ui-action'. Rust owns camera
 * and screen capture, H.264 encoding and media transmission; this renderer
 * decodes received and local preview frames and renders the call UI.
 */

'use strict';

const { ipcRenderer } = require('electron');
const { setIcon, playSound, stopSound } = window.zcallAssets;
const $ = (id) => document.getElementById(id);
const tr = (key, params) => window.zcallI18n.t(key, params);
ipcRenderer.on('zcall-ui-language', (_event, locale) => {
  if (!window.zcallI18n.apply(locale)) return;
  document.querySelectorAll('[data-i18n-key]').forEach((el) => {
    const value = tr(el.dataset.i18nKey);
    if (el.hasAttribute('title')) el.title = value;
    else el.textContent = value;
  });
  const s = state || {};
  $('mute').title = s.muted ? tr('unmute') : tr('mute');
  $('cam').title = sharing ? tr('sharing') : (noCamera ? tr('noCamera') : (camOn ? tr('cameraOff') : tr('cameraOn')));
  $('camCaret').title = sharing ? tr('sharing') : tr('chooseCamera');
  $('screen').title = sharing ? tr('stopScreen') : tr('screen');
  const tilesOn = document.body.classList.contains('has-tiles');
  $('layout').title = tilesOn ? (groupLayout === 'grid' ? tr('speakerLayout') : tr('gridLayout')) : tr('layout');
  selfTile.title = tr('pin');
  selfName.textContent = tr('self');
  $('compactBtn').textContent = compactMode ? tr('expand') : tr('compact');
  $('compactBtn').title = compactMode ? tr('expand') : tr('compact');
  $('full').title = fullScreen ? tr('leaveFullscreen') : tr('fullscreen');
  refreshLocalizedStatuses();
});

// Global state variables
let state = {};
let tick = null;
let compactMode = false, autoCompact = false;
let fullScreen = false;
let groupLayout = 'grid', pinned = null, talking = null;
const tiles = new Map();
let camOn = true, noCamera = false, sharing = false, pendingScreen = null;
let camPref = null, curCamId = '';
let localDecoder = null, localCodec = null, localTs = 0;
let dec = null, decCodec = null, ts = 0;
let decErrLogged = false, shown = 0, shownSince = 0, shownTotal = 0, rxBytes = 0;
let micSlider = null, micMeter = null, micPct = null, micAutoSwitch = null, micModeBadge = null, micDragging = false, micSentAt = 0, micTimer = null;

const act = (action, extra) => ipcRenderer.send('zcall-ui-action', Object.assign({ action }, extra || {}));

$('end').onclick = () => act('hangup');
$('mute').onclick = () => { const on = !state.muted; act('log', { text: 'mute ' + (on ? 'on' : 'off') }); act('mute', { on }); };
$('layout').onclick = () => {
  if (document.body.classList.contains('has-tiles')) {
    groupLayout = groupLayout === 'grid' ? 'conference' : 'grid';
    if (groupLayout === 'grid') pinned = null;
    act('log', { text: 'group layout ' + groupLayout });
    render();
    layoutTiles();
    return;
  }
  document.body.classList.toggle('split');
};
$('compactBtn').onclick = () => { autoCompact = false; ipcRenderer.send('zcall-ui-window', compactMode ? 'expand' : 'compact'); };
// Full screen (video and group): the bar's button, F11, a double click on the picture; Esc leaves it.
const toggleFull = () => { if (state.video && !compactMode) ipcRenderer.send('zcall-ui-window', 'fullscreen'); };
$('full').onclick = toggleFull;
$('main').addEventListener('dblclick', (e) => { if (!e.target.closest('button')) toggleFull(); });
ipcRenderer.on('zcall-ui-fullscreen', (_e, on) => {
  fullScreen = !!on;
  document.body.classList.toggle('fullscreen', fullScreen);
  setIcon($('fullIcon'), fullScreen ? 'fullscreenOff' : 'fullscreen');
  $('full').title = fullScreen ? tr('leaveFullscreen') : tr('fullscreen');
});
ipcRenderer.on('zcall-ui-compact', (_e, on) => {
  compactMode = on;
  if (on) closeMenu();
  if (on && (sharing || pendingScreen)) autoCompact = true;
  document.body.classList.toggle('compact', on);
  render();
});
window.addEventListener('blur', () => {
  if (sharing && !compactMode) {
    closeMenu();
    autoCompact = true;
    ipcRenderer.send('zcall-ui-window', 'compact');
  }
});

function fmt(ms) {
  const s = Math.max(0, Math.floor(ms / 1000)); const h = Math.floor(s / 3600);
  const mm = String(Math.floor(s / 60) % 60).padStart(2, '0'), ss = String(s % 60).padStart(2, '0');
  return (h ? h + ':' : '') + mm + ':' + ss;
}

// Ringback while our call rings; the end tone when a call that was up ends.
function sounds(before) {
  if (state.phase === 'outgoing') playSound('ringback', true); else stopSound('ringback');
  if (state.phase === 'ended' && before && before !== 'ended') playSound('end', false);
}

function refreshLocalizedCallStatus() {
  const s = state || {};
  const ended = s.phase === 'ended';
  const statusCode = s.statusCode || ({ outgoing: 'calling', connecting: 'connecting', ended: 'ended' }[s.phase] || '');
  $('status').textContent = s.phase === 'connected' && !ended && !s.group ? '' : (statusCode ? tr(statusCode, s.statusParams) : '');
}

function refreshLocalizedStatuses() {
  refreshLocalizedCallStatus();
  const s = state || {};
  const ended = s.phase === 'ended';
  $('audioNotice').textContent = s.noticeCode ? tr(s.noticeCode, s.noticeParams) : '';
  $('audioNotice').classList.toggle('hide', !s.noticeCode || ended);
  for (const tile of tiles.values()) {
    const code = tile.memberState;
    tile.status.textContent = ['calling', 'ringing', 'busy', 'declined'].includes(code) ? tr(code) : '';
  }
}

function render() {
  const s = state;
  const ended = s.phase === 'ended';
  const name = s.name || 'Zalo';
  document.title = 'Zalo Call - ' + name;
  for (const p of ['outgoing', 'connecting', 'connected', 'ended']) document.body.classList.toggle('phase-' + p, s.phase === p);
  document.body.classList.toggle('is-video', !!s.video);
  document.body.classList.toggle('is-sharing', sharing || !!pendingScreen);
  if (s.group) {
    document.body.classList.add('has-tiles');
  } else {
    document.body.classList.remove('has-tiles', 'conference');
  }
  refreshLocalizedCallStatus();
  $('peerName').textContent = name;
  const av = $('avatar'), bg = $('bg');
  if (s.avatar) {
    const url = 'url("' + String(s.avatar).replace(/["\\\n]/g, '') + '")';
    av.style.backgroundImage = url; av.textContent = '';
  } else {
    av.style.backgroundImage = ''; av.textContent = name.trim().charAt(0).toUpperCase();
  }
  // Call audio: use self's blurred avatar as background instead of peer's.
  // Call video when cam is off: use peer's avatar as blurred background.
  const isAudioCall = !s.video && !s.group;
  const bgAvatar = isAudioCall ? (s.selfAvatar || s.avatar) : (s.avatar || s.selfAvatar);
  if (bgAvatar) {
    bg.style.backgroundImage = 'url("' + String(bgAvatar).replace(/["\\\n]/g, '') + '")';
  } else {
    bg.style.backgroundImage = '';
  }
  // The peer's camera off: drop its last (frozen) frame, show the avatar.
  if (s.peerCamOff) document.body.classList.remove('has-video');
  $('peerCam').classList.toggle('hide', !(s.video && s.peerCamOff && s.phase === 'connected'));
  $('peerMic').classList.toggle('hide', !(s.peerMuted && s.phase === 'connected'));
  $('peerShare').classList.toggle('hide', !(s.peerSharing && s.phase === 'connected'));
  // Our microphone / speaker stopped (no sound server, device gone): shown in every layout.
  $('audioNotice').textContent = s.noticeCode ? tr(s.noticeCode, s.noticeParams) : '';
  $('audioNotice').classList.toggle('hide', !s.noticeCode || ended);
  // Micro: on / off. Camera: only in a video call (grey in a voice call, as in Zalo).
  setIcon($('micIcon'), s.muted ? 'micOff' : 'mic');
  $('mute').title = s.muted ? tr('unmute') : tr('mute');
  const camUsable = (!!s.video || !!s.group) && !noCamera && !sharing && !pendingScreen;
  $('cam').disabled = !camUsable;
  $('camCaret').disabled = !(s.video || s.group) || sharing || !!pendingScreen;
  setIcon($('camIcon'), !camUsable ? 'camDisabled' : (camOn ? 'cam' : 'camOff'));
  setIcon($('camCaret').firstElementChild, ((s.video || s.group) && !sharing && !pendingScreen) ? 'caret' : 'caretDisabled');
  $('cam').title = sharing ? tr('sharing') : (noCamera ? tr('noCamera') : (camOn ? tr('cameraOff') : tr('cameraOn')));
  $('camCaret').title = sharing ? tr('sharing') : tr('chooseCamera');
  setIcon($('screenIcon'), sharing ? 'shareOff' : 'share');
  $('screen').title = sharing ? tr('stopScreen') : tr('screen');
  const tilesOn = document.body.classList.contains('has-tiles');
  setIcon($('layout').firstElementChild, tilesOn && groupLayout === 'grid' ? 'conference' : 'grid');
  $('layout').title = tilesOn ? (groupLayout === 'grid' ? tr('speakerLayout') : tr('gridLayout')) : tr('layout');
  $('compactBtn').classList.toggle('hide', !(compactMode || sharing || !!pendingScreen));
  $('compactBtn').textContent = compactMode ? tr('expand') : tr('compact');
  clearInterval(tick);
  const upd = () => { $('timer').textContent = s.since && !ended ? fmt(Date.now() - s.since) : ''; };
  upd(); if (s.since && !ended) tick = setInterval(upd, 500);
  if (s.group) {
    placeSelf();
    updateSelfTile();
  }
}

// Group: our camera is a tile like the others (first), 1-1 it is the 170x96 corner view.
const selfTile = document.createElement('div');
selfTile.className = 'tile novideo';
selfTile.dataset.src = 'self';
selfTile.title = tr('pin');
// The pin mark of a pinned tile (Zalo's pin.svg, through setIcon: the assets' path).
function pinMark() { const img = document.createElement('img'); img.className = 'pinMark'; img.alt = ''; setIcon(img, 'pin'); return img; }
selfTile.appendChild(pinMark());

const selfAvBg = document.createElement('div'); selfAvBg.className = 'tavbg';
const selfAv = document.createElement('div'); selfAv.className = 'tav';
const selfLabel = document.createElement('span'); selfLabel.className = 'label';
const selfName = document.createElement('span'); selfName.className = 'name'; selfName.textContent = tr('self');
const selfMic = document.createElement('img'); selfMic.className = 'micx hide'; selfMic.alt = ''; setIcon(selfMic, 'micMuted');
selfLabel.append(selfName, selfMic);
selfTile.append(selfAvBg, selfAv, selfLabel);

function updateSelfTile() {
  if (!state.group) return;
  const hasVideo = (sharing || (camOn && !noCamera)) && document.body.classList.contains('has-self');
  selfTile.classList.toggle('novideo', !hasVideo);
  if (state.selfAvatar) {
    const url = 'url("' + String(state.selfAvatar).replace(/["\\\n]/g, '') + '")';
    selfAv.style.backgroundImage = url;
    selfAvBg.style.backgroundImage = url;
    selfAv.textContent = '';
  } else {
    selfAv.style.backgroundImage = '';
    selfAvBg.style.backgroundImage = '';
    selfAv.textContent = 'B';
  }
  selfMic.classList.toggle('hide', !state.muted);
}

function placeSelf() {
  const self = $('self');
  const isGroup = !!state.group;
  if (isGroup) {
    if (self.parentNode !== selfTile) { selfTile.insertBefore(self, selfLabel); }
    if (selfTile.parentNode !== $('stage')) { $('stage').prepend(selfTile); }
    updateSelfTile();
  } else {
    if (self.parentNode !== $('pip')) { $('pip').prepend(self); }
    if (selfTile.parentNode) { selfTile.remove(); }
  }
  layoutTiles();
}
// Group layouts of Zalo for macOS (ZCallGroupWindow), measured on its call window
// (screenshots 2026-10-10, 2x, video area 1996x968 px):
//   grid (_alignViewInGridLayout): equal cells of 820x482 px (ratio 1.70), 8-9 px
//     apart, rows filling the height, a last row that is not full centred;
//   conference (_alignViewInConferenceLayout): the main view on the left, the others
//     in a column on the right, 437 px wide (21.9 % of the width), cells of
//     437x285 px (ratio 1.53) from the top, 7 px apart, 16 px from the main view;
//   the bar's button switches them (vidBtnSplitScreenClick); the main view is the
//   pinned member (menuPinPartner) or the one talking (onPartnerSpeakingStateChanged);
//   a landscape video fills its cell, a portrait one (the phone) is shown whole with
//   black sides; a member without video shows its avatar (a circle 35 % of the cell's
//   height on its blurred picture), and every cell its name and a muted micro.
const CELL_RATIO = 820 / 482, THUMB_RATIO = 437 / 285, COLUMN_SHARE = 437 / 1996;
const GRID_GAP = 4, COLUMN_GAP = 8, THUMB_GAP = 4, COLUMN_RIGHT = 2, AVATAR_SHARE = 0.35; // COLUMN_RIGHT: 5 px (2x) right of the column
function tileKey(box) { return box === selfTile ? 'self' : box.dataset.src; }
function videoRatio(box) {
  const c = box.querySelector('canvas');
  return c && c.width > 0 && c.height > 0 ? c.width / c.height : 16 / 9;
}
function place(box, x, y, w, h) {
  box.style.left = Math.round(x) + 'px'; box.style.top = Math.round(y) + 'px';
  box.style.width = Math.max(1, Math.round(w)) + 'px'; box.style.height = Math.max(1, Math.round(h)) + 'px';
  // Fill when the video has the cell's orientation, else whole (macOS: the phone's
  // portrait in a landscape cell has black sides, a landscape camera fills).
  const c = box.querySelector('canvas');
  if (c) c.style.objectFit = (sharing && box === selfTile) ? 'contain' : ((videoRatio(box) >= 1) === (w >= h) ? 'cover' : 'contain');
  const av = box.querySelector('.tav');
  if (av) {
    const d = Math.round(h * AVATAR_SHARE);
    av.style.width = d + 'px';
    av.style.height = d + 'px';
    av.style.fontSize = Math.round(d * 0.4) + 'px';
    const status = box.querySelector('.tstatus');
    if (status) {
      status.style.top = Math.round(h / 2 + d / 2 + Math.max(4, Math.min(8, h * 0.04))) + 'px';
      status.style.fontSize = Math.max(11, Math.min(14, Math.round(d * 0.22))) + 'px';
    }
  }
}
// _calculateColRowFromWindowSize: the column count that gives the largest cell.
function gridLayout(boxes, W, H) {
  const n = boxes.length;
  let best = null;
  for (let cols = 1; cols <= n; cols++) {
    const rows = Math.ceil(n / cols);
    const w = Math.min((W - (cols - 1) * GRID_GAP) / cols, ((H - (rows - 1) * GRID_GAP) / rows) * CELL_RATIO);
    if (w > 0 && (!best || w > best.w + 0.5)) best = { cols, rows, w };
  }
  if (!best) return;
  const { cols, rows, w } = best;
  const h = w / CELL_RATIO;
  const top = (H - (rows * h + (rows - 1) * GRID_GAP)) / 2;
  boxes.forEach((box, i) => {
    const row = Math.floor(i / cols), col = i % cols;
    const inRow = row === rows - 1 ? n - row * cols : cols;
    const left = (W - (inRow * w + (inRow - 1) * GRID_GAP)) / 2;
    place(box, left + col * (w + GRID_GAP), top + row * (h + GRID_GAP), w, h);
  });
}
function conferenceLayout(main, rest, W, H) {
  if (!rest.length) { place(main, 0, 0, W, H); return; }
  const colW = Math.round(Math.max(150, Math.min(320, W * COLUMN_SHARE)));
  let tw = colW, th = colW / THUMB_RATIO;
  const need = rest.length * th + (rest.length - 1) * THUMB_GAP;
  if (need > H) { th = (H - (rest.length - 1) * THUMB_GAP) / rest.length; tw = th * THUMB_RATIO; }
  const colX = W - COLUMN_RIGHT - colW;
  rest.forEach((box, i) => place(box, colX + (colW - tw) / 2, i * (th + THUMB_GAP), tw, th));
  place(main, 0, 0, colX - COLUMN_GAP, H);
}
function layoutTiles() {
  const stage = $('stage');
  const boxes = Array.from(stage.children);
  if (!boxes.length) return;
  const W = stage.clientWidth, H = stage.clientHeight;
  if (W <= 0 || H <= 0) return;
  const conference = groupLayout === 'conference' && boxes.length > 1;
  document.body.classList.toggle('conference', conference);
  let main = null;
  if (conference) {
    const want = pinned || talking;
    main = boxes.find((b) => tileKey(b) === want) || boxes.find((b) => b !== selfTile) || boxes[0];
  }
  for (const b of boxes) {
    b.classList.toggle('main', b === main);
    b.classList.toggle('pinned', pinned !== null && tileKey(b) === pinned);
  }
  if (conference) conferenceLayout(main, boxes.filter((b) => b !== main), W, H);
  else gridLayout(boxes, W, H);
}
window.addEventListener('resize', layoutTiles);
$('stage').addEventListener('click', (e) => {
  const box = e.target.closest('.tile');
  if (!box) return;
  const key = tileKey(box);
  // Pin: that tile in the main view (the conference layout); again: unpin.
  if (pinned === key) pinned = null;
  else { pinned = key; groupLayout = 'conference'; }
  act('log', { text: 'group layout ' + groupLayout + (pinned ? ', pinned ' + pinned : '') });
  render();
  layoutTiles();
});
ipcRenderer.on('zcall-ui-speaking', (_e, m) => {
  const key = String(m.src); // tiles are keyed by the src as text
  const t = tiles.get(key);
  if (t) t.box.classList.toggle('speaking', !!m.on);
  if (m.on) talking = key;
  else if (talking === key) talking = null;
  if (groupLayout === 'conference' && !pinned) layoutTiles();
});
// One tile per member: video canvas, avatar (no video yet, or camera off), name and
// muted micro (macOS PartnerUI), pin mark. Made by the member list or the first frame.
function ensureTile(key) {
  let t = tiles.get(key);
  if (t) return t;
  const c = document.createElement('canvas');
  const box = document.createElement('div');
  box.className = 'tile novideo';
  box.dataset.src = key;
  box.title = tr('pin');
  const avbg = document.createElement('div'); avbg.className = 'tavbg';
  const av = document.createElement('div'); av.className = 'tav';
  const status = document.createElement('div'); status.className = 'tstatus';
  const label = document.createElement('span'); label.className = 'label';
  const name = document.createElement('span'); name.className = 'name';
  const mic = document.createElement('img'); mic.className = 'micx hide'; mic.alt = ''; setIcon(mic, 'micMuted');
  label.append(name, mic);
  box.append(avbg, av, status, c, label, pinMark());
  $('stage').appendChild(box);
  t = { box, canvas: c, g: c.getContext('2d'), dec: null, codec: null, ts: 0, errLogged: false, sentW: 0, ro: null, rt: 0,
    name, mic, av, avbg, status, avatar: '', camOff: false, hasFrame: false };
  tiles.set(key, t);
  // The engine asks the server for the layer that fits this tile (macOS does the same
  // from the tile's render width): tell it the width in pixels, and again when it changes.
  const reportWidth = () => {
    t.rt = 0;
    const w = Math.round(box.getBoundingClientRect().width * (window.devicePixelRatio || 1));
    if (w > 0 && w !== t.sentW) { t.sentW = w; act('tile', { src: Number(key), width: w }); }
  };
  t.ro = new ResizeObserver(() => { if (!t.rt) t.rt = setTimeout(reportWidth, 300); });
  t.ro.observe(box);
  if (state.group) document.body.classList.add('has-tiles');
  placeSelf();
  return t;
}
function tileVideoState(t) {
  t.box.classList.toggle('novideo', t.camOff || !t.hasFrame);
}
function dropTile(key) {
  const t = tiles.get(key);
  if (!t) return;
  if (t.dec && t.dec.state !== 'closed') try { t.dec.close(); } catch (_) {}
  if (t.ro) t.ro.disconnect();
  if (t.rt) clearTimeout(t.rt);
  t.box.remove();
  tiles.delete(key);
  if (pinned === key) pinned = null;
  if (talking === key) talking = null;
  if (!tiles.size) document.body.classList.remove('has-tiles', 'conference');
  placeSelf();
}
// The engine's member list (state.members): a tile for everyone in the call or being
// rung, name, avatar, micro and camera state; tiles of those who left go.
function syncMembers(list) {
  if (!Array.isArray(list)) return;
  const keep = new Set();
  for (const m of list) {
    const key = String(m.src);
    keep.add(key);
    const t = ensureTile(key);
    t.fromList = true;
    t.name.textContent = m.name || tr('member');
    t.memberState = m.state;
    t.status.textContent = ['calling', 'ringing', 'busy', 'declined'].includes(m.state) ? tr(m.state) : '';
    t.mic.classList.toggle('hide', !m.muted);
    t.camOff = !!m.camOff;
    if ((m.avatar || '') !== t.avatar) {
      t.avatar = m.avatar || '';
      const url = t.avatar ? 'url("' + t.avatar.replace(/["\\\n]/g, '') + '")' : '';
      t.av.style.backgroundImage = url; t.avbg.style.backgroundImage = url;
      t.av.textContent = t.avatar ? '' : (m.name || '?').trim().charAt(0).toUpperCase();
    }
    tileVideoState(t);
  }
  for (const [key] of [...tiles]) if (!keep.has(key)) dropTile(key);
  layoutTiles();
}
new MutationObserver(placeSelf).observe(document.body, { attributes: true, attributeFilter: ['class'] });

// Local preview is decoded from Rust's native camera/screen capture. Capture,
// H.264 encoding and network transmission never run in the renderer.
const self = $('self');
const sg = self.getContext('2d');

function stopLocal() {
  if (localDecoder && localDecoder.state !== 'closed') {
    try { localDecoder.close(); } catch (_) {}
  }
  localDecoder = null;
  localCodec = null;
  localTs = 0;
  self.width = 1;
  self.height = 1;
  document.body.classList.remove('has-self');
  if (state.group) updateSelfTile();
}

function decodeLocalVideo(m) {
  if (state.phase !== 'connected' || (!sharing && !camOn)) return;
  const codec = m.codec || 'avc1.42E01E';
  if (m.key && (!localDecoder || localDecoder.state === 'closed' || codec !== localCodec)) {
    if (localDecoder && localDecoder.state !== 'closed') {
      try { localDecoder.close(); } catch (_) {}
    }
    localCodec = codec;
    const mine = new VideoDecoder({
      output: (frame) => {
        if (localDecoder !== mine) { frame.close(); return; }
        if (self.width !== frame.displayWidth || self.height !== frame.displayHeight) {
          self.width = frame.displayWidth;
          self.height = frame.displayHeight;
        }
        sg.drawImage(frame, 0, 0);
        document.body.classList.add('has-self');
        if (state.group) updateSelfTile();
        frame.close();
      },
      error: (error) => {
        if (localDecoder !== mine) return;
        localDecoder = null;
        act('log', { text: 'local video decode: ' + ((error && error.message) || error) });
        act('keyframe');
      },
    });
    localDecoder = mine;
    try {
      mine.configure({ codec, optimizeForLatency: true, hardwareAcceleration: 'prefer-software' });
    } catch (error) {
      localDecoder = null;
      try { mine.close(); } catch (_) {}
      act('log', { text: 'local video configure: ' + ((error && error.message) || error) });
      return;
    }
  }
  if (!localDecoder || localDecoder.state !== 'configured') return;
  if (localDecoder.decodeQueueSize >= 3 && !m.key) return;
  try {
    localDecoder.decode(new EncodedVideoChunk({
      type: m.key ? 'key' : 'delta',
      timestamp: localTs += 33333,
      data: annexB(m),
    }));
  } catch (error) {
    const failed = localDecoder;
    localDecoder = null;
    try { if (failed && failed.state !== 'closed') failed.close(); } catch (_) {}
    act('log', { text: 'local video decode: ' + ((error && error.message) || error) });
    act('keyframe');
  }
}

function startScreen(source = null) {
  if (sharing || pendingScreen) return;
  pendingScreen = true;
  autoCompact = !compactMode;
  const action = { on: true };
  if (source) action.source = source;
  act('screen', action);
  render();
}

function stopScreen() {
  if (!sharing && !pendingScreen) return;
  sharing = false;
  pendingScreen = false;
  stopLocal();
  act('screen', { on: false });
  render();
}

ipcRenderer.on('zcall-ui-state', (_e, s) => {
  const before = state.phase;
  state = s || {};
  if (typeof state.cameraOn === 'boolean') camOn = state.cameraOn;
  if (typeof state.sharing === 'boolean') sharing = state.sharing;
  if (state.phase === 'ended') {
    stopVideo();
    stopLocal();
    sharing = false;
    pendingScreen = false;
    camOn = false;
  } else if (!state.video && !state.group) {
    stopLocal();
  } else if (!sharing && !camOn) {
    stopLocal();
  }
  if (state.group) {
    document.body.classList.add('has-tiles');
    placeSelf();
    if (state.members) syncMembers(state.members);
  }
  if (Number.isFinite(state.micVolume)) micVolumeShown(state.micVolume);
  if (typeof state.micAutoGain === 'boolean') micAutoModeShown(state.micAutoGain);
  sounds(before);
  render();
});

ipcRenderer.on('zcall-ui-local-video', (_e, frame) => decodeLocalVideo(frame));
ipcRenderer.on('zcall-ui-camera-error', (_e, error) => {
  camOn = false;
  noCamera = true;
  stopLocal();
  act('log', { text: 'camera capture failed: ' + ((error && error.message) || 'unknown error') });
  render();
});
ipcRenderer.on('zcall-ui-screen-started', (_e, message) => {
  sharing = true;
  pendingScreen = false;
  stopLocal();
  if (message && typeof message.cameraWasOn === 'boolean') camOn = message.cameraWasOn;
  render();
});
ipcRenderer.on('zcall-ui-screen-stopped', () => {
  sharing = false;
  pendingScreen = false;
  stopLocal();
  if (autoCompact) {
    autoCompact = false;
    ipcRenderer.send('zcall-ui-window', 'expand');
  }
  if (camOn) act('keyframe');
  render();
});
ipcRenderer.on('zcall-ui-screen-error', (_e, error) => {
  sharing = false;
  pendingScreen = false;
  stopLocal();
  act('log', { text: 'screen share failed: ' + ((error && error.message) || error) });
  if (autoCompact) {
    autoCompact = false;
    ipcRenderer.send('zcall-ui-window', 'expand');
  }
  if (camOn) act('keyframe');
  render();
});
ipcRenderer.on('zcall-ui-screen-sources', (_e, windows) => {
  openMenu('share-windows', $('screen'), windows).catch(closeMenu);
});
ipcRenderer.on('zcall-ui-screen-source-mode', (_e, source) => startScreen(source || 'window'));

$('cam').onclick = () => {
  if (noCamera || sharing || pendingScreen) return;
  camOn = !camOn;
  act('camera', { on: camOn });
  if (!camOn) stopLocal();
  updateSelfTile();
  render();
};
$('screen').onclick = (e) => {
  e.stopPropagation();
  if (sharing || pendingScreen) { stopScreen(); return; }
  openMenu('share', $('screen')).catch(() => startScreen(null));
};

// Received video: H.264 Annex-B frames decoded with WebCodecs, drawn on the canvas.
// 1-1 is one stream on #video. A group call is one decoder per member (src):
// two people sharing one decoder errors after a few frames and the last
// picture stays up.
const canvas = $('video'); const g = canvas.getContext('2d');
// (dec, decCodec, ts, tiles, etc. declared at top)
// 1-1: the decoder broke (or a decode call threw): say so, and ask the phone for a key frame
// (the engine sends the PLI), instead of staying on the last picture until the next one.
function decoderBroke(why, failed) {
  if (failed && failed !== dec) return;
  dec = null;
  if (!decErrLogged) { decErrLogged = true; act('log', { text: 'video decode: ' + why }); }
  act('needkey', { why: 'decoder' });
}
function stopVideo() {
  if (dec && dec.state !== 'closed') try { dec.close(); } catch (_) {}
  dec = null; decCodec = null;
  decErrLogged = false; shown = 0; shownSince = 0; shownTotal = 0;
  if (state.phase === 'ended' || !state.group) {
    for (const key of [...tiles.keys()]) dropTile(key);
    pinned = null; talking = null; groupLayout = 'grid';
    document.body.classList.remove('has-video', 'has-tiles', 'conference');
    placeSelf();
    $('stage').textContent = '';
  }
}
const DECODE_QUEUE_MAX = 3; // past this, another delta only makes the smear later
function frameBytes(m) {
  const d = m && m.data;
  if (d && typeof d !== 'string') return d.byteLength || d.length || 0;
  return Math.floor(String(d || '').length * 3 / 4);
}
function annexB(m) {
  const d = m && m.data;
  if (d && typeof d !== 'string') return d instanceof Uint8Array ? d : new Uint8Array(d.buffer, d.byteOffset, d.byteLength);
  const bin = atob(d || ''); const data = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) data[i] = bin.charCodeAt(i);
  return data;
}
function newDecoder(codec) {
  if (dec && dec.state !== 'closed') try { dec.close(); } catch (_) {}
  decCodec = codec;
  const mine = new VideoDecoder({
    output: (f) => {
      if (dec !== mine) { f.close(); return; }
      if (canvas.width !== f.displayWidth || canvas.height !== f.displayHeight) {
        canvas.width = f.displayWidth; canvas.height = f.displayHeight;
        act('log', { text: 'video ' + f.displayWidth + 'x' + f.displayHeight });
      }
      g.drawImage(f, 0, 0);
      const now = Date.now(); shown++; shownTotal++;
      if (!shownSince) shownSince = now;
      if (now - shownSince >= 5000) {
        act('log', { text: 'video shown ' + (shown * 1000 / (now - shownSince)).toFixed(1) + ' fps, ' + f.displayWidth + 'x' + f.displayHeight + ', ' + shownTotal + ' frames, ' +
          Math.round(rxBytes * 8 / ((now - shownSince) / 1000) / 1000) + ' kbps in, decode queue ' + (dec ? dec.decodeQueueSize : '-') + ', shown at ' + canvas.clientWidth + 'x' + canvas.clientHeight + ' css px' });
        shown = 0; shownSince = now; rxBytes = 0;
      }
      f.close();
      if (!state.peerCamOff) document.body.classList.add('has-video');
    },
    error: (e) => decoderBroke((e && e.message) || String(e), mine), // the next key frame restarts it
  });
  dec = mine;
  // Software: the hardware path refuses these streams on this Electron.
  dec.configure({ codec, optimizeForLatency: true, hardwareAcceleration: 'prefer-software' });
}
function showMember(m) {
  const t = ensureTile(String(m.src));
  t.rxBytes = (t.rxBytes || 0) + frameBytes(m);
  if (t.dec && t.dec.state === 'configured' && t.dec.decodeQueueSize >= DECODE_QUEUE_MAX) {
    if (!m.key) { act('needkey', { why: 'queue', src: m.src }); return; }
    try { t.dec.close(); } catch (_) {}
    t.dec = null;
  }
  if (m.key && (!t.dec || t.dec.state === 'closed' || m.codec !== t.codec)) {
    if (t.dec && t.dec.state !== 'closed') try { t.dec.close(); } catch (_) {}
    t.codec = m.codec;
    const src = m.src;
    const mine = new VideoDecoder({
      output: (f) => {
        if (t.dec !== mine) { f.close(); return; }
        if (t.canvas.width !== f.displayWidth || t.canvas.height !== f.displayHeight) {
          t.canvas.width = f.displayWidth; t.canvas.height = f.displayHeight;
          act('log', { text: 'video ' + src + ' ' + f.displayWidth + 'x' + f.displayHeight + ' (tile ' + t.sentW + ' px)' });
          layoutTiles(); // fill or whole, by the video's orientation
        }
        t.g.drawImage(f, 0, 0);
        if (!t.hasFrame) { t.hasFrame = true; tileVideoState(t); }
        const now = Date.now(); t.shown = (t.shown || 0) + 1;
        if (!t.shownSince) t.shownSince = now;
        if (now - t.shownSince >= 5000) {
          act('log', { text: 'video shown ' + src + ' ' + (t.shown * 1000 / (now - t.shownSince)).toFixed(1) + ' fps, ' + f.displayWidth + 'x' + f.displayHeight +
            ', ' + Math.round((t.rxBytes || 0) * 8 / ((now - t.shownSince) / 1000) / 1000) + ' kbps in, decode queue ' + (t.dec ? t.dec.decodeQueueSize : '-') + ', tile ' + t.sentW + ' px' });
          t.shown = 0; t.shownSince = now; t.rxBytes = 0;
        }
        f.close();
        document.body.classList.add('has-video');
      },
      error: (e) => {
        if (t.dec !== mine) return;
        t.dec = null;
        if (!t.errLogged) { t.errLogged = true; act('log', { text: 'video decode ' + src + ': ' + (e && e.message ? e.message : e) }); }
        act('needkey', { why: 'decoder', src });
      },
    });
    t.dec = mine;
    t.dec.configure({ codec: m.codec, optimizeForLatency: true, hardwareAcceleration: 'prefer-software' });
  }
  if (!t.dec || t.dec.state !== 'configured') return;
  try { t.dec.decode(new EncodedVideoChunk({ type: m.key ? 'key' : 'delta', timestamp: t.ts += 66666, data: annexB(m) })); }
  catch (e) {
    t.dec = null;
    if (!t.errLogged) { t.errLogged = true; act('log', { text: 'video decode ' + m.src + ': ' + (e && e.message ? e.message : e) }); }
    act('needkey', { why: 'decoder', src: m.src });
  }
}
ipcRenderer.on('zcall-ui-video', (_e, m) => {
  if (state.phase !== 'connected') return;
  if (m.src) { showMember(m); return; }
  rxBytes += frameBytes(m);
  if (dec && dec.state === 'configured' && dec.decodeQueueSize >= DECODE_QUEUE_MAX) {
    if (!m.key) { act('needkey', { why: 'queue' }); return; }
    try { dec.close(); } catch (_) {}
    dec = null;
  }
  if (m.key && (!dec || dec.state === 'closed' || m.codec !== decCodec)) newDecoder(m.codec);
  if (!dec || dec.state !== 'configured') return;
  try { dec.decode(new EncodedVideoChunk({ type: m.key ? 'key' : 'delta', timestamp: ts += 66666, data: annexB(m) })); } catch (e) { decoderBroke((e && e.message) || String(e), dec); }
});

// --- device menu (▲ on the pills, the gear): devices come from the Rust engine ---
const menu = $('menu');
let screenWindows = [];
ipcRenderer.invoke('zcall-ui-devices').then((d) => {
  camPref = d.prefs.camera || null;
  curCamId = camPref && camPref.id || '';
  noCamera = !Array.isArray(d.cameras) || d.cameras.length === 0;
  render();
}).catch(() => {});
async function cameras() {
  const d = await ipcRenderer.invoke('zcall-ui-devices');
  camPref = d.prefs.camera || null;
  curCamId = camPref && camPref.id || '';
  const items = Array.isArray(d.cameras) ? d.cameras : [];
  noCamera = items.length === 0;
  return items;
}
function closeMenu() {
  menu.classList.remove('show'); menu.textContent = ''; menu.dataset.kind = ''; menu.style.left = '';
  document.querySelectorAll('.caret.open').forEach((b) => b.classList.remove('open'));
}
// Sections: mic -> micro + loa (as Zalo's micro menu), camera -> camera, all (gear) -> the three.
async function sections(kind) {
  const out = [];
  if (kind === 'mic' || kind === 'all') {
    const d = await ipcRenderer.invoke('zcall-ui-devices');
    for (const k of ['mic', 'speaker']) {
      const list = k === 'mic' ? d.mics : d.speakers;
      out.push({
        kind: k, title: k === 'mic' ? tr('chooseMic') : tr('chooseSpeaker'), icon: k === 'mic' ? 'menuMic' : 'menuSpeaker',
        items: list.concat([{ id: '', label: tr('defaultDevice') }]),
        current: list.some((x) => x.id === d.prefs[k]) ? d.prefs[k] : '',
      });
    }
  }
  if ((kind === 'camera' && !sharing) || (kind === 'all' && state.video && !sharing)) {
    const items = await cameras();
    out.push({ kind: 'camera', title: tr('chooseCamera'), icon: 'menuCam', items, current: curCamId || (items[0] && items[0].id) });
  }
  if (kind === 'share') {
    out.push({
      kind: 'share',
      noHeader: true,
      items: [
        { id: 'screen:0', label: tr('chooseScreen') },
        { id: 'window:select', label: tr('chooseWindow') },
      ],
    });
  }
  if (kind === 'share-windows') {
    out.push({ kind: 'share-windows', noHeader: true, items: screenWindows });
  }
  return out;
}
async function openMenu(kind, anchor, suppliedWindows) {
  if (kind === 'share-windows' && Array.isArray(suppliedWindows)) screenWindows = suppliedWindows;
  if (kind === 'camera' && sharing) return;
  if (menu.dataset.kind === kind) { closeMenu(); return; }
  const secs = await sections(kind);
  menu.textContent = '';
  secs.forEach((sec, i) => {
    if (i) { const l = document.createElement('div'); l.className = 'sep'; menu.appendChild(l); }
    if (!sec.noHeader && sec.title) {
      const h = document.createElement('div'); h.className = 'h';
      h.dataset.kind = sec.kind;
      const hi = document.createElement('img'); setIcon(hi, sec.icon);
      h.append(hi, sec.title); menu.appendChild(h);
      if (!sec.items.length) { const r = document.createElement('div'); r.className = 'h'; r.textContent = tr('noDevices'); menu.appendChild(r); }
    }
    if (sec.noHeader && !sec.items.length) {
      const r = document.createElement('div'); r.className = 'h';
      r.textContent = sec.kind === 'share-windows' ? tr('noWindows') : tr('empty');
      menu.appendChild(r);
    }
    for (const it of sec.items) {
      const r = document.createElement('div'); r.className = 'it'; r.title = it.label;
      if (it.id === sec.current) { const ck = document.createElement('img'); setIcon(ck, 'check'); r.appendChild(ck); }
      r.append(it.label);
      r.onclick = (e) => { e.stopPropagation(); closeMenu(); pick(sec.kind, it); };
      menu.appendChild(r);
    }
    if (sec.kind === 'mic' && Number.isFinite(state.micVolume)) menu.appendChild(micVolumeRow());
  });
  if (kind === 'share') {
    menu.style.left = '8px';
    menu.style.width = '180px';
  } else if (kind === 'share-windows') {
    menu.style.left = '8px';
    menu.style.width = '240px';
  } else {
    menu.style.width = '';
    const r = anchor.getBoundingClientRect();
    const right = Math.min(window.innerWidth - 8, Math.max(318, r.right + 19));
    menu.style.left = (right - 310) + 'px';
  }
  menu.dataset.kind = kind;
  menu.classList.add('show');
  document.querySelectorAll('.caret').forEach((b) => b.classList.toggle('open', b.dataset.kind === kind));
}
// Microphone level, set during the call (the engine moves the PulseAudio source;
// dragging the slider switches to manual mode, and ticking the Auto switch
// re-enables automatic gain adjustment). The meter is the loudest 100 ms of
// what we send: red from -9 dBFS, where a voice starts to clip.
const MIC_HOT_DBOV = 9;
// micSlider, micMeter, micPct, micAutoSwitch, micModeBadge, etc. declared at top
function micVolumeRow() {
  const isAuto = state.micAutoGain !== false;
  const row = document.createElement('div'); row.className = 'vol';
  const title = document.createElement('div'); title.className = 'volTitle';
  const right = document.createElement('span'); right.className = 'volRight';
  micModeBadge = document.createElement('span');
  micPct = document.createElement('span'); micPct.className = 'volPct';
  right.append(micModeBadge, micPct);
  title.append(tr('micVolume'), right);
  micSlider = document.createElement('input');
  Object.assign(micSlider, { type: 'range', min: 0, max: 100, step: 1, value: state.micVolume });
  micSlider.setAttribute('aria-label', tr('micVolume'));
  micMeter = document.createElement('div'); micMeter.className = 'meter';
  const fill = document.createElement('div'); fill.className = 'fill'; micMeter.appendChild(fill);

  const autoRow = document.createElement('label'); autoRow.className = 'volAuto';
  const autoLabel = document.createElement('span'); autoLabel.textContent = tr('micAutoGain');
  const toggleWrap = document.createElement('span'); toggleWrap.className = 'volToggle';
  micAutoSwitch = document.createElement('input');
  micAutoSwitch.type = 'checkbox';
  micAutoSwitch.checked = isAuto;
  micAutoSwitch.setAttribute('aria-label', tr('micAutoGain'));
  const track = document.createElement('span'); track.className = 'volTrack';
  toggleWrap.append(micAutoSwitch, track);
  autoRow.append(autoLabel, toggleWrap);
  micAutoSwitch.onchange = () => {
    const on = !!micAutoSwitch.checked;
    micAutoModeShown(on);
    act('micAutoGain', { on });
  };

  const hint = document.createElement('div'); hint.className = 'volHint';
  hint.textContent = tr('micHint');
  micPct.textContent = state.micVolume + '%';
  micAutoModeShown(isAuto);
  let last = null; // one message per value: input and change both end here
  const send = () => { const v = Number(micSlider.value); if (v === last) return; last = v; micSentAt = Date.now(); act('micVolume', { value: v }); };
  micSlider.oninput = () => {
    micDragging = true; micPct.textContent = micSlider.value + '%';
    micAutoModeShown(false);
    clearTimeout(micTimer);
    if (Date.now() - micSentAt >= 150) send(); else micTimer = setTimeout(send, 150);
  };
  micSlider.onchange = () => { clearTimeout(micTimer); send(); micDragging = false; };
  row.onclick = (e) => e.stopPropagation();
  row.append(title, micSlider, micMeter, autoRow, hint);
  return row;
}
function micVolumeShown(pct) {
  if (!micSlider || !document.body.contains(micSlider) || micDragging) return;
  micSlider.value = pct; micPct.textContent = pct + '%';
}
function micAutoModeShown(on) {
  const isAuto = !!on;
  state.micAutoGain = isAuto;
  if (micAutoSwitch && document.body.contains(micAutoSwitch)) micAutoSwitch.checked = isAuto;
  if (micModeBadge && document.body.contains(micModeBadge)) {
    micModeBadge.className = 'volBadge ' + (isAuto ? 'auto' : 'manual');
    micModeBadge.textContent = tr(isAuto ? 'micModeAuto' : 'micModeManual');
  }
}
ipcRenderer.on('zcall-ui-mic-level', (_e, level) => {
  if (!micMeter || !document.body.contains(micMeter)) return;
  const dbov = Math.max(0, Math.min(60, Number(level)));
  const fill = micMeter.firstChild;
  fill.style.width = Math.round((60 - dbov) / 60 * 100) + '%';
  fill.classList.toggle('hot', dbov <= MIC_HOT_DBOV);
});

function pick(kind, it) {
  if (kind === 'share') {
    if (it.id === 'window:select') {
      act('listScreenSources');
      return;
    }
    startScreen('monitor');
    return;
  }
  if (kind === 'share-windows') {
    startScreen(it.id);
    return;
  }
  if (kind === 'camera' && sharing) return;
  act('device', { kind, id: it.id, label: it.label });
  if (kind !== 'camera') return;
  camPref = { id: it.id, label: it.label };
  curCamId = it.id;
  noCamera = false;
}
document.querySelectorAll('.caret').forEach((b) => {
  b.onclick = (e) => { e.stopPropagation(); if (!b.disabled) openMenu(b.dataset.kind, b.parentElement).catch(closeMenu); };
});
$('settings').onclick = (e) => { e.stopPropagation(); openMenu('all', $('settings')).catch(closeMenu); };
document.addEventListener('click', (e) => { if (!menu.contains(e.target)) closeMenu(); });
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && menu.dataset.kind) closeMenu();
  else if (e.key === 'Escape' && fullScreen) ipcRenderer.send('zcall-ui-window', 'leave-fullscreen');
  else if (e.key === 'F11') { e.preventDefault(); toggleFull(); }
});
render();
