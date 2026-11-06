'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');

// Mock electron module in require.cache so tests run cleanly in environments where
// Electron binary download was skipped (ELECTRON_SKIP_BINARY_DOWNLOAD=1)
try {
  const electronPath = require.resolve('electron');
  require.cache[electronPath] = {
    id: electronPath,
    filename: electronPath,
    loaded: true,
    exports: {
      app: { getPath: () => '/tmp', commandLine: { getSwitchValue: () => '' } },
      ipcMain: { on: () => { }, handle: () => { } },
      dialog: { showOpenDialog: async () => ({ canceled: true, filePaths: [] }) },
      nativeImage: { createFromPath: () => ({ isEmpty: () => true }) },
      clipboard: { writeImage: () => { } }
    }
  };
} catch (_) { }

const plugins = require(path.join(__dirname, '..', 'plugins'));

describe('Plugins Orchestrator', () => {
  function createMockContext() {
    const ipcHandlers = new Map();
    const appCommandLine = {
      getSwitchValue: () => ''
    };

    const app = {
      commandLine: appCommandLine,
      getPath: () => '/tmp',
      on: () => { },
      once: () => { }
    };

    const ipcMain = {
      on: (channel, handler) => ipcHandlers.set(channel, handler),
      handle: (channel, handler) => ipcHandlers.set(channel, handler),
      _handlers: ipcHandlers
    };

    const BrowserWindow = class MockBrowserWindow {
      constructor() { }
      static getAllWindows() { return []; }
      on() { }
    };

    const dialog = {
      showOpenDialog: async () => ({ canceled: true, filePaths: [] })
    };

    const nativeImage = {
      createFromPath: () => ({ isEmpty: () => true })
    };

    const tray = {
      setImage: () => { },
      setToolTip: () => { }
    };

    return {
      app,
      ipcMain,
      BrowserWindow,
      dialog,
      nativeImage,
      tray,
      appDir: path.join(__dirname, '..'),
      iconPath: path.join(__dirname, '..', 'sample', 'icon.png')
    };
  }

  it('exports required orchestrator interface functions', () => {
    assert.strictEqual(typeof plugins.initEarly, 'function');
    assert.strictEqual(typeof plugins.register, 'function');
    assert.strictEqual(typeof plugins.onMainWindowCreated, 'function');
  });

  it('runs initEarly without throwing on mock context', () => {
    const ctx = createMockContext();
    assert.doesNotThrow(() => {
      plugins.initEarly({ app: ctx.app, ipcMain: ctx.ipcMain });
    });
  });

  it('registers all feature plugins safely and does not throw', () => {
    const ctx = createMockContext();
    assert.doesNotThrow(() => {
      plugins.register(ctx);
    });
  });

  it('forwards main window to plugins via onMainWindowCreated', () => {
    const mockWin = { id: 1, getTitle: () => 'Zalo' };
    assert.doesNotThrow(() => {
      plugins.onMainWindowCreated(mockWin);
    });
  });

  it('isolates errors gracefully when registration fails', () => {
    const originalError = console.error;
    console.error = () => { };
    try {
      // Calling register with missing context or unexpected shapes should not crash
      assert.doesNotThrow(() => {
        plugins.register({});
      });
    } finally {
      console.error = originalError;
    }
  });
});
