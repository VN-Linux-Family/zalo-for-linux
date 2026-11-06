'use strict';

const fs = require('fs');
const path = require('path');

const RENDERER_SCRIPT = fs.readFileSync(path.join(__dirname, 'renderer.js'), 'utf8');

function isZaDarkInstalled(appDir) {
  if (!appDir) return false;
  const zadarkDir = path.join(appDir, 'pc-dist', 'zadark');
  return fs.existsSync(zadarkDir);
}

function register({ app, ipcMain, appDir }) {
  // If ZaDark assets are not installed in app/pc-dist, do not register hooks (regular app)
  if (appDir && !isZaDarkInstalled(appDir)) {
    return;
  }

  app.on('browser-window-created', (_event, win) => {
    win.webContents.on('dom-ready', async () => {
      try {
        if (win.isDestroyed()) return;
        const url = win.webContents.getURL();
        if (!url.includes('/pc-dist/index.html')) return;

        // Verify that this window has ZaDark loaded
        const hasZaDark = await win.webContents.executeJavaScript(`
          Boolean(
            window.__zadark ||
            document.querySelector('link[href*="zadark"]') ||
            document.querySelector('script[src*="zadark"]') ||
            document.documentElement.hasAttribute('data-zadark-theme') ||
            document.body.classList.contains('zadark')
          )
        `, true).catch(() => false);

        if (!hasZaDark || win.isDestroyed()) return;

        // Inject the two-way theme synchronization script
        await win.webContents.executeJavaScript(RENDERER_SCRIPT, true);
      } catch (err) {
        console.error('[ZaDark Sync Plugin] Error injecting renderer sync:', err);
      }
    });
  });
}

module.exports = { register, isZaDarkInstalled };
