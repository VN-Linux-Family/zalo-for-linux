'use strict';

const fs = require('fs');
const path = require('path');

const ZALO_PAGE_PATTERN = /\/pc-dist\/(index|child|login|compact-app)\.html(?:[?#]|$)/;
const registrations = new WeakMap();

let rendererScript;
function getRendererScript() {
  if (rendererScript === undefined) {
    rendererScript = fs.readFileSync(path.join(__dirname, 'renderer.js'), 'utf8');
  }
  return rendererScript;
}

function isZaloPage(win) {
  try {
    return Boolean(win && !win.isDestroyed() && win.webContents &&
      typeof win.webContents.getURL === 'function' &&
      ZALO_PAGE_PATTERN.test(win.webContents.getURL()));
  } catch (_) {
    return false;
  }
}

function inject(win, initialLocale = null) {
  if (!isZaloPage(win) || typeof win.webContents.executeJavaScript !== 'function') {
    return Promise.resolve(false);
  }
  try {
    // New/reloaded renderers may not contain Zalo's Language Manager. Seed
    // them from Main's last known locale, but never overwrite a valid local
    // snapshot already published by Zalo during page startup.
    const seed = initialLocale === 'vi' || initialLocale === 'en'
      ? `if(!window.__ZALO_LANGUAGE_BRIDGE__||window.__ZALO_LANGUAGE_BRIDGE__.version!==1||!['vi','en'].includes(window.__ZALO_LANGUAGE_BRIDGE__.locale)||!Number.isFinite(window.__ZALO_LANGUAGE_BRIDGE__.timestamp)){window.__ZALO_LANGUAGE_BRIDGE__={version:1,locale:${JSON.stringify(initialLocale)},timestamp:Date.now()};}\n`
      : '';
    return Promise.resolve(win.webContents.executeJavaScript(seed + getRendererScript(), true))
      .then(() => true)
      .catch((error) => {
        console.error('[language-core] Cannot inject renderer bridge:', error.message);
        return false;
      });
  } catch (error) {
    console.error('[language-core] Cannot inject renderer bridge:', error.message);
    return Promise.resolve(false);
  }
}

function register({ app, getInitialLocale } = {}) {
  if (!app || typeof app.on !== 'function') {
    throw new TypeError('Language Core requires Electron app lifecycle events.');
  }
  var existing = registrations.get(app);
  if (existing && !existing.disposed) return existing.publicHandle;

  var listeners = new Set();
  var disposed = false;

  function onWindowCreated(_event, win) {
    if (!win || !win.webContents || typeof win.webContents.on !== 'function') return;
    var webContents = win.webContents;
    var onDomReady = () => {
      if (disposed) return;
      let initialLocale = null;
      try {
        if (typeof getInitialLocale === 'function') initialLocale = getInitialLocale();
      } catch (error) {
        console.warn('[language-core] Cannot read Main locale snapshot:', error.message);
      }
      inject(win, initialLocale);
    };
    var onDestroyed = () => {
      webContents.removeListener('dom-ready', onDomReady);
      webContents.removeListener('destroyed', onDestroyed);
      listeners.delete(disposeWindow);
    };
    function disposeWindow() {
      webContents.removeListener('dom-ready', onDomReady);
      webContents.removeListener('destroyed', onDestroyed);
    }
    webContents.on('dom-ready', onDomReady);
    webContents.once('destroyed', onDestroyed);
    listeners.add(disposeWindow);
  }

  function dispose() {
    if (disposed) return false;
    disposed = true;
    if (typeof app.removeListener === 'function') {
      app.removeListener('browser-window-created', onWindowCreated);
      app.removeListener('before-quit', dispose);
    }
    listeners.forEach((removeListeners) => removeListeners());
    listeners.clear();
    registrations.delete(app);
    return true;
  }

  app.on('browser-window-created', onWindowCreated);
  app.on('before-quit', dispose);
  var publicHandle = Object.freeze({
    inject,
    isZaloPage,
    dispose,
    get disposed() { return disposed; }
  });
  registrations.set(app, { publicHandle, get disposed() { return disposed; } });
  return publicHandle;
}

module.exports = { register, isZaloPage, getRendererScript, inject, _inject: inject };
