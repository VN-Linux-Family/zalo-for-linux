/**
 * plugins/start-hidden/index.js
 *
 * Start-hidden plugin - launching with `--hidden` (the flag the `auto-launch`
 * library writes into the Linux autostart entry) or `--start-hidden` keeps the
 * main window in the tray on startup (fixes #58).
 *
 * Zalo creates its main window with `show: false` and later shows it from the
 * renderer through `win.show()` - the same call it uses when a notification is
 * clicked. So shows are only suppressed during startup: once Zalo's startup
 * show has been swallowed (plus a short settle time for repeated calls), or
 * after a hard cap, the window behaves normally again. Opening from the tray
 * or launching Zalo again releases it immediately.
 */

'use strict';

const HIDDEN_FLAGS = ['--hidden', '--start-hidden'];
const SUPPRESSED_METHODS = ['show', 'showInactive', 'focus'];

// Keep swallowing repeated show calls for this long after the first one.
const DEFAULT_SETTLE_MS = 3000;
// Never suppress longer than this, even if Zalo never tried to show.
const DEFAULT_MAX_MS = 60000;

function isStartHiddenRequested(argv = process.argv) {
  return argv.some((arg) => HIDDEN_FLAGS.includes(arg));
}

function createStartHiddenController({
  argv = process.argv,
  settleMs = DEFAULT_SETTLE_MS,
  maxMs = DEFAULT_MAX_MS,
  timers = { setTimeout, clearTimeout }
} = {}) {
  const enabled = isStartHiddenRequested(argv);
  let released = !enabled;
  let win = null;
  let originals = null;
  let settleTimer = null;
  let maxTimer = null;

  function onSuppressedShow() {
    if (!settleTimer) settleTimer = timers.setTimeout(release, settleMs);
  }

  function onShow() {
    if (released || !win || win.isDestroyed()) return;
    win.hide();
    onSuppressedShow();
  }

  // Take control of the main window. Returns true if it was kept hidden.
  function attach(target) {
    if (released || win || !target || target.isDestroyed()) return false;
    win = target;

    originals = {};
    for (const name of SUPPRESSED_METHODS) {
      originals[name] = win[name];
      win[name] = () => onSuppressedShow();
    }
    win.on('show', onShow);
    maxTimer = timers.setTimeout(release, maxMs);

    if (win.isVisible()) win.hide();
    return true;
  }

  // Stop suppressing shows. Does not show the window itself.
  function release() {
    if (released) return;
    released = true;
    if (settleTimer) timers.clearTimeout(settleTimer);
    if (maxTimer) timers.clearTimeout(maxTimer);
    if (!win) return;

    win.removeListener('show', onShow);
    if (!win.isDestroyed()) {
      for (const name of SUPPRESSED_METHODS) win[name] = originals[name];
    }
  }

  return {
    get enabled() { return enabled; },
    get active() { return !released && win !== null; },
    attach,
    release
  };
}

module.exports = { HIDDEN_FLAGS, isStartHiddenRequested, createStartHiddenController };
