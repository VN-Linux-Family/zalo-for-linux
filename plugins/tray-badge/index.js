/**
 * plugins/tray-badge/index.js
 *
 * Unread count on the tray icon. The StatusNotifierItem protocol used by KDE
 * and GNOME trays has no numeric-badge API, so the count is drawn onto the
 * icon image itself (badge-icon.js).
 *
 * The tray is the one main.js creates: Zalo builds its own tray only on
 * Windows. The count comes from app.setBadgeCount, which both Zalo and the
 * launcher-badge plugin call with the unread total.
 */

'use strict';

const fs = require('fs');
const path = require('path');
const { renderTrayBadge, decodePng } = require('./badge-icon');

// Square Zalo logos shipped in the app, in preference order; the first one
// that decodes as a plain 8-bit PNG is drawn on. Sourced from the app so
// Zalo's artwork stays out of this repo.
const BASE_CANDIDATES = [
  'pc-dist/favicon-256x256.png',
  'pc-dist/android-icon-192x192.png',
  'pc-dist/apple-icon-180x180.png',
  'pc-dist/favicon-512x512.png',
];

let _tray = null;
let _nativeImage = null;
let _iconPath = null;
let _baseBuffer = null;
let _lastCount = 0;

function register({ app, tray, nativeImage, appDir, iconPath }) {
  if (process.platform !== 'linux' || !tray) return;

  _baseBuffer = readBaseIcon(appDir);
  if (!_baseBuffer) {
    console.warn('tray-badge: no usable base icon, tray badge disabled');
    return;
  }

  _tray = tray;
  _nativeImage = nativeImage;
  _iconPath = iconPath;

  const original = typeof app.setBadgeCount === 'function' ? app.setBadgeCount.bind(app) : null;
  app.setBadgeCount = (count) => {
    try { setCount(count); } catch (e) { console.error('tray-badge: update failed', e); }
    return original ? original(count) : false;
  };
}

function readBaseIcon(appDir) {
  for (const rel of BASE_CANDIDATES) {
    try {
      const buffer = fs.readFileSync(path.join(appDir, rel));
      decodePng(buffer);
      return buffer;
    } catch (_) {
      // Missing, or an encoding the decoder does not handle; try the next.
    }
  }
  return null;
}

function setCount(rawCount) {
  const count = normalizeCount(rawCount);
  if (count === _lastCount) return;
  _lastCount = count;

  // main.js destroys the tray on quit, before the final zero count arrives.
  if (!_tray || _tray.isDestroyed()) return;

  _tray.setImage(count > 0
    ? _nativeImage.createFromBuffer(renderTrayBadge(_baseBuffer, count))
    : _iconPath);
}

function normalizeCount(rawCount) {
  const count = Number.parseInt(rawCount, 10);
  if (!Number.isFinite(count) || count < 1) return 0;
  return Math.min(count, 9999);
}

module.exports = { register };
