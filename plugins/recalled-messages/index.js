'use strict';

const fs = require('fs');
const path = require('path');
const { createStore } = require('../advanced-settings/store');
const { STORAGE_KEY } = require('./log');

const MAIN_PAGE = /\/pc-dist\/index\.html(?:[?#]|$)/;

function register({ app, ipcMain, BrowserWindow, advancedSettings }) {
  const store = createStore(app.getPath('userData'));
  const localeMessages = JSON.stringify(require('./locale.json').messages);
  const localeScript = 'window.__zaloLinuxRecalledMessages = ' + localeMessages + ';\n';
  const enableScript = localeScript + ['log.js', 'renderer.js']
    .map((name) => fs.readFileSync(path.join(__dirname, name), 'utf8'))
    .join('\n;\n');
  // Turning the feature off also deletes what it kept.
  const disableScript = `(function () {
    if (window.__zaloLinuxRecalled) window.__zaloLinuxRecalled.stop();
    else window.localStorage.removeItem(${JSON.stringify(STORAGE_KEY)});
  })();`;

  // Chromium commits localStorage a few seconds late; flush so a deletion
  // survives an immediate SIGTERM (logout, shutdown).
  function flush(contents) {
    if (contents.isDestroyed()) return;
    try {
      contents.session.flushStorageData();
    } catch (error) {
      console.error('[recalled-messages] Cannot flush storage:', error.message);
    }
  }

  function apply(contents, enabled) {
    if (contents.isDestroyed() || !MAIN_PAGE.test(contents.getURL())) return;
    contents.executeJavaScript(enabled ? enableScript : disableScript, true)
      .then(() => { if (!enabled) flush(contents); })
      .catch((error) => {
        console.error('[recalled-messages] Cannot apply setting:', error.message);
      });
  }

  app.on('browser-window-created', (_event, win) => {
    win.webContents.on('dom-ready', () => apply(win.webContents, store.read().keepRecalled));
    win.on('page-title-updated', (event, title) => {
      if (title !== 'LINUX_RECALLED_FLUSH' || !MAIN_PAGE.test(win.webContents.getURL())) return;
      event.preventDefault();
      flush(win.webContents);
    });
  });

  ipcMain.handle('linux-settings:set-keep-recalled', (event, enabled) => {
    advancedSettings.assertSender(event);
    const settings = store.setKeepRecalled(enabled);
    for (const win of BrowserWindow.getAllWindows()) apply(win.webContents, settings.keepRecalled);
    return settings;
  });
}

module.exports = { register };
