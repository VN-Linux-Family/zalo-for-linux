'use strict';

const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');
const { createStore, AUDIO_EXTENSIONS, defaultSoundUrl } = require('./store');

function register({ app, ipcMain, BrowserWindow, dialog, devMode }) {
  const store = createStore(app.getPath('userData'));
  if (!devMode || typeof devMode.getStatus !== 'function' || typeof devMode.setEnabled !== 'function') {
    throw new Error('Advanced Settings requires the Dev Mode plugin.');
  }
  let settingsWindow = null;
  let mainWindow = null;

  function getWindow() { return settingsWindow; }

  function assertSender(event) {
    if (!settingsWindow || settingsWindow.isDestroyed() ||
        event.sender !== settingsWindow.webContents ||
        event.senderFrame !== event.sender.mainFrame) {
      throw new Error('Settings request was rejected.');
    }
  }

  function open() {
    if (settingsWindow && !settingsWindow.isDestroyed()) {
      if (settingsWindow.isMinimized()) settingsWindow.restore();
      settingsWindow.show();
      settingsWindow.focus();
      return;
    }
    settingsWindow = new BrowserWindow({
      parent: mainWindow && !mainWindow.isDestroyed() ? mainWindow : undefined,
      width: 980,
      height: 680,
      minWidth: 760,
      minHeight: 520,
      title: 'Nâng cao',
      backgroundColor: '#f4f6f8',
      webPreferences: {
        preload: path.join(__dirname, 'preload.js'),
        contextIsolation: true,
        nodeIntegration: false,
        sandbox: false
      }
    });
    settingsWindow.setMenuBarVisibility(false);
    if (settingsWindow.removeMenu) settingsWindow.removeMenu();
    settingsWindow.webContents.on('will-navigate', (event) => event.preventDefault());
    settingsWindow.webContents.setWindowOpenHandler(() => ({ action: 'deny' }));
    settingsWindow.loadFile(path.join(__dirname, 'index.html'));
    settingsWindow.on('closed', () => { settingsWindow = null; });
  }

  ipcMain.handle('linux-settings:get', (event) => {
    assertSender(event);
    return { ...store.read(), ...devMode.getStatus() };
  });
  ipcMain.handle('linux-settings:save-sound', (event, kind, value) => {
    assertSender(event);
    return store.save(kind, value);
  });
  ipcMain.handle('linux-settings:save-screenshot-delay', (event, kind, value) => {
    assertSender(event);
    return store.saveScreenshotDelay(kind, value);
  });
  ipcMain.handle('linux-settings:set-dev-mode', (event, enabled) => {
    assertSender(event);
    if (typeof enabled !== 'boolean') throw new Error('Giá trị dev mode không hợp lệ.');
    return devMode.setEnabled(enabled);
  });
  ipcMain.handle('linux-settings:pick-sound', async (event, kind) => {
    assertSender(event);
    store.validateKind(kind);
    const result = await dialog.showOpenDialog(settingsWindow, {
      title: kind === 'ringtone' ? 'Chọn nhạc chuông' : 'Chọn nhạc chờ',
      defaultPath: store.read()[kind] || undefined,
      properties: ['openFile'],
      filters: [{ name: 'Âm thanh', extensions: AUDIO_EXTENSIONS }]
    });
    if (result.canceled || !result.filePaths[0]) return null;
    return store.validatePath(result.filePaths[0]);
  });
  ipcMain.handle('linux-settings:preview-sound', (event, kind, value) => {
    assertSender(event);
    store.validateKind(kind);
    const validated = store.validatePath(value);
    return validated ? pathToFileURL(validated).href : defaultSoundUrl(kind);
  });

  const menuScript = fs.readFileSync(path.join(__dirname, 'inject-menu.js'), 'utf8');
  app.on('browser-window-created', (_event, win) => {
    const isZaloPage = () => /\/pc-dist\/(index|child)\.html(?:[?#]|$)/.test(win.webContents.getURL());
    win.on('page-title-updated', (event, title) => {
      if (title !== 'LINUX_ADVANCED_SETTINGS_TRIGGER' || !isZaloPage()) return;
      event.preventDefault();
      mainWindow = win;
      open();
    });
    win.webContents.on('dom-ready', () => {
      if (!isZaloPage()) return;
      win.webContents.executeJavaScript(menuScript, true).catch((error) => {
        console.error('[advanced-settings] Cannot inject menu:', error.message);
      });
    });
  });

  return { getWindow, assertSender, store };
}

module.exports = { register };
