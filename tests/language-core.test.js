'use strict';

(function testGroup_language_core_test_js() {
const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const vm = require('vm');
const { EventEmitter } = require('events');

const rendererPath = path.join(__dirname, '..', 'plugins', 'language-core', 'renderer.js');
const rendererSource = fs.readFileSync(rendererPath, 'utf8');
const languageCore = require('../plugins/language-core');

function createRenderer({ locale = null, source = true } = {}) {
  const callbacks = new Set();
  const sourceObject = {
    locale,
    currentLanguage() {
      return this.index !== undefined ? this.index :
        (this.locale === 'en' ? 1 : this.locale === 'vi' ? 0 : -1);
    },
    getCurrentLanguageName() { return this.currentLanguage() === 1 ? 'en' : 'vi'; },
    callUpdate(callback) { callbacks.add(callback); },
    removeUpdate(callback) { callbacks.delete(callback); },
    changeToLang() { throw new Error('write API must never be called'); },
    setGeneralFlag() { throw new Error('write API must never be called'); }
  };
  const html = {
    lang: '',
    getAttribute(name) { return name === 'lang' ? this.lang : null; }
  };
  const lifecycle = {};
  const listeners = new Map();
  const window = {
    document: { documentElement: html },
    addEventListener(name, callback) {
      lifecycle[name] = callback;
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(callback);
    },
    removeEventListener(name, callback) {
      if (listeners.has(name)) listeners.get(name).delete(callback);
    },
    dispatchEvent(event) {
      const handlers = listeners.get(event.type);
      if (handlers) for (const handler of handlers) handler(event);
      return true;
    }
  };
  if (arguments.length && arguments[0] && arguments[0].initialBridge) {
    window.__ZALO_LANGUAGE_BRIDGE__ = arguments[0].initialBridge;
  }
  if (source) {
    const runtime = function webpackRequire() {};
    runtime.c = { languageModule: { exports: sourceObject } };
    window.__webpack_require__ = runtime;
  }
  vm.runInNewContext(rendererSource, { window, console, Date, Map, Set, Object, Array, String,
    Number, Error, TypeError, Promise, RegExp, JSON });
  return {
    api: window.ZaloLanguage, window, html, sourceObject, callbacks,
    mutateHtml(locale) { html.lang = locale; },
    emitBridge(locale) {
      window.__ZALO_LANGUAGE_BRIDGE__ = { version: 1, locale, timestamp: Date.now() };
      window.dispatchEvent({ type: 'zalo-language:changed', detail: window.__ZALO_LANGUAGE_BRIDGE__ });
    },
    triggerPagehide() { if (lifecycle.pagehide) lifecycle.pagehide(); },
    listenerCount(name) { return listeners.has(name) ? listeners.get(name).size : 0; }
  };
}

describe('Language Core renderer bridge', () => {
  it('reads the Zalo singleton through an already exposed module cache and broadcasts requests', () => {
    const runtime = createRenderer({ locale: 'vi' });
    const receivedA = [], receivedB = [];
    runtime.api.registerPlugin({ id: 'plugin-a', onLanguageUpdateRequest: (request) => receivedA.push(request) });
    runtime.api.registerPlugin({ id: 'plugin-b', onLanguageUpdateRequest: (request) => receivedB.push(request) });

    assert.equal(runtime.api.getLocale(), 'vi');
    assert.equal(receivedA.length, 1);
    assert.equal(receivedA[0].source, 'initial');
    assert.equal(receivedA[0].type, 'LANGUAGE_UPDATE_REQUEST');

    runtime.sourceObject.locale = 'en';
    for (const callback of runtime.callbacks) callback();
    assert.equal(runtime.api.getLocale(), 'en');
    assert.equal(receivedA.length, 2);
    assert.equal(receivedB.length, 2);
    assert.equal(receivedA[1].previousLocale, 'vi');
    assert.equal(receivedA[1].locale, 'en');
    assert.equal(receivedA[1].source, 'zalo');
    assert.ok(receivedA[1].requestId);
    assert.equal(typeof receivedA[1].timestamp, 'number');

    for (const callback of runtime.callbacks) callback();
    assert.equal(receivedA.length, 2, 'same locale must not broadcast twice');
  });

  it('isolates plugin exceptions and continues broadcasting', () => {
    const runtime = createRenderer({ locale: 'vi' });
    let healthyCalls = 0;
    const originalError = console.error;
    console.error = () => {};
    try {
      runtime.api.registerPlugin({ id: 'throws', onLanguageUpdateRequest() { throw new Error('expected'); } });
      runtime.api.registerPlugin({ id: 'healthy', onLanguageUpdateRequest() { healthyCalls++; } });
      runtime.sourceObject.locale = 'en';
      for (const callback of runtime.callbacks) callback();
      assert.equal(healthyCalls, 2);
      assert.equal(runtime.api.getStatus().dispatchErrors, 2);
    } finally { console.error = originalError; }
  });

  it('unregisters plugins and sends no further requests to them', () => {
    const runtime = createRenderer({ locale: 'vi' });
    let calls = 0;
    const unregister = runtime.api.registerPlugin({
      id: 'temporary-plugin',
      onLanguageUpdateRequest() { calls++; }
    });
    assert.equal(calls, 1);
    assert.equal(unregister(), true);
    assert.equal(unregister(), false);
    runtime.sourceObject.locale = 'en';
    for (const callback of runtime.callbacks) callback();
    assert.equal(calls, 1);
    assert.equal(runtime.api.getStatus().registeredPlugins, 0);
  });

  it('keeps plugin registration focused on locale update callbacks', () => {
    const runtime = createRenderer({ locale: 'vi' });
    const received = [];
    runtime.api.registerPlugin({
      id: 'demo-plugin',
      onLanguageUpdateRequest(request) { received.push(request); }
    });
    assert.equal(received.length, 1);
    assert.equal(received[0].locale, 'vi');
    assert.equal('translate' in runtime.api, false);
    assert.equal('messages' in runtime.api, false);
  });

  it('exposes no language setter and never calls Zalo write methods', () => {
    const runtime = createRenderer({ locale: 'vi' });
    assert.deepEqual(Object.keys(runtime.api).sort(), [
      'getLocale', 'getStatus', 'registerPlugin'
    ]);
    assert.equal('setLocale' in runtime.api, false);
    assert.equal('changeLanguage' in runtime.api, false);
    runtime.sourceObject.locale = 'en';
    for (const callback of runtime.callbacks) callback();
    assert.equal(runtime.api.getLocale(), 'en');
  });

  it('does not mistake Zalo uninitialized index -1 for its getter fallback Vietnamese', () => {
    const runtime = createRenderer({ locale: null });
    runtime.sourceObject.index = -1;
    assert.equal(runtime.sourceObject.getCurrentLanguageName(), 'vi');
    assert.equal(runtime.api.getLocale(), null);
    const received = [];
    runtime.api.registerPlugin({ id: 'wait-for-zalo-index', onLanguageUpdateRequest: request => received.push(request) });
    for (const callback of runtime.callbacks) callback();
    assert.equal(runtime.api.getLocale(), null);
    assert.equal(received.length, 0);

    runtime.sourceObject.index = 1;
    runtime.sourceObject.locale = 'en';
    for (const callback of runtime.callbacks) callback();
    assert.equal(runtime.api.getLocale(), 'en');
    assert.equal(received.length, 1);
    assert.equal(received[0].locale, 'en');
  });

  it('reads a patched bridge state that was published before Language Core initializes', () => {
    const runtime = createRenderer({ source: false, initialBridge: { version: 1, locale: 'vi', timestamp: Date.now() } });
    const received = [];
    runtime.api.registerPlugin({ id: 'preloaded-plugin', onLanguageUpdateRequest: (request) => received.push(request) });
    assert.equal(runtime.api.getLocale(), 'vi');
    assert.equal(runtime.api.getStatus().source, 'zalo');
    assert.equal(received.length, 1);
    assert.equal(received[0].source, 'initial');
    runtime.emitBridge('en');
    assert.equal(runtime.api.getLocale(), 'en');
    assert.equal(received.length, 2);
    assert.equal(received[1].previousLocale, 'vi');
    assert.equal(received[1].source, 'zalo');
  });

  it('subscribes before the patch emits its first locale event', () => {
    const runtime = createRenderer({ source: false });
    assert.equal(runtime.api.getLocale(), null);
    const received = [];
    runtime.api.registerPlugin({ id: 'late-locale-plugin', onLanguageUpdateRequest: (request) => received.push(request) });
    runtime.emitBridge('en');
    assert.equal(runtime.api.getLocale(), 'en');
    assert.equal(received.length, 1);
    assert.equal(received[0].locale, 'en');
    assert.equal(received[0].previousLocale, null);
  });

  it('never infers the active locale from the HTML lang attribute', () => {
    const runtime = createRenderer({ source: false });
    runtime.html.lang = 'en';
    assert.equal(runtime.api.getLocale(), null);
    let calls = 0;
    runtime.api.registerPlugin({ id: 'no-guess-plugin', onLanguageUpdateRequest() { calls++; } });
    runtime.mutateHtml('vi');
    assert.equal(runtime.api.getLocale(), null);
    assert.equal(calls, 0);
  });

  it('fails closed when neither a supported source nor html lang is available', () => {
    const runtime = createRenderer({ source: false });
    assert.equal(runtime.api.getLocale(), null);
    assert.equal(runtime.api.getStatus().ready, false);
    let calls = 0;
    runtime.api.registerPlugin({ id: 'waiting-plugin', onLanguageUpdateRequest() { calls++; } });
    assert.equal(calls, 0);
  });

  it('cleans up singleton and bridge event listeners on pagehide', () => {
    const runtime = createRenderer({ locale: 'vi' });
    assert.equal(runtime.callbacks.size, 1);
    assert.equal(runtime.listenerCount('zalo-language:changed'), 1);
    runtime.triggerPagehide();
    assert.equal(runtime.callbacks.size, 0);
    assert.equal(runtime.listenerCount('zalo-language:changed'), 0);
  });
});

describe('Language Core main-process injection', () => {
  it('seeds late/reloaded renderers from Main locale without overwriting Zalo snapshot', async () => {
    const app = new EventEmitter();
    languageCore.register({ app, getInitialLocale: () => 'en' });
    const scripts = [];
    const webContents = new EventEmitter();
    webContents.getURL = () => 'file:///opt/zalo/app/pc-dist/index.html';
    webContents.executeJavaScript = async (script) => { scripts.push(script); };
    const win = new EventEmitter();
    win.webContents = webContents;
    win.isDestroyed = () => false;
    app.emit('browser-window-created', {}, win);
    webContents.emit('dom-ready');
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(scripts.length, 1);
    assert.match(scripts[0], /__ZALO_LANGUAGE_BRIDGE__.*version:1,locale:\"en\",timestamp:Date\.now\(\)/s);
    assert.ok(scripts[0].indexOf('__ZALO_LANGUAGE_BRIDGE__') < scripts[0].indexOf('installZaloLanguageCore'));
    assert.match(scripts[0], /window\.__ZALO_LANGUAGE_BRIDGE__\.version!==1/);
    assert.ok(scripts[0].includes('window.__ZALO_LANGUAGE_BRIDGE__={version:1,locale:"en",timestamp:Date.now()}'));
  });

  it('injects the bridge into supported Zalo UI pages and excludes unrelated popups', async () => {
    const app = new EventEmitter();
    languageCore.register({ app });
    const scripts = [];
    const webContents = new EventEmitter();
    webContents.getURL = () => 'file:///opt/zalo/app/pc-dist/index.html';
    webContents.executeJavaScript = async (script) => { scripts.push(script); };
    const win = new EventEmitter();
    win.webContents = webContents;
    win.isDestroyed = () => false;
    app.emit('browser-window-created', {}, win);
    webContents.emit('dom-ready');
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(scripts.length, 1);
    assert.match(scripts[0], /LANGUAGE_UPDATE_REQUEST/);
    assert.equal(languageCore.isZaloPage(win), true);

    for (const page of ['child.html', 'login.html', 'compact-app.html']) {
      const pageContents = new EventEmitter();
      pageContents.getURL = () => 'file:///opt/zalo/app/pc-dist/' + page;
      pageContents.executeJavaScript = async () => {};
      const pageWindow = new EventEmitter();
      pageWindow.webContents = pageContents;
      pageWindow.isDestroyed = () => false;
      assert.equal(languageCore.isZaloPage(pageWindow), true, page + ' should receive Language Core');
    }

    const otherContents = new EventEmitter();
    otherContents.getURL = () => 'file:///opt/zalo/app/pc-dist/popup-viewer.html';
    otherContents.executeJavaScript = async () => { throw new Error('must not inject'); };
    const otherWin = new EventEmitter();
    otherWin.webContents = otherContents;
    otherWin.isDestroyed = () => false;
    app.emit('browser-window-created', {}, otherWin);
    otherContents.emit('dom-ready');
    assert.equal(languageCore.isZaloPage(otherWin), false);
  });
});

})();

(function testGroup_language_core_contract_test_js() {
const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const { EventEmitter } = require('events');
const fs = require('fs');
const path = require('path');

const rendererSource = fs.readFileSync(path.join(__dirname, '..', 'plugins', 'language-core', 'renderer.js'), 'utf8');
const core = require('../plugins/language-core');

function makeRenderer(options = {}) {
  const listeners = new Map();
  const sourceCallbacks = new Set();
  const source = options.sourceObject || {
    locale: options.locale || null,
    currentLanguage() { return this.locale === 'en' ? 1 : this.locale === 'vi' ? 0 : -1; },
    getCurrentLanguageName() { return this.currentLanguage() === 1 ? 'en' : 'vi'; },
    callUpdate(callback) { sourceCallbacks.add(callback); },
    removeUpdate(callback) { sourceCallbacks.delete(callback); }
  };
  const window = {
    document: { documentElement: { lang: options.htmlLang || '' } },
    addEventListener(name, callback) {
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(callback);
    },
    removeEventListener(name, callback) {
      if (listeners.has(name)) listeners.get(name).delete(callback);
    },
    dispatchEvent(event) {
      (listeners.get(event.type) || []).forEach((callback) => callback(event));
    }
  };
  if (options.bridge) window.__ZALO_LANGUAGE_BRIDGE__ = options.bridge;
  if (options.cache !== false) {
    const runtime = function webpackRequire() {};
    runtime.c = options.cacheEntries || { locale: { exports: source } };
    window.__webpack_require__ = runtime;
  }
  vm.runInNewContext(rendererSource, {
    window, console, Date, Map, Set, Object, Array, String, Number, Error,
    TypeError, Promise, RegExp, JSON
  });
  return {
    api: window.ZaloLanguage,
    window,
    source,
    sourceCallbacks,
    emit(locale, timestamp = Date.now()) {
      const state = { version: 1, locale, timestamp };
      window.__ZALO_LANGUAGE_BRIDGE__ = state;
      window.dispatchEvent({ type: 'zalo-language:changed', detail: state });
    },
    pagehide() {
      (listeners.get('pagehide') || []).forEach((callback) => callback());
    }
  };
}

describe('Language Core contract hardening', () => {
  it('uses a stable recipient snapshot when handlers mutate plugin registration', () => {
    const runtime = makeRenderer({ locale: 'vi' });
    const calls = [];
    let unregisterB;
    runtime.api.registerPlugin({
      id: 'plugin-a',
      onLanguageUpdateRequest(request) {
        calls.push('a:' + request.locale);
        if (request.locale === 'en') {
          runtime.api.registerPlugin({
            id: 'plugin-c',
            onLanguageUpdateRequest: (request2) => calls.push('c:' + request2.locale)
          });
          unregisterB();
        }
      }
    });
    unregisterB = runtime.api.registerPlugin({
      id: 'plugin-b',
      onLanguageUpdateRequest(request) { calls.push('b:' + request.locale); }
    });
    runtime.source.locale = 'en';
    runtime.sourceCallbacks.forEach((callback) => callback());
    assert.deepEqual(calls, ['a:vi', 'b:vi', 'a:en', 'c:en']);
    assert.equal(runtime.api.getStatus().registeredPlugins, 2);
  });

  it('ignores detached bridge events and rejects malformed locale payloads', () => {
    const runtime = makeRenderer({
      cache: false,
      bridge: { version: 1, locale: 'vi', timestamp: 100 }
    });
    runtime.window.dispatchEvent({
      type: 'zalo-language:changed',
      detail: { version: 1, locale: 'en', timestamp: 101 }
    });
    assert.equal(runtime.api.getLocale(), 'vi', 'detached event detail must not override the snapshot');
    runtime.emit('fr');
    assert.equal(runtime.api.getLocale(), 'vi');
    runtime.emit('en');
    assert.equal(runtime.api.getLocale(), 'en');
    assert.equal(runtime.api.getStatus().bridge, 'zalo-locale-bridge');
  });

  it('rejects a bridge snapshot older than the latest accepted locale state', () => {
    const runtime = makeRenderer({ cache: false, bridge: { version: 1, locale: 'vi', timestamp: 200 } });
    runtime.emit('en', 210);
    assert.equal(runtime.api.getLocale(), 'en');
    runtime.emit('vi', 199);
    assert.equal(runtime.api.getLocale(), 'en');
  });

  it('refuses ambiguous Webpack singleton discovery instead of choosing an arbitrary source', () => {
    const one = {
      currentLanguage: () => 0,
      getCurrentLanguageName: () => 'vi',
      callUpdate() {},
      removeUpdate() {}
    };
    const two = {
      currentLanguage: () => 1,
      getCurrentLanguageName: () => 'en',
      callUpdate() {},
      removeUpdate() {}
    };
    const runtime = makeRenderer({
      cacheEntries: { a: { exports: one }, b: { exports: two } }
    });
    assert.equal(runtime.api.getLocale(), null);
    assert.equal(runtime.api.getStatus().bridge, 'unavailable');
  });

  it('requires a recognized Zalo language index before accepting the getter result', () => {
    const source = {
      currentLanguage: () => -1,
      getCurrentLanguageName: () => 'vi',
      callUpdate() {},
      removeUpdate() {}
    };
    const runtime = makeRenderer({ sourceObject: source });
    assert.equal(runtime.api.getLocale(), null);
    assert.equal(runtime.api.getStatus().ready, false);
  });

  it('cleans up a partially attached read-only source when subscription throws', () => {
    let removed = 0;
    const source = {
      currentLanguage: () => 0,
      getCurrentLanguageName: () => 'vi',
      callUpdate() { throw new Error('subscription failed'); },
      removeUpdate() { removed += 1; }
    };
    const originalWarn = console.warn;
    console.warn = () => {};
    try {
      const runtime = makeRenderer({ sourceObject: source });
      assert.equal(runtime.api.getLocale(), null);
      assert.equal(removed, 1);
    } finally {
      console.warn = originalWarn;
    }
  });

  it('does not accept or expose plugin dictionaries or translation helpers', () => {
    const runtime = makeRenderer({ locale: 'vi' });
    let received = 0;
    runtime.api.registerPlugin({
      id: 'sample-plugin',
      messages: { 'sample-plugin.dialog.title': { vi: 'Tiêu đề', en: 'Title' } },
      onLanguageUpdateRequest() { received += 1; }
    });
    assert.equal(received, 1);
    assert.equal('translate' in runtime.api, false);
    assert.equal('messages' in runtime.api, false);
    assert.deepEqual(Object.keys(runtime.api).sort(), [
      'getLocale', 'getStatus', 'registerPlugin'
    ]);
  });

  it('counts rejected async handlers without stopping healthy handlers', async () => {
    const runtime = makeRenderer({ locale: 'vi' });
    const originalError = console.error;
    console.error = () => {};
    try {
      runtime.api.registerPlugin({
        id: 'async-failure',
        onLanguageUpdateRequest() { return Promise.reject(new Error('expected rejection')); }
      });
      let healthy = 0;
      runtime.api.registerPlugin({
        id: 'healthy-plugin',
        onLanguageUpdateRequest() { healthy += 1; }
      });
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(healthy, 1);
      assert.equal(runtime.api.getStatus().dispatchErrors, 1);
    } finally {
      console.error = originalError;
    }
  });

  it('makes main-process registration idempotent and disposes listeners cleanly', () => {
    const app = new EventEmitter();
    const first = core.register({ app });
    const second = core.register({ app });
    assert.equal(first, second);
    assert.equal(app.listenerCount('browser-window-created'), 1);
    assert.equal(app.listenerCount('before-quit'), 1);

    const webContents = new EventEmitter();
    webContents.getURL = () => 'file:///opt/zalo/app/pc-dist/index.html';
    webContents.executeJavaScript = () => Promise.resolve();
    const win = { webContents, isDestroyed: () => false };
    app.emit('browser-window-created', {}, win);
    assert.equal(webContents.listenerCount('dom-ready'), 1);
    assert.equal(first.dispose(), true);
    assert.equal(first.dispose(), false);
    assert.equal(app.listenerCount('browser-window-created'), 0);
    assert.equal(webContents.listenerCount('dom-ready'), 0);
    assert.equal(first.disposed, true);
  });
});

})();

'use strict';

(function testGroup_language_core_main_transport_test_js() {
const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const { registerMainTransport, isTrustedMainFrame } = require('../plugins/language-core/main-transport');

describe('Language main-process transport', () => {
  function fixture(url = 'file:///app/pc-dist/index.html') {
    const listeners = new Map();
    const mainFrame = {};
    const sender = { mainFrame, getURL: () => url };
    const mainWindow = { webContents: sender, isDestroyed: () => false };
    const compactFrame = {};
    const compactSender = { mainFrame: compactFrame, getURL: () => 'file:///app/pc-dist/compact-app.html' };
    const compactWindow = { webContents: compactSender, isDestroyed: () => false };
    const ipcMain = {
      on(channel, fn) { listeners.set(channel, fn); },
      removeListener(channel, fn) { if (listeners.get(channel) === fn) listeners.delete(channel); }
    };
    return { listeners, mainFrame, sender, mainWindow, compactFrame, compactSender, compactWindow, ipcMain };
  }

  it('accepts vi/en locale updates only from the authorized primary renderer', () => {
    const f = fixture();
    const received = [];
    registerMainTransport({ ipcMain: f.ipcMain, getWindows: () => [f.mainWindow], onLocale: (v, event) => received.push([v, event.sender]) });
    const listener = f.listeners.get('zalo-language:publish-locale');
    listener({ sender: f.sender, senderFrame: f.mainFrame }, 'en');
    listener({ sender: f.compactSender, senderFrame: f.compactFrame }, 'vi');
    listener({ sender: f.sender, senderFrame: f.mainFrame }, 'fr');
    assert.deepEqual(received, [['en', f.sender]]);
  });

  it('rejects Compact App, unowned windows, subframes and non-Zalo pages as locale sources', () => {
    const f = fixture();
    const getWindows = () => [f.mainWindow];
    assert.equal(isTrustedMainFrame({ sender: f.sender, senderFrame: {} }, getWindows), false);
    assert.equal(isTrustedMainFrame({ sender: {}, senderFrame: f.mainFrame }, getWindows), false);
    assert.equal(isTrustedMainFrame({ sender: { ...f.sender, getURL: () => 'https://evil.test/' }, senderFrame: f.mainFrame }, getWindows), false);
    assert.equal(isTrustedMainFrame({ sender: f.sender, senderFrame: f.mainFrame }, getWindows), true);
    assert.equal(isTrustedMainFrame({ sender: f.compactSender, senderFrame: f.compactFrame }, getWindows), false);
    assert.equal(isTrustedMainFrame({ sender: f.sender, senderFrame: f.mainFrame }, () => []), false);
  });

  it('contains a plugin/main handler exception without throwing through Electron IPC', () => {
    const f = fixture();
    const original = console.error;
    console.error = () => {};
    try {
      registerMainTransport({
        ipcMain: f.ipcMain,
        getWindows: () => [f.mainWindow],
        onLocale() { throw new Error('expected main handler failure'); }
      });
      const listener = f.listeners.get('zalo-language:publish-locale');
      assert.doesNotThrow(() => listener({ sender: f.sender, senderFrame: f.mainFrame }, 'en'));
    } finally { console.error = original; }
  });

  it('can dispose its IPC listener', () => {
    const f = fixture();
    const handle = registerMainTransport({ ipcMain: f.ipcMain, getWindows: () => [f.mainWindow], onLocale() {} });
    handle.dispose();
    assert.equal(f.listeners.has('zalo-language:publish-locale'), false);
  });
});

})();
