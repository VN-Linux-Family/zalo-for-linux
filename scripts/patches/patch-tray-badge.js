const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { decodePng } = require('./lib/badge-icon');

const APP_DIR = path.join(__dirname, '..', '..', 'app');
const MAIN_DIST = path.join(APP_DIR, 'main-dist');

const LIB_SRC = path.join(__dirname, 'lib', 'badge-icon.js');
const LIB_DEST = path.join(MAIN_DIST, 'zalo-tray-badge-lib.js');
const BASE_DEST = path.join(MAIN_DIST, 'zalo-tray-base.png');

const PATCH_START = '// ZALO LINUX TRAY BADGE START';
const PATCH_END = '// ZALO LINUX TRAY BADGE END';

// Square Zalo logos shipped in the app, in preference order; the first one that
// decodes as a plain 8-bit PNG becomes the tray base. Keeping the logo out of
// this repo and sourcing it from the app avoids committing Zalo's artwork.
const BASE_CANDIDATES = [
  'pc-dist/favicon-256x256.png',
  'pc-dist/android-icon-192x192.png',
  'pc-dist/apple-icon-180x180.png',
  'pc-dist/favicon-512x512.png',
];

function pickBaseIcon() {
  for (const rel of BASE_CANDIDATES) {
    const src = path.join(APP_DIR, rel);
    if (!fs.existsSync(src)) continue;
    try {
      decodePng(fs.readFileSync(src));
      return src;
    } catch (_) {
      // Unsupported encoding for this candidate; try the next.
    }
  }
  return null;
}

async function main() {
  const mainPath = path.join(MAIN_DIST, 'main.js');

  if (!fs.existsSync(mainPath)) {
    logger.warn('main-dist/main.js not present, skipping tray badge patch');
    return;
  }

  const baseSrc = pickBaseIcon();
  if (!baseSrc) {
    logger.warn('no usable base icon found, skipping tray badge patch');
    return;
  }

  fs.copySync(baseSrc, BASE_DEST);
  fs.copySync(LIB_SRC, LIB_DEST);

  let content = fs.readFileSync(mainPath, 'utf8');
  const trayScript = getTrayBadgeScript();

  if (content.includes(PATCH_START) && content.includes(PATCH_END)) {
    const start = content.indexOf(PATCH_START);
    const end = content.indexOf(PATCH_END, start) + PATCH_END.length;
    content = trayScript + content.slice(end).replace(/^\r?\n/, '');
    fs.writeFileSync(mainPath, content, 'utf8');
    logger.dim('Updated main.js: tray unread-count badge module');
    return;
  }

  content = trayScript + content;
  fs.writeFileSync(mainPath, content, 'utf8');
  logger.dim('Patched main.js: tray unread-count badge module');
}

function getTrayBadgeScript() {
  return `${PATCH_START}
try {
  const electron = require('electron');
  const { app, nativeImage } = electron;

  if (process.platform === 'linux') {
    const path = require('path');
    const fs = require('fs');

    const BASE_ICON_PATH = path.join(__dirname, 'zalo-tray-base.png');

    let renderTrayBadge = null;
    try {
      ({ renderTrayBadge } = require('./zalo-tray-badge-lib.js'));
    } catch (_) {}

    let _tray = null;
    let _baseBuffer = null;
    let _baseImage = null;
    let _lastCount = -1;

    function loadBase() {
      if (_baseBuffer) return true;
      try {
        _baseBuffer = fs.readFileSync(BASE_ICON_PATH);
        _baseImage = nativeImage.createFromBuffer(_baseBuffer);
        return true;
      } catch (_) {
        return false;
      }
    }

    function trayAlive() {
      return _tray && (typeof _tray.isDestroyed !== 'function' || !_tray.isDestroyed());
    }

    function applyBadge(count) {
      if (!trayAlive() || !renderTrayBadge || !loadBase()) return;
      try {
        if (count > 0) {
          const buf = renderTrayBadge(_baseBuffer, count);
          _tray.setImage(nativeImage.createFromBuffer(buf));
        } else {
          _tray.setImage(_baseImage);
        }
      } catch (_) {}
    }

    function setCount(rawCount) {
      const parsed = Number.parseInt(rawCount, 10);
      const count = Number.isFinite(parsed) && parsed > 0 ? Math.min(parsed, 9999) : 0;
      if (count === _lastCount) return;
      _lastCount = count;
      applyBadge(count);
    }

    // Capture Zalo's own tray instance by wrapping the Tray constructor, so the
    // badge is drawn on the same icon the app manages (no second tray entry).
    const OrigTray = electron.Tray;
    if (OrigTray) {
      class PatchedTray extends OrigTray {
        constructor(...args) {
          super(...args);
          _tray = this;
          if (_lastCount > 0) setTimeout(() => applyBadge(_lastCount), 0);
        }
      }
      try {
        Object.defineProperty(electron, 'Tray', {
          configurable: true,
          get() { return PatchedTray; },
          set() {},
        });
      } catch (_) {}
    }

    // On Linux the unread total reaches the app through app.setBadgeCount; mirror
    // it onto the tray while preserving the original launcher-badge behaviour.
    const initTrayBadge = () => {
      try {
        const original = typeof app.setBadgeCount === 'function' ? app.setBadgeCount.bind(app) : null;
        app.setBadgeCount = (count) => {
          try { setCount(count); } catch (_) {}
          return original ? original(count) : false;
        };
      } catch (_) {}
    };

    if (app.isReady()) {
      initTrayBadge();
    } else {
      app.whenReady().then(initTrayBadge);
    }
  }
} catch (err) {
  console.error('Failed to apply Zalo Linux Tray Badge Patch:', err);
}
${PATCH_END}
`;
}

if (require.main === module) {
  main();
}

module.exports = { main };
