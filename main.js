const { app, BrowserWindow, Menu, Tray, ipcMain, nativeImage, screen, dialog } = require('electron');
const { registerMainTransport } = require('./plugins/language-core/main-transport');
const { buildTrayMenuTemplate } = require('./plugins/tray-menu');
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
app.setName('Zalo');
let tray = null;
let mainWindow = null;
let isAppQuitting = false;
let devMode = null;
let trayLocale = null;
const ZALO_LANGUAGE_PAGE_PATTERN = /\/pc-dist\/(?:index|child|login|compact-app)\.html(?:[?#]|$)/;
const ZALO_PRIMARY_LANGUAGE_PAGE_PATTERN = /\/pc-dist\/(?:index|login)\.html(?:[?#]|$)/;
function getLanguageSourceWindows() {
  try {
    const candidates = BrowserWindow.getAllWindows().filter((win) => {
      try {
        return win && !win.isDestroyed() && win.webContents &&
          ZALO_PRIMARY_LANGUAGE_PAGE_PATTERN.test(win.webContents.getURL());
      } catch (_) { return false; }
    });
    if (mainWindow && !mainWindow.isDestroyed() && candidates.includes(mainWindow)) {
      return [mainWindow];
    }
    // The app-usage helper may be created before the actual Zalo page; never
    // authorize it as a locale source. Fall back only to a primary UI page.
    return candidates.slice(0, 1);
  } catch (_) {
    return [];
  }
}
const languageTransport = registerMainTransport({
  ipcMain,
  // Only the primary index/login renderer owns the app-wide locale. Child and
  // Compact App renderers receive snapshots through the relay below.
  getWindows: getLanguageSourceWindows,
  onLocale: (locale, event) => {
    // Ignore renderer-to-main echoes after a locale has already been fanned out.
    const previousLocale = trayLocale;
    if (previousLocale === locale) return;
    trayLocale = locale;

    // Relay the authoritative locale snapshot to the other Zalo UI renderers.
    // Their Language Core instances then update renderer-owned plugins locally.
    const source = event && event.sender;
    const state = JSON.stringify({ version: 1, locale, timestamp: Date.now() });
    const relayScript = `(() => { const state = ${state}; window.__ZALO_LANGUAGE_BRIDGE__ = state; window.dispatchEvent(new CustomEvent('zalo-language:changed', { detail: state })); })();`;
    for (const win of BrowserWindow.getAllWindows()) {
      try {
        if (!win || win.isDestroyed() || !win.webContents || win.webContents === source ||
            !ZALO_LANGUAGE_PAGE_PATTERN.test(win.webContents.getURL())) continue;
        win.webContents.executeJavaScript(relayScript, true).catch((error) => {
          console.warn('[language-core] Cannot relay locale to Zalo renderer:', error.message);
        });
      } catch (error) {
        console.warn('[language-core] Cannot relay locale to Zalo renderer:', error.message);
      }
    }

    // In-memory snapshot only; refresh native/plugin-owned UI without persisting locale.
    if (plugins && typeof plugins.updateLanguage === 'function') plugins.updateLanguage(locale);
    updateTrayVisibilityMenu();
    if (zcall && typeof zcall.onLanguageUpdateRequest === 'function') {
      zcall.onLanguageUpdateRequest({ type: 'LANGUAGE_UPDATE_REQUEST', requestId: 'main-zcall-' + Date.now(), locale, previousLocale, source: 'zalo', timestamp: Date.now() });
    }
  }
});

// Hidden windows Zalo uses as background processes, never the main window.
const BACKGROUND_WINDOW_TITLES = ['Shared Worker', 'SQLite'];

// Linux optimizations & environment defaults
if (process.platform === 'linux') {
  if (process.env.FLATPAK_ID || fs.existsSync('/.flatpak-info')) {
    // Electron 22 crashes in Notification::Close() with libnotify's portal backend.
    // Use the already permitted org.freedesktop.Notifications service instead.
    process.env.NOTIFY_IGNORE_PORTAL = '1';
  }
  const uid = process.getuid ? process.getuid() : 1000;
  const runtimeDir = process.env.XDG_RUNTIME_DIR || `/run/user/${uid}`;
  if (!process.env.PULSE_SERVER) {
    process.env.PULSE_SERVER = `unix:${runtimeDir}/pulse/native`;
  }
}

// ---------------------------------------------------------------------------
// Plugins
// ---------------------------------------------------------------------------

const plugins = require('./plugins');
const zcall = require('./plugins/zcall');
const trayHost = require('./plugins/tray-host');
// Created with the main window: the screen module is not usable before 'ready'.
let windowState = null;
const startHidden = require('./plugins/start-hidden').createStartHiddenController({
  onMaximize: () => { if (windowState) windowState.requestMaximize(); }
});

// Before Zalo's bootstrap: the main window's frame option depends on it.
plugins.initEarly({ app, ipcMain });

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

function updateTrayVisibilityMenu() {
  if (!tray) return;
  const visible = Boolean(
    mainWindow &&
    !mainWindow.isDestroyed() &&
    mainWindow.isVisible() &&
    !mainWindow.isMinimized()
  );
  const contextMenu = Menu.buildFromTemplate(buildTrayMenuTemplate({
    locale: trayLocale,
    visible,
    // Dev Mode owns these items. Pass them through without inspection or mutation.
    devModeItems: devMode ? devMode.getTrayMenuItems() : [],
    onToggleVisibility: () => {
      if (mainWindow && !mainWindow.isDestroyed() && mainWindow.isVisible() && !mainWindow.isMinimized()) {
        mainWindow.hide();
      } else {
        showMainWindow();
      }
      updateTrayVisibilityMenu();
    },
    onQuit: () => {
      isAppQuitting = true;
      if (tray) {
        tray.destroy();
        tray = null;
      }
      app.quit();
    }
  }));
  tray.setContextMenu(contextMenu);
}

// ---------------------------------------------------------------------------
// App lifecycle
// ---------------------------------------------------------------------------

// Launching Zalo again (dock, menu, notification) starts a second instance
// that only hands its arguments to the running one: Zalo's
// second-instance.js quits it during bootstrap, but 'ready' and 'before-quit'
// still fire in it. It must not start the tray or the plugins, nor stop the
// running instance's call engine.
function isPrimaryInstance() {
  return app.hasSingleInstanceLock();
}

app.on('before-quit', () => {
  isAppQuitting = true;
  if (tray) {
    tray.destroy();
    tray = null;
  }
});

// Zalo cancels the first quit to let the renderer save its state, then quits
// again, so 'before-quit' fires twice; 'will-quit' fires once, after every
// window is closed. The native call engine hangs up a call in progress.
app.on('will-quit', () => {
  languageTransport.dispose();
  if (isPrimaryInstance()) zcall.stop();
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
      plugins.onMainWindowCreated(win);

      if (!windowState) {
        windowState = require('./plugins/window-state').createWindowStateController({
          screen,
          // Native Wayland windows cannot be moved by the app (plugins/wayland-titlebar).
          canPosition: !global.__zaloNativeWayland,
          stateFile: path.join(app.getPath('userData'), 'zalo-linux-window-state.json')
        });
      }
      win.on('ready-to-show', updateTrayVisibilityMenu);
      win.on('show', updateTrayVisibilityMenu);
      win.on('hide', updateTrayVisibilityMenu);
      win.on('minimize', updateTrayVisibilityMenu);
      win.on('restore', updateTrayVisibilityMenu);
      win.on('focus', updateTrayVisibilityMenu);
      win.on('closed', () => {
        if (mainWindow === win) mainWindow = null;
        updateTrayVisibilityMenu();
      });
      windowState.attach(win);

      // Only start hidden when the tray exists, otherwise the window
      // would be unreachable.
      if (tray && trayHost.isAvailable()) startHidden.attach(win);
      updateTrayVisibilityMenu();

    }

    // Minimize to tray instead of closing.
    // The 50ms delay lets `event.preventDefault()` settle before hiding —
    // hiding immediately causes "Show" to be a no-op on some Linux DEs
    // (fixes #27).
    win.on('close', (event) => {
      if (isAppQuitting) return;
      // Not the call windows ("Zalo Call - …", the incoming notice): closing those ends / declines the call.
      if (tray && trayHost.isAvailable() && !win.zcallWindow && (win === mainWindow || win.getTitle().includes('Zalo'))) {
        event.preventDefault();
        setTimeout(() => {
          if (!isAppQuitting && !win.isDestroyed()) {
            win.hide();
          }
        }, 50);
      } else if (win === mainWindow) {
        // No tray host (stock GNOME): the tray icon is invisible, so a hidden
        // window could never be reopened or quit. Quit instead.
        event.preventDefault();
        isAppQuitting = true;
        setImmediate(() => app.quit());
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
  if (!isPrimaryInstance()) return;
  try { Menu.setApplicationMenu(null); } catch (_) { }

  trayHost.init();

  if (fs.existsSync(iconPath)) {
    try {
      tray = new Tray(iconPath);
      tray.setToolTip('Zalo');
      tray.on('click', showMainWindow);
      tray.on('double-click', showMainWindow);
      updateTrayVisibilityMenu();
    } catch (e) {
      console.error('Tray init failed:', e);
    }
  }

  // Register plugins once with the complete Electron context.
  devMode = plugins.register({
    app,
    ipcMain,
    BrowserWindow,
    dialog,
    nativeImage,
    tray,
    appDir,
    iconPath,
    onTrayMenuChanged: updateTrayVisibilityMenu
  });
  updateTrayVisibilityMenu();

  zcall.start({ userDataDir: app.getPath('userData') });
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
zcall.configure(app); // Chromium switches (PipeWire screen capture on Wayland): after Zalo's own
