'use strict';

const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');

const RENDERER_SCRIPT = fs.readFileSync(path.join(__dirname, 'renderer.js'), 'utf8');

const ASSETS_BY_TYPE = {
  index: {
    css: [
      'css/zadark-fonts.min.css',
      'css/zadark.min.css',
      'css/zadark-popup.min.css'
    ],
    scripts: [
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
    ]
  },
  popup: {
    css: [],
    scripts: [
      'js/zadark-popup-viewer.min.js'
    ]
  },
  znotification: {
    css: [
      'css/zadark-znotification.min.css'
    ],
    scripts: [
      'libs/zadark-webfont.min.js',
      'js/zadark-znotification.min.js'
    ]
  }
};

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

function toBaseFileUrl(zadarkDir) {
  return pathToFileURL(path.resolve(zadarkDir)).href.replace(/\/+$/, '');
}

/**
 * Resolve ZaDark version metadata for `data-zadark-version` on `<html>`.
 */
function resolveZaDarkVersion(zadarkDir, appDir) {
  if (process.env.ZADARK_VERSION) {
    return process.env.ZADARK_VERSION;
  }

  const candidates = [
    zadarkDir && path.join(zadarkDir, 'package.json'),
    zadarkDir && path.join(zadarkDir, 'build-info.json'),
    zadarkDir && path.join(zadarkDir, '..', 'package.json'),
    appDir && path.join(appDir, 'pc-dist', 'build-info.json')
  ].filter(Boolean);

  for (const file of candidates) {
    if (!fs.existsSync(file)) continue;
    try {
      const data = JSON.parse(fs.readFileSync(file, 'utf8'));
      const ver = data.zadarkVersion || data.version;
      if (ver) return String(ver);
    } catch (_) {
      // Ignore malformed metadata files and fall through
    }
  }

  return '';
}

/**
 * Rewrite relative CSS asset URLs (e.g. `../fonts/`) to absolute `file://` URLs
 * rooted at `zadarkDir`, so `insertCSS` does not resolve them against `pc-dist/`.
 */
function rewriteZaDarkCssPaths(cssContent, zadarkDir) {
  const baseUrl = toBaseFileUrl(zadarkDir);
  return cssContent.replace(/\.\.\/(fonts|images)\//g, `${baseUrl}/$1/`);
}

/**
 * Rewrite relative `zadark/...` paths inside ZaDark renderer scripts:
 * 1. Node `require('zadark/...')` -> absolute filesystem path in `zadarkDir`.
 * 2. Browser asset literals (`'zadark/css/...'`, `'zadark/images/...'`, etc.) -> `file://` URLs.
 * Leaves non-asset keys such as `'zadark/reaction-recent-emojis'` untouched.
 */
function rewriteZaDarkScriptPaths(scriptContent, zadarkDir) {
  const fsPrefix = path.resolve(zadarkDir).replace(/\\/g, '/').replace(/\/+$/, '');
  const fileUrlPrefix = toBaseFileUrl(zadarkDir);

  const withRequireRewritten = scriptContent.replace(
    /require\((['"])zadark\//g,
    `require($1${fsPrefix}/`
  );

  return withRequireRewritten.replace(
    /(['"])zadark\/(css|images|fonts|libs|js)\//g,
    `$1${fileUrlPrefix}/$2/`
  );
}

function resolveWindowType(url) {
  if (typeof url !== 'string') return null;
  if (url.includes('/pc-dist/index.html')) return 'index';
  if (url.includes('/pc-dist/popup-viewer.html')) return 'popup';
  if (url.includes('/pc-dist/znotification.html')) return 'znotification';
  return null;
}

async function injectZaDarkAssets(win, zadarkDir, type = 'index', options = {}) {
  if (!win || win.isDestroyed() || !zadarkDir) return false;

  const assetBundle = ASSETS_BY_TYPE[type] || ASSETS_BY_TYPE.index;
  const zadarkVersion = options.zadarkVersion !== undefined
    ? options.zadarkVersion
    : resolveZaDarkVersion(zadarkDir, options.appDir);

  // 1. Inject CSS with absolute asset URLs
  for (const relCss of assetBundle.css) {
    if (win.isDestroyed()) return false;
    const fullCssPath = path.join(zadarkDir, relCss);
    if (!fs.existsSync(fullCssPath)) continue;

    try {
      const rawCss = fs.readFileSync(fullCssPath, 'utf8');
      const cssContent = rewriteZaDarkCssPaths(rawCss, zadarkDir);
      await win.webContents.insertCSS(cssContent);
    } catch (e) {
      console.warn(`[ZaDark Plugin] Could not insert CSS ${relCss}:`, e.message);
    }
  }

  // 2. Set required body class names and root metadata attributes
  // ZaDark popup SCSS matches html[data-zadark-os="Windows"] for Ctrl+* hotkey hints.
  if (win.isDestroyed()) return false;
  try {
    await win.webContents.executeJavaScript(`
      window.__zflZadarkInjected = true;
      document.documentElement.setAttribute('data-zadark-os', 'Windows');
      document.documentElement.setAttribute('data-zadark-version', ${JSON.stringify(zadarkVersion)});
      document.body.classList.add('zadark', 'zadark-pc', 'zadark-linux');
    `, true);
  } catch (e) {
    console.warn('[ZaDark Plugin] Could not add body classes:', e.message);
  }

  // 3. Execute JS scripts sequentially in main world with rewritten asset paths
  for (const relScript of assetBundle.scripts) {
    if (win.isDestroyed()) return false;
    const fullScriptPath = path.join(zadarkDir, relScript);
    if (!fs.existsSync(fullScriptPath)) continue;

    try {
      const rawScript = fs.readFileSync(fullScriptPath, 'utf8');
      const scriptContent = rewriteZaDarkScriptPaths(rawScript, zadarkDir);
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

  const zadarkVersion = resolveZaDarkVersion(zadarkDir, appDir);

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
    const inFlight = new WeakSet();

    win.webContents.on('dom-ready', async () => {
      if (win.isDestroyed() || inFlight.has(win.webContents)) return;
      inFlight.add(win.webContents);

      try {
        const windowType = resolveWindowType(win.webContents.getURL());
        if (!windowType) return;

        // Verify whether this window already has ZaDark loaded
        let hasZaDark = await win.webContents.executeJavaScript(`
          Boolean(
            window.__zadark ||
            window.__zflZadarkInjected ||
            document.getElementById('div_Main_TabZaDark') ||
            document.querySelector('link[href*="zadark"]') ||
            document.querySelector('script[src*="zadark"]') ||
            document.documentElement.hasAttribute('data-zadark-theme') ||
            document.body.classList.contains('zadark')
          )
        `, true).catch(() => false);

        if (win.isDestroyed()) return;

        // If not loaded statically, inject dynamically from the resolved ZaDark directory
        if (!hasZaDark) {
          hasZaDark = await injectZaDarkAssets(win, zadarkDir, windowType, { appDir, zadarkVersion });
        }

        if (!hasZaDark || win.isDestroyed()) return;

        // Inject the two-way theme synchronization script (main window only)
        if (windowType === 'index') {
          await win.webContents.executeJavaScript(RENDERER_SCRIPT, true);
        }
      } catch (err) {
        console.error('[ZaDark Plugin] Error injecting renderer sync:', err);
      } finally {
        inFlight.delete(win.webContents);
      }
    });
  });
}

module.exports = {
  register,
  isZaDarkInstalled,
  resolveZaDarkDir,
  resolveZaDarkVersion,
  resolveWindowType,
  isFlatpak,
  rewriteZaDarkCssPaths,
  rewriteZaDarkScriptPaths,
  injectZaDarkAssets,
  ASSETS_BY_TYPE
};
