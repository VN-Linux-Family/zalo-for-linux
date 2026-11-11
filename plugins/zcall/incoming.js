/**
 * plugins/zcall/incoming.js
 *
 * The incoming call notice (ui/incoming.html): a small frameless window in
 * the top right corner of the screen, as Zalo's macOS one, instead of the
 * call window. window.js shows it for phase 'incoming' and closes it when
 * the call is answered, declined or over; answering opens the call window.
 */

'use strict';

const path = require('path');
const localeMessages = require('./ui/locale.json').messages;
let locale = 'vi';
const translate = (key) => { const entry = localeMessages['zcall.' + key]; return entry ? entry[locale] : key; };

// Measured on Zalo for macOS (CSS px = pt): 344 wide, 232 for a voice call,
// 290 with "Trả lời không mở camera"; flush with the top of the work area,
// 10 from its right edge.
const WIDTH = 344;
const HEIGHT_VOICE = 232;
const HEIGHT_VIDEO = 290;
const MARGIN_RIGHT = 10;

let win = null;
let ready = false;
let pending = null;

function size(state) {
  return { width: WIDTH, height: state.video && !state.noAnswer ? HEIGHT_VIDEO : HEIGHT_VOICE };
}

function place(state) {
  const { screen } = require('electron');
  const wa = screen.getDisplayNearestPoint(screen.getCursorScreenPoint()).workArea;
  const s = size(state);
  return { x: wa.x + wa.width - s.width - MARGIN_RIGHT, y: wa.y, width: s.width, height: s.height };
}

/** Shows (or updates) the notice for an incoming state. */
function show(state, assetsDir) {
  const { BrowserWindow } = require('electron');
  pending = state;
  if (win && !win.isDestroyed()) {
    win.setBounds(place(state));
    if (ready) win.webContents.send('zcall-incoming-state', state);
    return;
  }
  ready = false;
  const notice = win = new BrowserWindow({
    ...place(state),
    useContentSize: true,
    frame: false,
    resizable: false,
    maximizable: false,
    minimizable: false,
    fullscreenable: false,
    alwaysOnTop: false, // set once shown, see below
    skipTaskbar: false,
    show: false,
    title: 'Zalo: ' + translate('incoming'),
    backgroundColor: '#3989ff',
    // Local static page; the only remote content is the avatar image.
    webPreferences: { contextIsolation: false, nodeIntegration: true },
  });
  notice.zcallWindow = true; // main.js: closing it declines, it does not hide to the tray
  notice.setMenuBarVisibility(false);
  notice.webContents.on('did-finish-load', () => {
    // The caller may have cancelled while the page was still loading.
    if (win !== notice || notice.isDestroyed()) return;
    ready = true;
    if (pending) notice.webContents.send('zcall-incoming-state', pending);
    notice.webContents.send('zcall-ui-language', locale);
    notice.showInactive();
    // alwaysOnTop of the constructor does not reach a window shown later (X11:
    // no _NET_WM_STATE_ABOVE, the notice stayed under Zalo's main window).
    notice.setAlwaysOnTop(true);
    notice.moveTop();
    notice.flashFrame(true);
  });
  notice.on('closed', () => {
    if (win !== notice) return;
    win = null; ready = false; pending = null;
  });
  notice.loadFile(path.join(__dirname, 'ui', 'incoming.html'), { query: { assets: assetsDir } });
}

/** Closes the notice (answered, declined, cancelled). */
function close() {
  const notice = win;
  const wasReady = ready;
  win = null; ready = false; pending = null;
  if (!notice || notice.isDestroyed()) return;
  if (wasReady) notice.webContents.send('zcall-incoming-stop');
  notice.destroy();
}

function setLocale(nextLocale) {
  if (nextLocale !== 'vi' && nextLocale !== 'en') return false;
  locale = nextLocale;
  if (win && !win.isDestroyed()) {
    win.setTitle('Zalo: ' + translate('incoming'));
    if (ready) win.webContents.send('zcall-ui-language', locale);
  }
  return true;
}

function isOpen() {
  return !!(win && !win.isDestroyed());
}

module.exports = { show, close, isOpen, setLocale };
