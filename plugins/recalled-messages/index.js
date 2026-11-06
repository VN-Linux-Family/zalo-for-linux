'use strict';

const fs = require('fs');
const path = require('path');
const { createStore } = require('../advanced-settings/store');
const { STORAGE_KEY } = require('./log');

const MAIN_PAGE = /\/pc-dist\/index\.html(?:[?#]|$)/;

function register({ app, ipcMain, BrowserWindow, advancedSettings }) {
  const store = createStore(app.getPath('userData'));
  const enableScript = ['log.js', 'renderer.js']
    .map((name) => fs.readFileSync(path.join(__dirname, name), 'utf8'))
    .join('\n;\n');
  // Turning the feature off also deletes what it kept.
  const disableScript = `(function () {
    if (window.__zaloLinuxRecalled) window.__zaloLinuxRecalled.stop();
    else window.localStorage.removeItem(${JSON.stringify(STORAGE_KEY)});
  })();`;

  function apply(contents, enabled) {
    if (contents.isDestroyed() || !MAIN_PAGE.test(contents.getURL())) return;
    contents.executeJavaScript(enabled ? enableScript : disableScript, true).catch((error) => {
      console.error('[recalled-messages] Cannot apply setting:', error.message);
    });
  }

  app.on('browser-window-created', (_event, win) => {
    win.webContents.on('dom-ready', () => apply(win.webContents, store.read().keepRecalled));
  });

  ipcMain.handle('linux-settings:set-keep-recalled', (event, enabled) => {
    advancedSettings.assertSender(event);
    const settings = store.setKeepRecalled(enabled);
    for (const win of BrowserWindow.getAllWindows()) apply(win.webContents, settings.keepRecalled);
    return settings;
  });
}

module.exports = { register };
