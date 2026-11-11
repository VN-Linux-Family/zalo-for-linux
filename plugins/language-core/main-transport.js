'use strict';

const ZALO_MAIN_PAGE = /\/pc-dist\/(?:index|child|login|compact-app)\.html(?:[?#]|$)/;

function isTrustedMainFrame(event, getWindows) {
  if (!event || !event.sender || !event.senderFrame ||
      event.senderFrame !== event.sender.mainFrame ||
      typeof getWindows !== 'function') return false;
  try {
    const windows = getWindows();
    if (!Array.isArray(windows)) return false;
    return windows.some((win) => {
      if (!win || typeof win.isDestroyed !== 'function' || win.isDestroyed() ||
          !win.webContents || win.webContents !== event.sender) return false;
      return ZALO_MAIN_PAGE.test(event.sender.getURL());
    });
  } catch (_) {
    return false;
  }
}

function registerMainTransport({ ipcMain, getWindows, onLocale } = {}) {
  if (!ipcMain || typeof ipcMain.on !== 'function' ||
      typeof getWindows !== 'function' || typeof onLocale !== 'function') {
    throw new TypeError('Language transport requires ipcMain, getWindows and onLocale');
  }

  const listener = (event, locale) => {
    if (locale !== 'vi' && locale !== 'en') return;
    if (!isTrustedMainFrame(event, getWindows)) return;
    try {
      onLocale(locale, event);
    } catch (error) {
      console.error('[language-core] Main locale handler failed:', error);
    }
  };

  ipcMain.on('zalo-language:publish-locale', listener);
  return Object.freeze({
    dispose() {
      if (typeof ipcMain.removeListener === 'function') {
        ipcMain.removeListener('zalo-language:publish-locale', listener);
      }
    }
  });
}

module.exports = { registerMainTransport, isTrustedMainFrame };
