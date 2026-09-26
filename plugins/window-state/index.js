/**
 * plugins/window-state/index.js
 *
 * Window-state plugin - keeps the main window's position, size and maximized
 * state across hide/show on Linux (fixes #96).
 *
 * Hiding a window unmaps it, and on X11/XWayland the window manager does not
 * reliably bring it back the way it was:
 *  - Zalo's close handler calls `unmaximize()` right before `hide()`. The WM
 *    never gets to process the unmaximize, so Electron and the WM disagree on
 *    the state and the window comes back maximized or at the wrong place.
 *  - mutter forgets the "restore" geometry of a window that was hidden while
 *    maximized, so un-maximizing afterwards picks an arbitrary size.
 *  - With start-hidden, Zalo's startup `maximize()` maps the window while it is
 *    being hidden, and it ends up covering the screen from (0,0) without being
 *    maximized, so the maximize button does nothing.
 *
 * The plugin remembers the last normal bounds and the maximized flag while
 * the window is visible, and puts them back every time the window is shown,
 * whoever shows it (tray, second instance, notification, Zalo itself).
 */

'use strict';

// Let the WM finish mapping the window before correcting it.
const DEFAULT_SETTLE_MS = 80;
// Coalesce bursts of move/resize events.
const DEFAULT_SAVE_DELAY_MS = 250;

function boundsEqual(a, b) {
  return !!a && !!b && a.x === b.x && a.y === b.y && a.width === b.width && a.height === b.height;
}

// Clamp bounds into the work area of the display they belong to, so a window
// saved on a monitor that is gone does not come back off-screen.
function fitToScreen(bounds, screen) {
  if (!bounds || !screen) return bounds;
  const display = screen.getDisplayMatching(bounds);
  if (!display || !display.workArea) return bounds;
  const area = display.workArea;
  const width = Math.min(bounds.width, area.width);
  const height = Math.min(bounds.height, area.height);
  const x = Math.min(Math.max(bounds.x, area.x), area.x + area.width - width);
  const y = Math.min(Math.max(bounds.y, area.y), area.y + area.height - height);
  return { x, y, width, height };
}

function createWindowStateController({
  screen = null,
  // Native Wayland windows cannot be positioned, only sized and maximized.
  canPosition = true,
  settleMs = DEFAULT_SETTLE_MS,
  saveDelayMs = DEFAULT_SAVE_DELAY_MS,
  timers = { setTimeout, clearTimeout }
} = {}) {
  let win = null;
  let normalBounds = null;
  let maximized = false;
  // Electron's getNormalBounds() is right until a maximized window has been
  // hidden once; after that it returns the maximized bounds.
  let normalBoundsTrusted = true;
  // State to restore on the next show, taken when the window gets hidden.
  let pending = null;
  // After re-showing a maximized window, the WM's restore geometry is gone:
  // put our own back on the first un-maximize.
  let fixNextUnmaximize = false;
  let saveTimer = null;
  let restoreTimer = null;
  let restoring = false;
  let suppressUnmaximize = false;

  function alive() {
    return win && !win.isDestroyed();
  }

  function readState() {
    maximized = win.isMaximized();
    if (!maximized) {
      normalBounds = win.getBounds();
      normalBoundsTrusted = true;
    } else if (normalBoundsTrusted || !normalBounds) {
      normalBounds = win.getNormalBounds();
    }
  }

  function save() {
    saveTimer = null;
    if (!alive() || restoring || !win.isVisible() || win.isMinimized() || win.isFullScreen()) return;
    readState();
  }

  function scheduleSave() {
    if (saveTimer) timers.clearTimeout(saveTimer);
    saveTimer = timers.setTimeout(save, saveDelayMs);
  }

  // Called just before the window is hidden, while its state is still right.
  function snapshot() {
    if (!alive() || !win.isVisible() || win.isMinimized()) return;
    if (saveTimer) {
      timers.clearTimeout(saveTimer);
      saveTimer = null;
    }
    if (!win.isFullScreen()) readState();
    if (maximized) normalBoundsTrusted = false;
    pending = { maximized, bounds: normalBounds };
  }

  function applyBounds(bounds) {
    const target = fitToScreen(bounds, screen);
    if (canPosition) {
      if (!boundsEqual(win.getBounds(), target)) win.setBounds(target, false);
    } else {
      win.setSize(target.width, target.height, false);
    }
  }

  function restore() {
    restoreTimer = null;
    const state = pending;
    pending = null;
    if (!alive() || !state || !win.isVisible() || win.isMinimized() || win.isFullScreen()) return;

    restoring = true;
    try {
      if (state.maximized) {
        if (!win.isMaximized()) {
          if (state.bounds) applyBounds(state.bounds);
          win.maximize();
        }
        fixNextUnmaximize = !!state.bounds;
      } else if (state.bounds) {
        if (win.isMaximized()) win.unmaximize();
        applyBounds(state.bounds);
      }
    } finally {
      restoring = false;
    }
  }

  function onShow() {
    if (!pending) return;
    if (restoreTimer) timers.clearTimeout(restoreTimer);
    restoreTimer = timers.setTimeout(restore, settleMs);
  }

  function onUnmaximize() {
    if (fixNextUnmaximize && !restoring && normalBounds) {
      fixNextUnmaximize = false;
      // Let the WM apply its own geometry first, then replace it.
      timers.setTimeout(() => {
        if (alive() && !win.isMaximized()) applyBounds(normalBounds);
      }, settleMs);
    }
    scheduleSave();
  }

  // Our close listener runs before Zalo's, so the snapshot still sees the
  // maximized state, and Zalo's unmaximize() before hide() is skipped.
  function onClose() {
    snapshot();
    suppressUnmaximize = true;
    timers.setTimeout(() => { suppressUnmaximize = false; }, 0);
  }

  function attach(target) {
    if (win || !target || target.isDestroyed()) return false;
    win = target;

    const originalHide = win.hide;
    win.hide = function (...args) {
      snapshot();
      return originalHide.apply(this, args);
    };
    const originalUnmaximize = win.unmaximize;
    win.unmaximize = function (...args) {
      if (suppressUnmaximize) return undefined;
      return originalUnmaximize.apply(this, args);
    };

    normalBounds = win.getNormalBounds();
    maximized = win.isMaximized();

    win.on('close', onClose);
    win.on('show', onShow);
    win.on('move', scheduleSave);
    win.on('resize', scheduleSave);
    win.on('maximize', scheduleSave);
    win.on('unmaximize', onUnmaximize);
    win.once('closed', () => {
      if (saveTimer) timers.clearTimeout(saveTimer);
      if (restoreTimer) timers.clearTimeout(restoreTimer);
      win = null;
    });
    return true;
  }

  // Remember a maximize that happened while the window could not be shown
  // (start-hidden), so the first real show maximizes it properly.
  function requestMaximize() {
    if (alive()) normalBounds = win.getNormalBounds();
    maximized = true;
    pending = { maximized: true, bounds: normalBounds };
  }

  return {
    attach,
    requestMaximize
  };
}

module.exports = { createWindowStateController, fitToScreen };
