/**
 * plugins/screenshot/index.js
 *
 * Built-in Zalo screenshot plugin for Linux (Wayland & X11).
 * Features standalone silent screen capture and native Zalo screenshot UI
 * without any third-party screenshot software dependencies.
 */

'use strict';

const {
  app,
  BrowserWindow,
  clipboard,
  desktopCapturer,
  dialog,
  ipcMain,
  nativeImage,
  screen
} = require('electron');
const fs = require('fs');
const path = require('path');
const { pathToFileURL } = require('url');
const { captureScreen } = require('./capture');

let _mainWindow = null;
let _overlayWindow = null;
let _isCapturing = false;

function ensureFont() {
  const fontPath = path.join(__dirname, 'ui', 'fonts', 'zalo-pc-screencap.ttf');
  if (fs.existsSync(fontPath)) return;

  const scriptPath = path.join(__dirname, '..', '..', 'scripts', 'extract-screenshot-assets.js');
  if (fs.existsSync(scriptPath)) {
    try {
      require(scriptPath).main();
    } catch (_) {}
  }
}

function register({ ipcMain: customIpcMain } = {}) {
  ensureFont();
  const ipc = customIpcMain || ipcMain;

  const originalHandle = ipc.handle.bind(ipc);
  ipc.handle = function (channel, handler) {
    if (channel === 'screen-capture') {
      const wrappedHandler = async (event, ...args) => {
        if (_isCapturing) {
          if (_overlayWindow && !_overlayWindow.isDestroyed()) {
            _overlayWindow.focus();
          }
          return true;
        }

        const opts = args[0] || {};
        const hideWindow = opts && opts.captureMode === false;

        let success = false;
        try {
          _isCapturing = true;

          // Hide or minimize main window if requested
          if (hideWindow && _mainWindow && !_mainWindow.isDestroyed()) {
            if (global.__zaloNativeWayland || process.env.FLATPAK_ID || fs.existsSync('/.flatpak-info')) {
              _mainWindow.minimize();
            } else {
              _mainWindow.hide();
            }
            // Settle time for WM to unmap window from framebuffer
            await new Promise((r) => setTimeout(r, 180));
          }

          success = await startInteractiveScreenshot(event);
          if (!success) {
            console.warn('[Screenshot Plugin] Screenshot session ended without accepting image');
          }
        } catch (e) {
          console.error('[Screenshot Plugin] Critical failure in screenshot session:', e.stack || e);
        } finally {
          _isCapturing = false;
          restoreMainWindow(hideWindow);
        }

        // Auto-paste captured screenshot into chat if accepted
        if (success) {
          const targetSender = (event && event.sender && !event.sender.isDestroyed())
            ? event.sender
            : (_mainWindow && !_mainWindow.isDestroyed() ? _mainWindow.webContents : null);

          if (targetSender && !targetSender.isDestroyed()) {
            setTimeout(() => {
              try {
                if (!targetSender.isDestroyed()) {
                  targetSender.send('zalo-linux-auto-paste-screenshot');
                }
              } catch (_) {}
            }, 300);
          }
        }

        return true;
      };
      return originalHandle(channel, wrappedHandler);
    }
    return originalHandle(channel, handler);
  };
}

function restoreMainWindow(wasHidden) {
  if (wasHidden && _mainWindow && !_mainWindow.isDestroyed()) {
    try {
      if (_mainWindow.isMinimized()) _mainWindow.restore();
      _mainWindow.show();
      _mainWindow.focus();
      _mainWindow.moveTop();
      if (!_mainWindow.webContents.isDestroyed()) {
        _mainWindow.webContents.send('show-from-tray');
      }
    } catch (e) {
      console.error('[Screenshot Plugin] Restore error:', e);
    }
  }
}

async function startInteractiveScreenshot(event) {
  const targetDir = path.join(app.getPath('userData'), 'screenshot-cache');
  const capturePath = await captureScreen({
    desktopCapturer,
    screen,
    targetDir
  });

  if (!capturePath || !fs.existsSync(capturePath)) {
    console.error('[Screenshot Plugin] Screen capture failed: no valid screenshot image was produced.');
    return false;
  }

  const fileUrl = pathToFileURL(capturePath).href;

  return new Promise((resolve) => {
    let resolved = false;

    const cursorPt = screen.getCursorScreenPoint();
    const activeDisplay = screen.getDisplayNearestPoint(cursorPt) || screen.getPrimaryDisplay();
    const bounds = activeDisplay.bounds;

    _overlayWindow = new BrowserWindow({
      x: bounds.x,
      y: bounds.y,
      width: bounds.width,
      height: bounds.height,
      frame: false,
      transparent: false,
      backgroundColor: '#000000',
      alwaysOnTop: true,
      fullscreen: true,
      skipTaskbar: true,
      resizable: false,
      movable: false,
      hasShadow: false,
      show: false,
      webPreferences: {
        preload: path.join(__dirname, 'ui', 'preload.js'),
        contextIsolation: true,
        nodeIntegration: false
      }
    });

    _overlayWindow.setAlwaysOnTop(true, 'screen-saver');

    _overlayWindow.webContents.on('did-fail-load', (_evt, errorCode, errorDescription, validatedURL) => {
      console.error(`[Screenshot Plugin] Overlay UI failed to load: (${errorCode}) ${errorDescription} at ${validatedURL}`);
    });

    _overlayWindow.webContents.on('render-process-gone', (_evt, details) => {
      console.error('[Screenshot Plugin] Overlay render process terminated unexpectedly:', details);
    });

    const htmlPath = path.join(__dirname, 'ui', 'index.html');
    _overlayWindow.loadFile(htmlPath, { query: { img: fileUrl } });

    _overlayWindow.once('ready-to-show', () => {
      if (_overlayWindow && !_overlayWindow.isDestroyed()) {
        _overlayWindow.show();
        _overlayWindow.focus();
        try {
          _overlayWindow.webContents.send('screencap:init', { imageSrc: fileUrl });
        } catch (err) {
          console.error('[Screenshot Plugin] Failed to dispatch screencap:init to overlay:', err);
        }
      }
    });

    const cleanup = () => {
      if (!resolved) {
        resolved = true;
        resolve(false);
      }
      try {
        if (fs.existsSync(capturePath)) {
          fs.unlinkSync(capturePath);
        }
      } catch (_) {}
      if (_overlayWindow && !_overlayWindow.isDestroyed()) {
        _overlayWindow.destroy();
        _overlayWindow = null;
      }
    };

    _overlayWindow.on('closed', cleanup);

    // Listen to actions from the overlay window
    const actionChannel = 'screencap:action';
    const actionListener = async (_evt, data) => {
      if (!_overlayWindow || _overlayWindow.isDestroyed() || _evt.sender !== _overlayWindow.webContents) {
        return;
      }
      ipcMain.removeListener(actionChannel, actionListener);

      const action = data && data.action;
      const dataUrl = data && data.dataUrl;

      if (action === 'accept' && dataUrl) {
        try {
          const img = nativeImage.createFromDataURL(dataUrl);
          if (img.isEmpty()) {
            throw new Error('Rendered screenshot dataURL yielded an empty NativeImage');
          }
          clipboard.writeImage(img);
          resolved = true;
          cleanup();
          resolve(true);
        } catch (e) {
          console.error('[Screenshot Plugin] Failed to process accepted screenshot image:', e.stack || e);
          cleanup();
          resolve(false);
        }
      } else if (action === 'copy' && dataUrl) {
        try {
          const img = nativeImage.createFromDataURL(dataUrl);
          if (img.isEmpty()) {
            throw new Error('Rendered screenshot dataURL yielded an empty NativeImage');
          }
          clipboard.writeImage(img);
          resolved = true;
          cleanup();
          resolve(false); // copied to clipboard, but not pasting to chat
        } catch (e) {
          console.error('[Screenshot Plugin] Failed to copy screenshot image to clipboard:', e.stack || e);
          cleanup();
          resolve(false);
        }
      } else if (action === 'save' && dataUrl) {
        try {
          cleanup(); // close overlay first before opening save dialog
          const img = nativeImage.createFromDataURL(dataUrl);
          if (img.isEmpty()) {
            throw new Error('Rendered screenshot dataURL yielded an empty NativeImage');
          }
          const pngBuffer = img.toPNG();

          const now = new Date();
          const pad = (n) => String(n).padStart(2, '0');
          const timestamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
          const defaultPath = path.join(app.getPath('pictures') || app.getPath('documents'), `Zalo_Screenshot_${timestamp}.png`);

          const { canceled, filePath } = await dialog.showSaveDialog(_mainWindow, {
            title: 'Lưu ảnh chụp màn hình',
            defaultPath,
            filters: [{ name: 'PNG Image', extensions: ['png'] }]
          });

          if (!canceled && filePath) {
            fs.writeFileSync(filePath, pngBuffer);
          }
          resolved = true;
          resolve(false);
        } catch (e) {
          console.error('[Screenshot Plugin] Failed to save screenshot image to disk:', e.stack || e);
          cleanup();
          resolve(false);
        }
      } else {
        // cancel
        cleanup();
        resolve(false);
      }
    };

    ipcMain.on(actionChannel, actionListener);
  });
}

function setMainWindow(win) {
  _mainWindow = win;
}

module.exports = { register, setMainWindow };
