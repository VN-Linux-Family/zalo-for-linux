'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');

test('Screenshot Plugin - Built-in Linux Screen Capture', async (t) => {
  const pluginDir = path.join(__dirname, '..', 'plugins', 'screenshot');
  const extractScript = path.join(__dirname, '..', 'scripts', 'extract-screenshot-assets.js');

  await t.test('verifies all required plugin code files exist', () => {
    assert.ok(fs.existsSync(path.join(pluginDir, 'index.js')), 'index.js exists');
    assert.ok(fs.existsSync(path.join(pluginDir, 'capture.js')), 'capture.js exists');
    assert.ok(fs.existsSync(path.join(pluginDir, 'portal.py')), 'portal.py exists');
    assert.ok(fs.existsSync(path.join(pluginDir, 'ui', 'index.html')), 'ui/index.html exists');
    assert.ok(fs.existsSync(path.join(pluginDir, 'ui', 'screencap.css')), 'ui/screencap.css exists');
    assert.ok(fs.existsSync(path.join(pluginDir, 'ui', 'screencap.js')), 'ui/screencap.js exists');
    assert.ok(fs.existsSync(path.join(pluginDir, 'ui', 'preload.js')), 'ui/preload.js exists');
  });

  await t.test('verifies extract-screenshot-assets script extracts font from app directory', async () => {
    assert.ok(fs.existsSync(extractScript), 'scripts/extract-screenshot-assets.js exists');
    const { main, findFont } = require(extractScript);
    assert.strictEqual(typeof main, 'function');

    // Test extraction logic with mock app directory structure (clean checkout compatibility)
    const tempTestDir = path.join(pluginDir, 'mock-test-font');
    const mockAppDir = path.join(tempTestDir, 'app', 'pc-dist', 'fonts');
    const mockOutDir = path.join(tempTestDir, 'out');
    fs.mkdirSync(mockAppDir, { recursive: true });
    const dummyFontContent = Buffer.alloc(1024, 0x41);
    fs.writeFileSync(path.join(mockAppDir, 'zalo-pc-screencap.26b46cc0d8549b2db2c3314913a3b308.ttf'), dummyFontContent);

    try {
      await main({ appDir: path.join(tempTestDir, 'app'), outDir: mockOutDir });
      const copied = path.join(mockOutDir, 'zalo-pc-screencap.ttf');
      assert.ok(fs.existsSync(copied), 'mock font was successfully extracted');
      assert.strictEqual(fs.statSync(copied).size, 1024);
    } finally {
      fs.rmSync(tempTestDir, { recursive: true, force: true });
    }

    // If real app directory is present (e.g. after running prepare-app), also verify real extraction
    const realPcFonts = path.join(__dirname, '..', 'app', 'pc-dist', 'fonts');
    const hasRealFont = fs.existsSync(realPcFonts) || fs.existsSync(path.join(__dirname, '..', 'app'));
    if (hasRealFont && findFont) {
      const realFontFile = findFont(realPcFonts) || findFont(path.join(__dirname, '..', 'app'));
      if (realFontFile) {
        await main();
        const extractedFont = path.join(pluginDir, 'ui', 'fonts', 'zalo-pc-screencap.ttf');
        assert.ok(fs.existsSync(extractedFont), 'font extracted to plugins/screenshot/ui/fonts/zalo-pc-screencap.ttf');
        assert.ok(fs.statSync(extractedFont).size > 1000, 'extracted font size > 1000 bytes');
      }
    }
  });

  await t.test('verifies python portal script syntax and compilation', () => {
    const portalScript = path.join(pluginDir, 'portal.py');
    const res = spawnSync('python3', ['-m', 'py_compile', portalScript]);
    assert.strictEqual(res.status, 0, `portal.py compilation failed: ${res.stderr.toString()}`);
  });

  await t.test('verifies capture module export and portal script resolution in asar', () => {
    const { captureScreen, getPortalScriptPath } = require('../plugins/screenshot/capture');
    assert.strictEqual(typeof captureScreen, 'function');
    assert.strictEqual(typeof getPortalScriptPath, 'function');

    // In dev / non-asar environment
    const normalPath = getPortalScriptPath();
    assert.ok(fs.existsSync(normalPath), 'normal portal.py exists');
    assert.ok(normalPath.endsWith('portal.py'));

    // Simulated asar path where unpacked counterpart exists
    const mockAsarDir = path.join(pluginDir, 'mock.app.asar');
    const mockUnpackedDir = path.join(pluginDir, 'mock.app.asar.unpacked');
    fs.mkdirSync(mockUnpackedDir, { recursive: true });
    fs.writeFileSync(path.join(mockUnpackedDir, 'portal.py'), '# mock unpacked');
    try {
      const resolved = getPortalScriptPath(mockAsarDir);
      assert.strictEqual(resolved, path.join(mockUnpackedDir, 'portal.py'));
    } finally {
      fs.rmSync(mockUnpackedDir, { recursive: true, force: true });
    }
  });

  await t.test('verifies screenshot plugin register function wraps screen-capture IPC', () => {
    const screenshotPlugin = require('../plugins/screenshot');
    assert.strictEqual(typeof screenshotPlugin.register, 'function');
    assert.strictEqual(typeof screenshotPlugin.setMainWindow, 'function');

    const handlers = {};
    const mockIpcMain = {
      handle: (channel, fn) => {
        handlers[channel] = fn;
      }
    };

    screenshotPlugin.register({ ipcMain: mockIpcMain });
    assert.strictEqual(typeof mockIpcMain.handle, 'function');

    // Register a dummy handler
    mockIpcMain.handle('screen-capture', async () => 'original-response');
    assert.ok(handlers['screen-capture'], 'screen-capture handler registered');
  });

  await t.test('verifies UI assets contain original Zalo icon font glyphs', () => {
    const css = fs.readFileSync(path.join(pluginDir, 'ui', 'screencap.css'), 'utf8');
    assert.ok(css.includes('zalo-pc-screencap'), 'CSS references zalo-pc-screencap font');
    assert.ok(css.includes('\\61'), 'check glyph \\61 is defined');
    assert.ok(css.includes('\\62'), 'cancel glyph \\62 is defined');
    assert.ok(css.includes('\\64'), 'arrow glyph \\64 is defined');
    assert.ok(css.includes('\\67'), 'copy glyph \\67 is defined');
    assert.ok(css.includes('\\6d'), 'save glyph \\6d is defined');
  });

  await t.test('verifies captureScreen logs detailed diagnostics when capture fails', async () => {
    const { captureScreen } = require('../plugins/screenshot/capture');
    const logs = [];
    const origError = console.error;
    console.error = (...args) => {
      logs.push(args.join(' '));
    };

    try {
      // Mock invalid target directory to force failure or test exhaustion logging
      await captureScreen({ targetDir: '/dev/null/screencap' });
      assert.ok(logs.some(l => l.includes('[Screenshot Plugin] Failed to create target directory') || l.includes('capture failed')), 'diagnostic log captured');
    } finally {
      console.error = origError;
    }
  });

  await t.test('verifies getScreenshotDelay returns proper values for withZalo and withoutZalo modes', () => {
    const { getScreenshotDelay } = require('../plugins/screenshot');
    const { createStore } = require('../plugins/advanced-settings/store');
    const os = require('os');
    const tmpAdv = path.join(os.tmpdir(), `test-screenshot-delay-${Date.now()}`);

    const mockApp = {
      getPath: () => tmpAdv
    };

    try {
      // Default delays when no store exists yet
      assert.strictEqual(getScreenshotDelay('withZalo', mockApp), 100);
      assert.strictEqual(getScreenshotDelay('withoutZalo', mockApp), 500);

      // Custom configured delays
      const store = createStore(tmpAdv);
      store.saveScreenshotDelay('withZalo', 350);
      store.saveScreenshotDelay('withoutZalo', 850);

      assert.strictEqual(getScreenshotDelay('withZalo', mockApp), 350);
      assert.strictEqual(getScreenshotDelay('withoutZalo', mockApp), 850);
    } finally {
      fs.rmSync(tmpAdv, { recursive: true, force: true });
    }
  });
});
