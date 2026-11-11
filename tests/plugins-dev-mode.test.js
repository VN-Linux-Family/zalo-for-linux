'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { EventEmitter } = require('events');
const devModePlugin = require('../plugins/dev-mode');

describe('Dev Mode plugin', () => {
  it('defaults off and persists the preference in zalo-linux-settings.json', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-dev-mode-'));
    try {
      const app = new EventEmitter();
      app.getPath = () => dir;
      const BrowserWindow = { getAllWindows: () => [], getFocusedWindow: () => null };
      const devMode = devModePlugin.register({ app, BrowserWindow });
      assert.deepEqual(devMode.getStatus(), { devModeEnabled: false, devModeForced: false });
      devMode.setEnabled(true);
      assert.deepEqual(JSON.parse(fs.readFileSync(path.join(dir, 'zalo-linux-settings.json'), 'utf8')), { devMode: 1 });
      const reloaded = devModePlugin.register({ app, BrowserWindow });
      assert.equal(reloaded.isEnabled(), true);
      reloaded.dispose();
      devMode.setEnabled(false);
      assert.equal(devMode.isEnabled(), false);
      devMode.dispose();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('environment override forces enabled without changing saved preference', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-dev-mode-env-'));
    const previous = process.env.ZALO_DEV_MODE;
    try {
      fs.writeFileSync(path.join(dir, 'zalo-linux-settings.json'), JSON.stringify({ devMode: 0, other: 'preserve' }));
      process.env.ZALO_DEV_MODE = '1';
      const app = new EventEmitter();
      app.getPath = () => dir;
      const BrowserWindow = { getAllWindows: () => [], getFocusedWindow: () => null };
      const devMode = devModePlugin.register({ app, BrowserWindow });
      assert.deepEqual(devMode.getStatus(), { devModeEnabled: true, devModeForced: true });
      assert.deepEqual(devMode.setEnabled(false), { devModeEnabled: true, devModeForced: true });
      devMode.dispose();
      assert.deepEqual(JSON.parse(fs.readFileSync(path.join(dir, 'zalo-linux-settings.json'), 'utf8')), { devMode: 0, other: 'preserve' });
    } finally {
      if (previous === undefined) delete process.env.ZALO_DEV_MODE;
      else process.env.ZALO_DEV_MODE = previous;
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('owns the shortcut, blocks DevTools while disabled, and exposes tray item only when enabled', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-dev-mode-policy-'));
    const app = new EventEmitter();
    app.getPath = () => dir;
    const windows = [];
    const BrowserWindow = {
      getAllWindows: () => windows,
      getFocusedWindow: () => windows[0] || null
    };
    const devMode = devModePlugin.register({ app, BrowserWindow });
    const win = new EventEmitter();
    win.isDestroyed = () => false;
    const webContents = new EventEmitter();
    let opened = false;
    let closed = 0;
    webContents.isDevToolsOpened = () => opened;
    webContents.openDevTools = () => { opened = true; };
    webContents.closeDevTools = () => { opened = false; closed++; };
    win.webContents = webContents;
    windows.push(win);

    try {
      app.emit('browser-window-created', {}, win);
      assert.deepEqual(devMode.getTrayMenuItems(), []);

      let prevented = false;
      webContents.emit('before-input-event', { preventDefault: () => { prevented = true; } }, {
        control: true, shift: true, key: 'i'
      });
      assert.equal(prevented, true);
      assert.equal(opened, false);

      devMode.setEnabled(true);
      assert.equal(devMode.getTrayMenuItems().length, 1);
      prevented = false;
      webContents.emit('before-input-event', { preventDefault: () => { prevented = true; } }, {
        control: true, shift: true, key: 'I'
      });
      assert.equal(prevented, true);
      assert.equal(opened, true);

      devMode.setEnabled(false);
      assert.equal(opened, false);
      assert.equal(closed, 1);
      assert.deepEqual(devMode.getTrayMenuItems(), []);
    } finally {
      devMode.dispose();
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('localizes its tray item through Language Core requests', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-dev-mode-locale-'));
    try {
      const app = new EventEmitter();
      app.getPath = () => dir;
      const BrowserWindow = { getAllWindows: () => [], getFocusedWindow: () => null };
      const devMode = devModePlugin.register({ app, BrowserWindow });
      devMode.setEnabled(true);

      assert.equal(devMode.getLocale(), 'vi');
      assert.equal(devMode.getTrayMenuItems()[0].label, 'Bật/tắt DevTools');
      assert.equal(devMode.onLanguageUpdateRequest({
        type: 'LANGUAGE_UPDATE_REQUEST',
        locale: 'en'
      }), true);
      assert.equal(devMode.getLocale(), 'en');
      assert.equal(devMode.getTrayMenuItems()[0].label, 'Toggle DevTools');

      assert.equal(devMode.onLanguageUpdateRequest({
        type: 'LANGUAGE_UPDATE_REQUEST',
        locale: 'fr'
      }), false);
      assert.equal(devMode.getTrayMenuItems()[0].label, 'Toggle DevTools');
      devMode.dispose();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('rejects non-boolean updates', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-dev-mode-invalid-'));
    try {
      const app = new EventEmitter();
      app.getPath = () => dir;
      const BrowserWindow = { getAllWindows: () => [], getFocusedWindow: () => null };
      const devMode = devModePlugin.register({ app, BrowserWindow });
      assert.throws(() => devMode.setEnabled(1), /expected boolean/);
      devMode.dispose();
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
