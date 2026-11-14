'use strict';

const { describe, it, beforeEach, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const zadarkPlugin = require('../plugins/zadark');

describe('ZaDark Flatpak Extension & Dynamic Injection', () => {
  let tmpDir;
  const originalEnv = { ...process.env };

  beforeEach(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'zadark-flatpak-test-'));
  });

  afterEach(() => {
    process.env = { ...originalEnv };
    if (tmpDir && fs.existsSync(tmpDir)) {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  describe('Interface & Exports', () => {
    it('exports required functions for registration, detection, and asset injection', () => {
      assert.strictEqual(typeof zadarkPlugin.register, 'function');
      assert.strictEqual(typeof zadarkPlugin.isZaDarkInstalled, 'function');
      assert.strictEqual(typeof zadarkPlugin.resolveZaDarkDir, 'function');
      assert.strictEqual(typeof zadarkPlugin.isFlatpak, 'function');
      assert.strictEqual(typeof zadarkPlugin.injectZaDarkAssets, 'function');
    });
  });

  describe('Flatpak Detection (isFlatpak)', () => {
    it('detects Flatpak when process.env.FLATPAK_ID is set', () => {
      process.env.FLATPAK_ID = 'net.vietnamlinuxfamily.zalo_for_linux';
      assert.strictEqual(zadarkPlugin.isFlatpak(), true);
    });

    it('returns false when neither FLATPAK_ID nor /.flatpak-info is present', () => {
      delete process.env.FLATPAK_ID;
      // In normal test runner /.flatpak-info does not exist
      if (!fs.existsSync('/.flatpak-info')) {
        assert.strictEqual(zadarkPlugin.isFlatpak(), false);
      }
    });
  });

  describe('ZaDark Path Resolution (resolveZaDarkDir)', () => {
    it('prioritizes process.env.ZADARK_DIR when provided and directory exists', () => {
      const customZadark = path.join(tmpDir, 'custom-zadark');
      fs.mkdirSync(customZadark, { recursive: true });

      process.env.ZADARK_DIR = customZadark;
      process.env.FLATPAK_ID = 'net.vietnamlinuxfamily.zalo_for_linux';

      const resolved = zadarkPlugin.resolveZaDarkDir('/dummy/app');
      assert.strictEqual(resolved, customZadark);
    });

    it('ignores ZADARK_DIR if the specified directory does not exist', () => {
      process.env.ZADARK_DIR = path.join(tmpDir, 'non-existent-zadark');
      delete process.env.FLATPAK_ID;

      // In non-Flatpak with no local pc-dist/zadark, it returns null
      assert.strictEqual(zadarkPlugin.resolveZaDarkDir(tmpDir), null);
    });

    it('resolves local pc-dist/zadark in non-Flatpak environments (AppImage ZaDark build)', () => {
      delete process.env.FLATPAK_ID;
      delete process.env.ZADARK_DIR;

      const localZadark = path.join(tmpDir, 'pc-dist', 'zadark');
      fs.mkdirSync(localZadark, { recursive: true });

      const resolved = zadarkPlugin.resolveZaDarkDir(tmpDir);
      assert.strictEqual(resolved, localZadark);
      assert.strictEqual(zadarkPlugin.isZaDarkInstalled(tmpDir), true);
    });

    it('returns null in non-Flatpak environment when pc-dist/zadark does not exist (AppImage Original build)', () => {
      delete process.env.FLATPAK_ID;
      delete process.env.ZADARK_DIR;

      assert.strictEqual(zadarkPlugin.resolveZaDarkDir(tmpDir), null);
      assert.strictEqual(zadarkPlugin.isZaDarkInstalled(tmpDir), false);
    });

    it('returns null in Flatpak environment when extension /app/lib/zadark is not installed', () => {
      process.env.FLATPAK_ID = 'net.vietnamlinuxfamily.zalo_for_linux';
      delete process.env.ZADARK_DIR;

      // Local pc-dist/zadark exists, but in Flatpak we only mount extension at /app/lib/zadark
      const localZadark = path.join(tmpDir, 'pc-dist', 'zadark');
      fs.mkdirSync(localZadark, { recursive: true });

      // Unless /app/lib/zadark actually exists on host, it must return null
      if (!fs.existsSync('/app/lib/zadark')) {
        assert.strictEqual(zadarkPlugin.resolveZaDarkDir(tmpDir), null);
        assert.strictEqual(zadarkPlugin.isZaDarkInstalled(tmpDir), false);
      }
    });
  });

  describe('Dynamic Asset Injection (injectZaDarkAssets)', () => {
    function createMockWindow() {
      const insertedCss = [];
      const executedScripts = [];
      let isDestroyedState = false;

      return {
        isDestroyed: () => isDestroyedState,
        destroy: () => { isDestroyedState = true; },
        webContents: {
          insertCSS: async (css) => {
            insertedCss.push(css);
          },
          executeJavaScript: async (code) => {
            executedScripts.push(code);
            return true;
          }
        },
        _insertedCss: insertedCss,
        _executedScripts: executedScripts
      };
    }

    it('bails out safely if window is already destroyed', async () => {
      const mockWin = createMockWindow();
      mockWin.destroy();

      const result = await zadarkPlugin.injectZaDarkAssets(mockWin, tmpDir, 'index');
      assert.strictEqual(result, false);
      assert.strictEqual(mockWin._insertedCss.length, 0);
      assert.strictEqual(mockWin._executedScripts.length, 0);
    });

    it('injects CSS with font URLs rewritten and executes scripts in sequence', async () => {
      const mockWin = createMockWindow();

      // Create fake ZaDark directory structure
      fs.mkdirSync(path.join(tmpDir, 'css'), { recursive: true });
      fs.mkdirSync(path.join(tmpDir, 'fonts'), { recursive: true });
      fs.mkdirSync(path.join(tmpDir, 'libs'), { recursive: true });
      fs.mkdirSync(path.join(tmpDir, 'js'), { recursive: true });

      fs.writeFileSync(
        path.join(tmpDir, 'css', 'zadark-fonts.min.css'),
        '@font-face { src: url("../fonts/ZaDarkIcons.ttf"); }',
        'utf8'
      );
      fs.writeFileSync(
        path.join(tmpDir, 'css', 'zadark.min.css'),
        ':root { --zadark-bg: #1e1e1e; }',
        'utf8'
      );
      fs.writeFileSync(
        path.join(tmpDir, 'libs', 'zadark-jquery.min.js'),
        'window.jQuery = { fn: {} };',
        'utf8'
      );
      fs.writeFileSync(
        path.join(tmpDir, 'js', 'zadark.min.js'),
        'window.__zadark = true;',
        'utf8'
      );

      const result = await zadarkPlugin.injectZaDarkAssets(mockWin, tmpDir, 'index');
      assert.strictEqual(result, true);

      // Verify CSS font URL rewrite
      assert.ok(mockWin._insertedCss.length >= 2, 'Should have inserted at least 2 CSS stylesheets');
      const fontCss = mockWin._insertedCss.find(c => c.includes('@font-face'));
      assert.ok(fontCss, 'Font CSS must be present');
      assert.ok(!fontCss.includes('../fonts/'), 'Relative ../fonts/ must be replaced with file:// URL');
      assert.ok(fontCss.includes('/fonts/'), 'Font URL must point to fonts directory');

      // Verify body class injection
      const classScript = mockWin._executedScripts.find(s => s.includes('zadark-linux'));
      assert.ok(classScript, 'Should execute script setting body class names');

      // Verify JS execution order
      const jqIndex = mockWin._executedScripts.findIndex(s => s.includes('window.jQuery'));
      const zadarkIndex = mockWin._executedScripts.findIndex(s => s.includes('window.__zadark'));
      assert.ok(jqIndex !== -1, 'jQuery script must be executed');
      assert.ok(zadarkIndex !== -1, 'zadark script must be executed');
      assert.ok(jqIndex < zadarkIndex, 'jQuery must execute before main zadark script');
    });
  });

  describe('Plugin Registration Lifecycle (register)', () => {
    function createMockApp() {
      const listeners = new Map();
      return {
        on: (event, handler) => {
          if (!listeners.has(event)) listeners.set(event, []);
          listeners.get(event).push(handler);
        },
        _emit: (event, ...args) => {
          const handlers = listeners.get(event) || [];
          for (const h of handlers) h(...args);
        },
        _listeners: listeners
      };
    }

    it('does not register window listener when ZaDark is not installed', () => {
      delete process.env.FLATPAK_ID;
      delete process.env.ZADARK_DIR;

      const mockApp = createMockApp();
      zadarkPlugin.register({ app: mockApp, ipcMain: {}, appDir: tmpDir });

      assert.strictEqual(mockApp._listeners.has('browser-window-created'), false);
    });

    it('registers window listener and executes theme sync when ZaDark is installed', async () => {
      const zadarkDir = path.join(tmpDir, 'pc-dist', 'zadark');
      fs.mkdirSync(zadarkDir, { recursive: true });
      fs.mkdirSync(path.join(zadarkDir, 'js'), { recursive: true });

      // Create dummy zadark-main module
      fs.writeFileSync(
        path.join(zadarkDir, 'js', 'zadark-main.min.js'),
        '// mock zadark-main\nmodule.exports = {};',
        'utf8'
      );

      delete process.env.FLATPAK_ID;
      delete process.env.ZADARK_DIR;

      const mockApp = createMockApp();
      zadarkPlugin.register({ app: mockApp, ipcMain: {}, appDir: tmpDir });

      assert.strictEqual(mockApp._listeners.has('browser-window-created'), true);

      // Simulate window creation and dom-ready
      let domReadyHandler;
      const executed = [];
      const mockWin = {
        isDestroyed: () => false,
        webContents: {
          getURL: () => 'file:///app/pc-dist/index.html',
          executeJavaScript: async (code) => {
            executed.push(code);
            // Simulate that ZaDark is already loaded (static AppImage bake)
            if (code.includes('window.__zadark')) return true;
            return true;
          }
        }
      };

      const windowHandler = mockApp._listeners.get('browser-window-created')[0];
      mockWin.webContents.on = (event, handler) => {
        if (event === 'dom-ready') domReadyHandler = handler;
      };

      windowHandler({}, mockWin);
      assert.ok(domReadyHandler, 'dom-ready listener should be registered on webContents');

      await domReadyHandler();

      // Should have checked hasZaDark and injected RENDERER_SCRIPT
      assert.ok(executed.length >= 2);
    });
  });
});
