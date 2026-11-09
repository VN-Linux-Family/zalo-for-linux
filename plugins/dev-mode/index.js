'use strict';

const fs = require('fs');
const path = require('path');

const ENV_OVERRIDE = 'ZALO_DEV_MODE';
const BACKGROUND_WINDOW_TITLES = new Set(['Shared Worker', 'SQLite']);

// Dev Mode owns its preference, keyboard shortcut, DevTools policy and tray item.
function register({ app, BrowserWindow, onTrayMenuChanged } = {}) {
  if (!app || typeof app.getPath !== 'function') {
    throw new TypeError('Dev Mode requires an Electron app instance.');
  }
  if (!BrowserWindow || typeof BrowserWindow.getAllWindows !== 'function') {
    throw new TypeError('Dev Mode requires Electron BrowserWindow.');
  }

  const userDataPath = app.getPath('userData');
  const configPath = path.join(userDataPath, 'zalo-linux-settings.json');
  const isForced = () => process.env[ENV_OVERRIDE] === '1';
  let lastFocusedWindow = null;

  function readSettings() {
    try {
      const settings = JSON.parse(fs.readFileSync(configPath, 'utf8'));
      return settings && typeof settings === 'object' && !Array.isArray(settings)
        ? settings
        : {};
    } catch (error) {
      if (error.code !== 'ENOENT') {
        console.error('[dev-mode] Cannot read settings:', error.message);
      }
      return {};
    }
  }

  function writeSettings(settings) {
    fs.mkdirSync(userDataPath, { recursive: true });
    const temporary = configPath + '.tmp';
    fs.writeFileSync(temporary, JSON.stringify(settings, null, 2), { mode: 0o600 });
    fs.renameSync(temporary, configPath);
    return settings;
  }

  function getStatus() {
    const forced = isForced();
    return {
      devModeEnabled: forced || readSettings().devMode === 1,
      devModeForced: forced
    };
  }

  function closeAllDevTools() {
    for (const win of BrowserWindow.getAllWindows()) {
      if (!win.isDestroyed() && win.webContents && win.webContents.isDevToolsOpened()) {
        win.webContents.closeDevTools();
      }
    }
  }

  function getTargetWindow() {
    const focused = BrowserWindow.getFocusedWindow();
    if (focused && !focused.isDestroyed()) return focused;

    if (lastFocusedWindow && !lastFocusedWindow.isDestroyed()) {
      return lastFocusedWindow;
    }

    return BrowserWindow.getAllWindows().find((win) =>
      !win.isDestroyed() && !BACKGROUND_WINDOW_TITLES.has(win.getTitle())
    ) || null;
  }

  function toggleDevTools() {
    if (!getStatus().devModeEnabled) return false;
    try {
      const win = getTargetWindow();
      if (!win || win.isDestroyed() || !win.webContents) return false;
      if (win.webContents.isDevToolsOpened()) win.webContents.closeDevTools();
      else win.webContents.openDevTools({ mode: 'detach' });
      return true;
    } catch (error) {
      console.error('[dev-mode] Toggle DevTools failed:', error);
      return false;
    }
  }

  function onWindowFocused(_event, win) {
    if (win && !win.isDestroyed() && !BACKGROUND_WINDOW_TITLES.has(win.getTitle())) {
      lastFocusedWindow = win;
    }
  }

  function onWindowCreated(_event, win) {
    if (!win || !win.webContents) return;
    win.webContents.on('devtools-opened', () => {
      if (!getStatus().devModeEnabled) win.webContents.closeDevTools();
    });
    win.webContents.on('before-input-event', (event, input) => {
      if (input.control && input.shift && String(input.key).toLowerCase() === 'i') {
        event.preventDefault();
        toggleDevTools();
      }
    });
  }

  app.on('browser-window-created', onWindowCreated);
  app.on('browser-window-focus', onWindowFocused);
  for (const win of BrowserWindow.getAllWindows()) onWindowCreated(null, win);

  return {
    isEnabled: () => getStatus().devModeEnabled,
    isForced,
    getStatus,
    toggleDevTools,
    getTrayMenuItems() {
      if (!getStatus().devModeEnabled) return [];
      return [{
        label: 'Toggle DevTools',
        click: toggleDevTools
      }];
    },
    setEnabled(enabled) {
      if (typeof enabled !== 'boolean') {
        throw new Error('Invalid devMode value; expected boolean.');
      }
      // An environment override is temporary and must never rewrite saved settings.
      if (isForced()) return getStatus();

      const settings = readSettings();
      writeSettings({ ...settings, devMode: enabled ? 1 : 0 });
      if (!enabled) closeAllDevTools();
      const status = getStatus();
      if (typeof onTrayMenuChanged === 'function') onTrayMenuChanged();
      return status;
    },
    dispose() {
      app.removeListener('browser-window-created', onWindowCreated);
      app.removeListener('browser-window-focus', onWindowFocused);
    }
  };
}

module.exports = { register };
