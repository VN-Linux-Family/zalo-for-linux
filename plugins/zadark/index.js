'use strict';

const fs = require('fs');
const path = require('path');

const RENDERER_SCRIPT = fs.readFileSync(path.join(__dirname, 'renderer.js'), 'utf8');

const ZADARK_INDEX_CSS = [
  'css/zadark-fonts.min.css',
  'css/zadark.min.css',
  'css/zadark-popup.min.css'
];

const ZADARK_INDEX_SCRIPTS = [
  'libs/zadark-jquery.min.js',
  'libs/zadark-popper.min.js',
  'libs/zadark-tippy.min.js',
  'libs/zadark-hotkeys-js.min.js',
  'libs/zadark-toastify.min.js',
  'libs/zadark-webfont.min.js',
  'libs/zadark-introjs.min.js',
  'libs/zadark-localforage.min.js',
  'js/zadark-shared.min.js',
  'js/zadark-translate.min.js',
  'js/zadark-zconv.min.js',
  'js/zadark-reaction.min.js',
  'js/zadark.min.js'
];

const ZADARK_POPUP_CSS = [
  'css/zadark-fonts.min.css',
  'css/zadark.min.css',
  'css/zadark-popup.min.css'
];

const ZADARK_POPUP_SCRIPTS = [
  'libs/zadark-jquery.min.js',
  'libs/zadark-popper.min.js',
  'libs/zadark-tippy.min.js',
  'libs/zadark-hotkeys-js.min.js',
  'libs/zadark-toastify.min.js',
  'libs/zadark-webfont.min.js',
  'libs/zadark-introjs.min.js',
  'libs/zadark-localforage.min.js',
  'js/zadark-shared.min.js',
  'js/zadark-popup.min.js'
];

function isFlatpak() {
  return Boolean(process.env.FLATPAK_ID || fs.existsSync('/.flatpak-info'));
}

function resolveZaDarkDir(appDir) {
  // 1. Explicit override for testing or custom environments
  if (process.env.ZADARK_DIR && fs.existsSync(process.env.ZADARK_DIR)) {
    return process.env.ZADARK_DIR;
  }

  // 2. Flatpak extension mount point (/app/lib/zadark)
  if (isFlatpak()) {
    const flatpakExtDir = '/app/lib/zadark';
    if (fs.existsSync(flatpakExtDir)) {
      return flatpakExtDir;
    }
    return null;
  }

  // 3. Local pc-dist (AppImage ZaDark variant or extracted tree)
  if (appDir) {
    const localDir = path.join(appDir, 'pc-dist', 'zadark');
    if (fs.existsSync(localDir)) {
      return localDir;
    }
  }

  return null;
}

function isZaDarkInstalled(appDir) {
  return Boolean(resolveZaDarkDir(appDir));
}

async function injectZaDarkAssets(win, zadarkDir, type = 'index') {
  if (!win || win.isDestroyed() || !zadarkDir) return false;

  const cssList = type === 'popup' ? ZADARK_POPUP_CSS : ZADARK_INDEX_CSS;
  const scriptList = type === 'popup' ? ZADARK_POPUP_SCRIPTS : ZADARK_INDEX_SCRIPTS;

  // 1. Inject CSS with absolute font URLs
  for (const relCss of cssList) {
    if (win.isDestroyed()) return false;
    const fullCssPath = path.join(zadarkDir, relCss);
    if (!fs.existsSync(fullCssPath)) continue;

    try {
      let cssContent = fs.readFileSync(fullCssPath, 'utf8');
      const fontUrlPrefix = ('file://' + path.join(zadarkDir, 'fonts') + '/').replace(/\\/g, '/');
      cssContent = cssContent.replace(/\.\.\/fonts\//g, fontUrlPrefix);
      await win.webContents.insertCSS(cssContent);
    } catch (e) {
      console.warn(`[ZaDark Plugin] Could not insert CSS ${relCss}:`, e.message);
    }
  }

  // 2. Set required body class names
  if (win.isDestroyed()) return false;
  try {
    await win.webContents.executeJavaScript(`
      document.body.classList.add('zadark', 'zadark-pc', 'zadark-linux');
    `, true);
  } catch (e) {
    console.warn('[ZaDark Plugin] Could not add body classes:', e.message);
  }

  // 3. Execute JS scripts sequentially in main world
  for (const relScript of scriptList) {
    if (win.isDestroyed()) return false;
    const fullScriptPath = path.join(zadarkDir, relScript);
    if (!fs.existsSync(fullScriptPath)) continue;

    try {
      const scriptContent = fs.readFileSync(fullScriptPath, 'utf8');
      await win.webContents.executeJavaScript(scriptContent, true);
    } catch (e) {
      console.warn(`[ZaDark Plugin] Could not execute script ${relScript}:`, e.message);
    }
  }

  return true;
}

function register({ app, ipcMain, appDir }) {
  const zadarkDir = resolveZaDarkDir(appDir);
  if (!zadarkDir) {
    return;
  }

  // Ensure main process hooks (blocking typing/seen indicators) are loaded
  const zadarkMain = path.join(zadarkDir, 'js', 'zadark-main.min.js');
  if (fs.existsSync(zadarkMain)) {
    try {
      require(zadarkMain);
    } catch (err) {
      console.error('[ZaDark Plugin] Error loading zadark-main module:', err);
    }
  }

  app.on('browser-window-created', (_event, win) => {
    win.webContents.on('dom-ready', async () => {
      try {
        if (win.isDestroyed()) return;
        const url = win.webContents.getURL();
        const isIndex = url.includes('/pc-dist/index.html');
        const isPopup = url.includes('/pc-dist/popup-viewer.html');
        if (!isIndex && !isPopup) return;

        // Verify whether this window already has ZaDark loaded
        let hasZaDark = await win.webContents.executeJavaScript(`
          Boolean(
            window.__zadark ||
            document.querySelector('link[href*="zadark"]') ||
            document.querySelector('script[src*="zadark"]') ||
            document.documentElement.hasAttribute('data-zadark-theme') ||
            document.body.classList.contains('zadark')
          )
        `, true).catch(() => false);

        if (win.isDestroyed()) return;

        // If not loaded statically, inject dynamically from the resolved ZaDark directory
        if (!hasZaDark) {
          hasZaDark = await injectZaDarkAssets(win, zadarkDir, isPopup ? 'popup' : 'index');
        }

        if (!hasZaDark || win.isDestroyed()) return;

        // Inject the two-way theme synchronization script (main window only)
        if (isIndex) {
          await win.webContents.executeJavaScript(RENDERER_SCRIPT, true);
        }
      } catch (err) {
        console.error('[ZaDark Plugin] Error injecting renderer sync:', err);
      }
    });
  });
}

module.exports = {
  register,
  isZaDarkInstalled,
  resolveZaDarkDir,
  isFlatpak,
  injectZaDarkAssets
};
