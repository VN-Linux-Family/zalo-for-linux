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

      // Verify body class and metadata attributes injection
      const classScript = mockWin._executedScripts.find(s => s.includes('zadark-linux'));
      assert.ok(classScript, 'Should execute script setting body class names');
      assert.ok(classScript.includes('data-zadark-os'), 'Should set data-zadark-os attribute');
      assert.ok(classScript.includes('data-zadark-version'), 'Should set data-zadark-version attribute');

      // Verify JS execution order
      const jqIndex = mockWin._executedScripts.findIndex(s => s.includes('window.jQuery'));
      const zadarkIndex = mockWin._executedScripts.findIndex(s => s.includes('window.__zadark'));
      assert.ok(jqIndex !== -1, 'jQuery script must be executed');
      assert.ok(zadarkIndex !== -1, 'zadark script must be executed');
      assert.ok(jqIndex < zadarkIndex, 'jQuery must execute before main zadark script');
    });

    it('rewrites relative zadark/ URLs, require() paths, and resolves popup-viewer & znotification assets', async () => {
      const mockWin = createMockWindow();
      const baseUrl = require('url').pathToFileURL(tmpDir).href;

      fs.mkdirSync(path.join(tmpDir, 'css'), { recursive: true });
      fs.mkdirSync(path.join(tmpDir, 'js'), { recursive: true });
      fs.mkdirSync(path.join(tmpDir, 'libs'), { recursive: true });

      fs.writeFileSync(
        path.join(tmpDir, 'js', 'zadark.min.js'),
        [
          'if("function"==typeof require){window.$=require("zadark/libs/zadark-jquery.min.js")}',
          'const s=["zadark/css/zadark-fonts.min.css","zadark/css/zadark.min.css"];',
          'document.documentElement.setAttribute("data-zadark-emoji-url","zadark/images/zalo-emoji-md.png");',
          'const h=\'<img src="zadark/images/zadark-lockup.svg">\';',
          'localStorage.setItem("zadark/reaction-recent-emojis","[]");'
        ].join('\n'),
        'utf8'
      );
      fs.writeFileSync(
        path.join(tmpDir, 'js', 'zadark-popup-viewer.min.js'),
        'document.documentElement.setAttribute("data-zadark-theme","dark");',
        'utf8'
      );
      fs.writeFileSync(
        path.join(tmpDir, 'css', 'zadark-znotification.min.css'),
        'html[data-zadark-theme=dark]{color:#e5e5e5;}',
        'utf8'
      );
      fs.writeFileSync(
        path.join(tmpDir, 'js', 'zadark-znotification.min.js'),
        'window.WebFont=require("zadark/libs/zadark-webfont.min.js");',
        'utf8'
      );
      fs.writeFileSync(
        path.join(tmpDir, 'package.json'),
        JSON.stringify({ version: '26.1.0' }),
        'utf8'
      );

      await zadarkPlugin.injectZaDarkAssets(mockWin, tmpDir, 'index');

      const metaScript = mockWin._executedScripts.find(s => s.includes('data-zadark-version'));
      assert.ok(metaScript.includes('"26.1.0"'), 'Version must be resolved from package.json');

      const zadarkJs = mockWin._executedScripts.find(s => s.includes('data-zadark-emoji-url'));
      assert.ok(zadarkJs, 'zadark.min.js must be executed');
      assert.ok(zadarkJs.includes(`require("${tmpDir.replace(/\\/g, '/')}/libs/zadark-jquery.min.js")`), 'require() path must be rewritten to filesystem path');
      assert.ok(zadarkJs.includes(`"${baseUrl}/css/zadark-fonts.min.css"`), 'CSS href must be rewritten to file:// URL');
      assert.ok(zadarkJs.includes(`"${baseUrl}/images/zalo-emoji-md.png"`), 'Emoji sprite URL must be rewritten to file:// URL');
      assert.ok(zadarkJs.includes(`"${baseUrl}/images/zadark-lockup.svg"`), 'Logo img src must be rewritten to file:// URL');
      assert.ok(zadarkJs.includes('"zadark/reaction-recent-emojis"'), 'Non-asset localStorage key must remain untouched');

      const popupWin = createMockWindow();
      await zadarkPlugin.injectZaDarkAssets(popupWin, tmpDir, 'popup');
      assert.strictEqual(popupWin._insertedCss.length, 0, 'popup-viewer should not inject index CSS');
      assert.ok(
        popupWin._executedScripts.some(s => s.includes('data-zadark-theme')),
        'popup-viewer must execute js/zadark-popup-viewer.min.js'
      );

      const notifWin = createMockWindow();
      await zadarkPlugin.injectZaDarkAssets(notifWin, tmpDir, 'znotification');
      assert.strictEqual(notifWin._insertedCss.length, 1, 'znotification should inject zadark-znotification.min.css');
      assert.ok(
        notifWin._executedScripts.some(s => s.includes(`require("${tmpDir.replace(/\\/g, '/')}/libs/zadark-webfont.min.js")`)),
        'znotification script require() must be rewritten'
      );

      assert.strictEqual(zadarkPlugin.resolveWindowType('file:///app/pc-dist/index.html'), 'index');
      assert.strictEqual(zadarkPlugin.resolveWindowType('file:///app/pc-dist/popup-viewer.html'), 'popup');
      assert.strictEqual(zadarkPlugin.resolveWindowType('file:///app/pc-dist/znotification.html'), 'znotification');
      assert.strictEqual(zadarkPlugin.resolveWindowType('file:///app/pc-dist/other.html'), null);
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
      const insertedCss = [];
      const mockWin = {
        isDestroyed: () => false,
        webContents: {
          getURL: () => 'file:///app/pc-dist/index.html',
          insertCSS: async (css) => {
            insertedCss.push(css);
          },
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

      // Should have checked hasZaDark (including #div_Main_TabZaDark guard) and injected RENDERER_SCRIPT
      assert.ok(executed[0].includes('div_Main_TabZaDark'), 'hasZaDark check must guard against duplicate sidebar tab');
      assert.strictEqual(insertedCss.length, 0, 'Must not inject extra CSS when ZaDark is already loaded');
      assert.ok(executed.length >= 2);
    });

    it('prevents concurrent dom-ready events from double-injecting ZaDark assets', async () => {
      const zadarkDir = path.join(tmpDir, 'ext-zadark');
      fs.mkdirSync(path.join(zadarkDir, 'js'), { recursive: true });
      fs.writeFileSync(path.join(zadarkDir, 'js', 'zadark.min.js'), 'window.__testZadarkRuns = (window.__testZadarkRuns || 0) + 1;', 'utf8');

      process.env.ZADARK_DIR = zadarkDir;
      delete process.env.FLATPAK_ID;

      const mockApp = createMockApp();
      zadarkPlugin.register({ app: mockApp, ipcMain: {}, appDir: tmpDir });

      let domReadyHandler;
      let zadarkExecCount = 0;
      let hasInjected = false;
      const mockWin = {
        isDestroyed: () => false,
        webContents: {
          getURL: () => 'file:///app/pc-dist/index.html',
          on: (event, handler) => {
            if (event === 'dom-ready') domReadyHandler = handler;
          },
          insertCSS: async () => {},
          executeJavaScript: async (code) => {
            if (code.includes('window.__zflZadarkInjected = true')) {
              hasInjected = true;
              return true;
            }
            if (code.includes('window.__zadark ||')) {
              // Simulate slow async check so two concurrent dom-ready calls overlap
              await new Promise(resolve => setTimeout(resolve, 15));
              return hasInjected;
            }
            if (code.includes('__testZadarkRuns')) {
              zadarkExecCount++;
            }
            return true;
          }
        }
      };

      const windowHandler = mockApp._listeners.get('browser-window-created')[0];
      windowHandler({}, mockWin);

      // Fire two overlapping dom-ready events
      await Promise.all([domReadyHandler(), domReadyHandler()]);
      assert.strictEqual(zadarkExecCount, 1, 'zadark.min.js must execute only once when dom-ready races');

      // Subsequent dom-ready after injection completes must see hasZaDark === true
      await domReadyHandler();
      assert.strictEqual(zadarkExecCount, 1, 'zadark.min.js must not re-execute once injected');
    });
  });
});
