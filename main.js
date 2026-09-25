const { app, BrowserWindow, Menu, Tray, ipcMain } = require('electron');
const path = require('path');
const fs = require('fs');

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

const appDir = fs.existsSync(path.join(__dirname, 'app'))
  ? path.join(__dirname, 'app')
  : path.join(path.dirname(process.execPath), 'app');

const iconPath = path.join(appDir, 'pc-dist', 'favicon-512x512.png');

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------
app.setName('zalo');
let tray = null;
let mainWindow = null;
let isAppQuitting = false;

// Hidden windows Zalo uses as background processes, never the main window.
const BACKGROUND_WINDOW_TITLES = ['Shared Worker', 'SQLite'];

// Linux optimizations & environment defaults
if (process.platform === 'linux') {
  const uid = process.getuid ? process.getuid() : 1000;
  const runtimeDir = process.env.XDG_RUNTIME_DIR || `/run/user/${uid}`;
  if (!process.env.PULSE_SERVER) {
    process.env.PULSE_SERVER = `unix:${runtimeDir}/pulse/native`;
  }
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

const screenshotPlugin = require('./plugins/screenshot');
const launcherBadgePlugin = require('./plugins/launcher-badge');
const userscriptsPlugin = require('./plugins/userscripts');
const zcallBridgePlugin = require('./plugins/zcall-bridge');
const startHidden = require('./plugins/start-hidden').createStartHiddenController();

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function toggleDevTools() {
  try {
    const win = BrowserWindow.getFocusedWindow() || mainWindow;
    if (win && win.webContents) {
      if (win.webContents.isDevToolsOpened()) {
        win.webContents.closeDevTools();
      } else {
        win.webContents.openDevTools({ mode: 'detach' });
      }
    }
  } catch (e) {
    console.error('Toggle DevTools failed', e);
  }
}

function showMainWindow() {
  startHidden.release();
  if (!mainWindow || mainWindow.isDestroyed()) return;
  if (mainWindow.isMinimized()) mainWindow.restore();
  if (!mainWindow.isVisible()) mainWindow.show();
  mainWindow.focus();
  mainWindow.moveTop();
  try {
    mainWindow.webContents.send('show-from-tray');
  } catch (e) {
    console.error('Failed to send show-from-tray:', e);
  }
}

// ---------------------------------------------------------------------------
// App lifecycle
// ---------------------------------------------------------------------------

app.on('before-quit', () => {
  isAppQuitting = true;
  zcallBridgePlugin.shutdown();
  if (tray) {
    tray.destroy();
    tray = null;
  }
});

// Registered before Zalo's bootstrap, so this runs before Zalo's own
// second-instance handler tries to show the window.
app.on('second-instance', () => {
  startHidden.release();
});

app.on('browser-window-created', (_evt, win) => {
  try {
    if (fs.existsSync(iconPath)) {
      win.setIcon(iconPath);
    }

    win.setMenuBarVisibility(false);
    if (win.removeMenu) win.removeMenu();
    win.autoHideMenuBar = true;

    // Track the main Zalo window for tray menu
    if (!mainWindow && !BACKGROUND_WINDOW_TITLES.includes(win.getTitle())) {
      mainWindow = win;
      screenshotPlugin.setMainWindow(win);

      // Only start hidden when the tray exists, otherwise the window
      // would be unreachable.
      if (tray) startHidden.attach(win);

      mainWindow.webContents.on('before-input-event', (_event, input) => {
        if ((input.control) && input.shift && input.key.toLowerCase() === 'i') {
          toggleDevTools();
        }
      });

      if (tray) {
        const contextMenu = Menu.buildFromTemplate([
          {
            label: 'Mở Zalo',
            click: showMainWindow
          },
          {
            label: 'Ẩn Zalo',
            click: () => {
              if (mainWindow && !mainWindow.isDestroyed()) {
                mainWindow.hide();
              }
            }
          },
          {
            label: 'Toggle DevTools',
            click: toggleDevTools
          },
          {
            label: 'Cài đặt gọi điện…',
            click: () => {
              zcallBridgePlugin.openSetupDialog({ userDataDir: app.getPath('userData') });
            }
          },
          {
            label: 'Thoát',
            click: () => {
              isAppQuitting = true;
              if (tray) {
                tray.destroy();
                tray = null;
              }
              app.quit();
            }
          }
        ]);
        tray.setContextMenu(contextMenu);
      }
    }

    // Minimize to tray instead of closing.
    // The 50ms delay lets `event.preventDefault()` settle before hiding —
    // hiding immediately causes "Show" to be a no-op on some Linux DEs
    // (fixes #27).
    win.on('close', (event) => {
      if (!isAppQuitting && tray && (win === mainWindow || win.getTitle().includes('Zalo'))) {
        event.preventDefault();
        setTimeout(() => {
          if (!isAppQuitting && !win.isDestroyed()) {
            win.hide();
          }
        }, 50);
      }
    });
  } catch (e) {
    console.error('Error in browser-window-created:', e);
  }
});

// ---------------------------------------------------------------------------
// Ready
// ---------------------------------------------------------------------------

app.once('ready', () => {
  try { Menu.setApplicationMenu(null); } catch (_) { }

  if (fs.existsSync(iconPath)) {
    try {
      tray = new Tray(iconPath);
      tray.setToolTip('Zalo');
      tray.on('click', showMainWindow);
      tray.on('double-click', showMainWindow);
    } catch (e) {
      console.error('Tray init failed:', e);
    }
  }

// Register plugins
  launcherBadgePlugin.register({ app, ipcMain });
  screenshotPlugin.register({ ipcMain });
  userscriptsPlugin.register({ app, ipcMain, BrowserWindow });
  zcallBridgePlugin.launch({ userDataDir: app.getPath('userData') });
});

// ---------------------------------------------------------------------------
// Bootstrap Zalo
// ---------------------------------------------------------------------------

function bootstrap() {
  const bootstrapPath = path.join(appDir, 'bootstrap.js');
  if (!fs.existsSync(bootstrapPath)) {
    console.error('Zalo bootstrap.js not found at:', bootstrapPath);
    return;
  }
  process.chdir(appDir);
  try {
    require(bootstrapPath);
  } catch (e) {
    console.error('Error loading Zalo:', e);
  }
}

bootstrap();
