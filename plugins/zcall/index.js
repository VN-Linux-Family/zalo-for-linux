/**
 * plugins/zcall/index.js
 *
 * Linux call frontend host. Zalo owns and starts the native helper; this
 * plugin only opens the existing call window and its authenticated TCP socket.
 */

'use strict';

const fs = require('fs');
const path = require('path');
const callWindow = require('./window');

function zaloMainJs() {
  return [
    path.join(path.dirname(process.execPath), 'app', 'main-dist', 'main.js'),
    path.join(__dirname, '..', '..', 'app', 'main-dist', 'main.js')
  ].find((file) => fs.existsSync(file));
}

function configure(app) {
  const isWayland = (process.env.XDG_SESSION_TYPE || '').trim().toLowerCase() === 'wayland';
  if (!isWayland) return;
  if (!process.env.WAYLAND_DISPLAY) process.env.WAYLAND_DISPLAY = 'wayland-0';
  const features = app.commandLine.getSwitchValue('enable-features').split(',').filter(Boolean);
  if (!features.includes('WebRTCPipeWireCapturer')) features.push('WebRTCPipeWireCapturer');
  app.commandLine.appendSwitch('enable-features', features.join(','));
}

/** Start only the frontend listener; Zalo starts and supervises app/native/zcall/zcall. */
async function start({ userDataDir }) {
  if (process.env.TMPDIR) {
    try { fs.mkdirSync(process.env.TMPDIR, { recursive: true }); } catch (_) {}
  }
  const uiReady = callWindow.start();
  Object.assign(process.env, {
    ZALO_USER_DATA_DIR: userDataDir,
    ZCALL_MEDIA: 'zrtc',
    ZCALL_SEND_UDP: '1',
    ZCALL_LOG_DIR: userDataDir
  });
  const mainJs = process.env.ZALO_MAIN_JS || zaloMainJs();
  if (mainJs) process.env.ZALO_MAIN_JS = mainJs;
  await uiReady;
}

/** Zalo owns the native helper lifecycle; this plugin only closes its UI. */
function stop() {
  callWindow.stop();
}

function setLocale(locale) {
  if (locale !== 'vi' && locale !== 'en') return false;
  return callWindow.setLocale(locale);
}

function onLanguageUpdateRequest(request) {
  if (!request || request.type !== 'LANGUAGE_UPDATE_REQUEST') return false;
  return setLocale(request.locale);
}

// This plugin is required by main.js before Zalo's bootstrap. Prepare Chromium
// capture switches and bind the authenticated UI socket at module load, rather
// than waiting for app 'ready' (by then Zalo may already have launched the
// native helper). main.js may still call configure/start at 'ready'; both are
// idempotent and start() reuses the existing listener promise.
const { app } = require('electron');
configure(app);
const startup = start({ userDataDir: app.getPath('userData') });
startup.catch((error) => {
  console.error('[zcall] call UI listener failed to start:', error);
});

module.exports = { configure, start, stop, setLocale, onLanguageUpdateRequest };
