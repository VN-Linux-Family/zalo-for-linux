/**
 * plugins/tray-host/index.js
 *
 * Tray host detection. The tray icon only shows when a StatusNotifier host
 * owns org.kde.StatusNotifierWatcher (KDE, XFCE, GNOME with the AppIndicator
 * extension). Stock GNOME has none: the Tray object is created but invisible,
 * so hiding the window on close left Zalo running in the background with no
 * way to reopen or quit it.
 *
 * main.js hides to tray only when a host is available and quits otherwise.
 */

'use strict';

const { execFile, execFileSync } = require('child_process');

const WATCHER = 'org.kde.StatusNotifierWatcher';
const DBUS_ARGS = ['call', '--session', '--dest', 'org.freedesktop.DBus',
  '--object-path', '/org/freedesktop/DBus',
  '--method', 'org.freedesktop.DBus.NameHasOwner', WATCHER];
const TIMEOUT_MS = 3000;
// The host can come and go (extension toggled, panel restarted).
const RECHECK_MS = 60000;

let available = false;
let timer = null;

function parse(out) {
  return /\btrue\b/.test(String(out));
}

// Synchronous first check so the startup decisions (start-hidden) are right.
function init() {
  if (process.platform !== 'linux') {
    available = true;
    return available;
  }
  try {
    available = parse(execFileSync('gdbus', DBUS_ARGS, { timeout: TIMEOUT_MS }));
  } catch (_) {
    available = false;
  }
  if (!timer) {
    timer = setInterval(() => {
      execFile('gdbus', DBUS_ARGS, { timeout: TIMEOUT_MS }, (err, out) => {
        available = !err && parse(out);
      });
    }, RECHECK_MS);
    timer.unref();
  }
  return available;
}

function isAvailable() {
  return available;
}

module.exports = { init, isAvailable };
