const fs = require('fs-extra');
const path = require('path');
let logger;
try {
  logger = require('../utils/logger');
} catch (_) {
  logger = {
    info: (...args) => console.log('[INFO]', ...args),
    warn: (...args) => console.warn('[WARN]', ...args),
    error: (...args) => console.error('[ERROR]', ...args),
    success: (...args) => console.log('[SUCCESS]', ...args),
    dim: (...args) => console.log(' ', ...args)
  };
}

const APP_DIR = path.join(__dirname, '..', '..', 'app');

/**
 * Patch to remove default Electron application menu (Zalo, File, View, Window)
 * and eliminate extra window frame / title bar space, ensuring clean native frameless UI.
 */
async function main() {
  const mainJsPath = path.join(APP_DIR, 'main-dist', 'main.js');
  const bootstrapPath = path.join(APP_DIR, 'bootstrap.js');

  // 1. Patch bootstrap.js to ensure Menu.setApplicationMenu(null) globally
  if (fs.existsSync(bootstrapPath)) {
    let bootstrapContent = fs.readFileSync(bootstrapPath, 'utf8');
    if (!bootstrapContent.includes('Menu.setApplicationMenu(null)')) {
      bootstrapContent = `try {
  const { Menu, app } = require('electron');
  if (Menu && Menu.setApplicationMenu) Menu.setApplicationMenu(null);
  if (app) {
    app.on('browser-window-created', (_, win) => {
      try {
        win.setMenuBarVisibility(false);
        if (win.removeMenu) win.removeMenu();
        win.autoHideMenuBar = true;
      } catch (_) {}
    });
  }
} catch (_) {}\n` + bootstrapContent;
      fs.writeFileSync(bootstrapPath, bootstrapContent, 'utf8');
      logger.dim('Patched bootstrap.js with global menu suppressor');
    }
  }

  // 2. Patch main-dist/main.js
  if (!fs.existsSync(mainJsPath)) {
    logger.warn('main.js not present, skipping remove-menu patch');
    return;
  }

  let content = fs.readFileSync(mainJsPath, 'utf8');
  let changed = false;

  // Suppress application menu build in SGD2
  if (content.includes('i.setApplicationMenu(i.buildFromTemplate(a))')) {
    content = content.replace(
      'i.setApplicationMenu(i.buildFromTemplate(a))',
      '(e&&e.setMenuBarVisibility&&e.setMenuBarVisibility(!1),e&&e.removeMenu&&e.removeMenu(),i.setApplicationMenu(null))'
    );
    changed = true;
  }

  // Ensure BrowserWindow instance Ae removes menu and hides menu bar
  const anchorAe = 'Ae=m.createWithMultiWindow(i,o,gn,oe(),t),g(Ae),v(Ae.webContents),et.setMainWindow(Ae);';
  if (content.includes(anchorAe) && !content.includes('Ae.removeMenu')) {
    content = content.replace(
      anchorAe,
      `${anchorAe}try{Ae.removeMenu&&Ae.removeMenu();Ae.setMenuBarVisibility&&Ae.setMenuBarVisibility(!1);Ae.autoHideMenuBar=!0}catch(_){};`
    );
    changed = true;
  }

  // Restore clean frameless window (removes extra mutter title bar)
  if (content.includes('T,frame:!0')) {
    content = content.replace(/T,frame:!0/g, 'T,frame:!1');
    changed = true;
  }

  if (changed) {
    fs.writeFileSync(mainJsPath, content, 'utf8');
    logger.success('Removed menu bar and restored clean frameless UI in main.js');
  }
}

if (require.main === module) {
  main();
}

module.exports = { main };
