'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { pathToFileURL } = require('node:url');
const offlineStore = require('../plugins/offline/store');
const vm = require('node:vm');
const { register } = require('../plugins/offline');
const { createOfflineController } = require('../plugins/offline/renderer');

const root = path.join(__dirname, '..');
const read = (file) => fs.readFileSync(path.join(root, file), 'utf8');
const dictionary = JSON.parse(read('plugins/offline/locale.json'));
const renderer = read('plugins/offline/renderer.js');
const main = read('plugins/offline/index.js');

describe('Offline renderer and language integration', () => {
  it('uses its own schema-compatible Vietnamese and English dictionary', () => {
    assert.equal(dictionary.schemaVersion, 1);
    assert.equal(dictionary.plugin, 'offline');
    assert.deepEqual(Object.keys(dictionary.messages).sort(), [
      'offline.status.offline',
      'offline.status.reconnecting',
      'offline.status.retrying'
    ]);
    for (const [key, pair] of Object.entries(dictionary.messages)) {
      assert.ok(key.startsWith('offline.'));
      assert.deepEqual(Object.keys(pair).sort(), ['en', 'vi']);
      assert.equal(typeof pair.vi, 'string');
      assert.equal(typeof pair.en, 'string');
      assert.ok(pair.vi.length > 0);
      assert.ok(pair.en.length > 0);
    }
  });

  it('keeps translated UI text in the dictionary and uses stable status codes', () => {
    assert.doesNotMatch(renderer, /[À-ỹĐđ]/, 'renderer must not contain hard-coded Vietnamese UI text');
    for (const match of renderer.matchAll(/showStatus\('([^']+)'\)/g)) {
      assert.ok(dictionary.messages['offline.status.' + match[1]], 'missing Offline status key: ' + match[1]);
    }
    assert.match(renderer, /id: 'offline'/);
    assert.match(renderer, /registerPlugin\(\{/);
    assert.match(renderer, /zalo-language:ready/);
    assert.match(renderer, /unregisterLanguage\(\)/);
    assert.match(main, /require\('\.\/locale\.json'\)/);
    assert.match(main, /__zaloLinuxOfflineMessages/);
    assert.ok(main.includes("\"'use strict';\\nwindow.__zaloLinuxOfflineMessages = "));
    assert.doesNotThrow(() => new vm.Script("'use strict';\nwindow.__zaloLinuxOfflineMessages = " + JSON.stringify(dictionary.messages) + ';\n' + renderer));
  });

  it('serves a strict-mode renderer script with the plugin dictionary embedded', () => {
    const handlers = new Map();
    register({
      app: { getPath: () => '/tmp/zalo-offline-language-test' },
      ipcMain: {
        on(channel, handler) { handlers.set(channel, handler); },
        handle(channel, handler) { handlers.set(channel, handler); }
      },
      appDir: '/tmp/zalo-offline-app'
    });
    const frame = { url: 'file:///tmp/zalo-offline-app/pc-dist/index.html' };
    const event = { senderFrame: frame, sender: { mainFrame: frame } };
    handlers.get('linux-offline:renderer')(event);

    assert.match(event.returnValue, /^'use strict';\nwindow\.__zaloLinuxOfflineMessages = /);
    assert.ok(event.returnValue.includes(JSON.stringify(dictionary.messages)));
    assert.doesNotThrow(() => new vm.Script(event.returnValue));
  });

  it('accepts trusted Offline pages when Flatpak resolves the app symlink', t => {
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-offline-flatpak-'));
    try {
      const realAppDir = path.join(root, 'extra', 'app');
      const appDir = path.join(root, 'lib', 'zalo', 'app');
      const pcDir = path.join(realAppDir, 'pc-dist');
      fs.mkdirSync(pcDir, { recursive: true });
      for (const name of ['index.html', 'login.html', 'shared-worker.html']) {
        fs.writeFileSync(path.join(pcDir, name), '');
      }
      fs.mkdirSync(path.dirname(appDir), { recursive: true });
      try {
        fs.symlinkSync(realAppDir, appDir, 'dir');
      } catch (error) {
        if (['EPERM', 'EACCES', 'ENOTSUP'].includes(error.code)) {
          t.skip('directory symlinks are unavailable in this environment');
          return;
        }
        throw error;
      }

      const handlers = new Map();
      register({
        app: { getPath: () => path.join(root, 'user-data') },
        ipcMain: {
          on(channel, handler) { handlers.set(channel, handler); },
          handle(channel, handler) { handlers.set(channel, handler); }
        },
        appDir
      });

      const frame = { url: pathToFileURL(path.join(realAppDir, 'pc-dist', 'index.html')).href };
      const event = { senderFrame: frame, sender: { mainFrame: frame } };
      handlers.get('linux-offline:renderer')(event);

      assert.match(event.returnValue, /^'use strict';\nwindow\.__zaloLinuxOfflineMessages = /);

      handlers.get('linux-offline:active')(event, true);
      const workerFrame = { url: pathToFileURL(path.join(realAppDir, 'pc-dist', 'shared-worker.html')).href };
      const workerEvent = { senderFrame: workerFrame, sender: { mainFrame: workerFrame } };
      handlers.get('linux-offline:is-active')(workerEvent);
      assert.equal(workerEvent.returnValue, true);
    } finally {
      fs.rmSync(root, { recursive: true, force: true });
    }
  });

  it('updates the visible status immediately when Language Core changes locale', async () => {
    const elements = new Map();
    const listeners = new Map();
    const body = { appendChild(element) { elements.set(element.id, element); } };
    const document = {
      body,
      createElement() {
        return { id: '', style: {}, attributes: {}, setAttribute(name, value) { this.attributes[name] = value; } };
      }
    };
    const cached = {
      version: 1,
      clientId: 'test-client',
      info: { config: { uid: '123', current_time: 100 }, requestStartTime: Date.now() }
    };
    let languageHandler;
    let unregisterCalls = 0;
    const window = {
      __zaloLinuxOfflineMessages: dictionary.messages,
      __zaloOfflineBridge: {
        request: async (action) => action === 'read' ? cached : true,
        reportActive() {}
      },
      ZaloLanguage: {
        registerPlugin(options) {
          assert.equal(options.id, 'offline');
          languageHandler = options.onLanguageUpdateRequest;
          languageHandler({ type: 'LANGUAGE_UPDATE_REQUEST', locale: 'vi' });
          return () => { unregisterCalls += 1; };
        }
      },
      addEventListener(name, callback) { listeners.set(name, callback); },
      removeEventListener(name, callback) {
        if (listeners.get(name) === callback) listeners.delete(name);
      },
      setTimeout() { return 1; },
      clearTimeout() {}
    };
    const context = {
      window,
      document,
      navigator: { onLine: false },
      module: { exports: {} },
      console,
      Date,
      Promise,
      JSON,
      Object,
      Array,
      String,
      Number,
      Math
    };

    vm.runInNewContext(renderer, context, { filename: 'plugins/offline/renderer.js' });
    await window.__zaloOffline.fetchLoginInfo(
      async () => { throw new Error('network request must not run while offline'); },
      { clientId: 'test-client', userId: '123', onOffline() {} }
    );

    const badge = elements.get('linux-offline-status');
    assert.ok(badge);
    assert.equal(badge.textContent, dictionary.messages['offline.status.offline'].vi);
    assert.equal(window.__zaloOffline.active, true);

    languageHandler({ type: 'LANGUAGE_UPDATE_REQUEST', locale: 'en' });
    assert.equal(badge.textContent, dictionary.messages['offline.status.offline'].en);
    languageHandler({ type: 'LANGUAGE_UPDATE_REQUEST', locale: 'fr' });
    assert.equal(badge.textContent, dictionary.messages['offline.status.offline'].en);

    listeners.get('pagehide')();
    assert.equal(unregisterCalls, 1);
    assert.equal(listeners.has('zalo-language:ready'), false);
  });
});


describe('Offline startup and recovery controller', () => {
  const validInfo = () => ({
    config: { uid: '123', current_time: 1000, settings: { features: {} } },
    requestStartTime: 1000,
    UIN: '123', decryptKey: 'test-key'
  });
  const validCache = () => ({ version: 1, clientId: 'client', info: validInfo() });

  function createHarness({ cached = validCache(), online = false } = {}) {
    const timers = new Map();
    const calls = { reads: 0, writes: [], clears: 0, reloads: 0, statuses: [], reports: [], online: null, unload: null };
    let timerId = 0;
    const storage = {
      async read() { calls.reads++; return cached; },
      async write(record) { calls.writes.push(record); return true; },
      async clear() { calls.clears++; }
    };
    const controller = createOfflineController({
      storage,
      setTimeout(callback, delay) { const id = ++timerId; timers.set(id, { callback, delay }); return id; },
      clearTimeout(id) { timers.delete(id); },
      isOnline: () => online,
      now: () => 5000,
      showStatus: status => calls.statuses.push(status),
      reportActive: active => calls.reports.push(active),
      onOnline: callback => { calls.online = callback; },
      onUnload: callback => { calls.unload = callback; },
      reload: async () => { calls.reloads++; return true; }
    });
    return { controller, calls, timers, storage };
  }

  it('restores a matching cached account while offline and preserves the adjusted server clock', async () => {
    const h = createHarness();
    let networkCalls = 0;
    const result = await h.controller.fetchLoginInfo(async () => { networkCalls++; throw new Error('must not run'); }, {
      clientId: 'client', userId: '123', onOffline() {}
    });
    assert.equal(networkCalls, 0);
    assert.equal(result.config.uid, '123');
    assert.equal(result.config.current_time, 1004);
    assert.equal(result.requestStartTime, 5000);
    assert.equal(h.controller.active, true);
    assert.deepEqual(h.calls.statuses, ['offline']);
    assert.deepEqual(h.calls.reports, [false, true]);
    assert.equal(h.timers.size, 1);
  });

  it('rejects a cache belonging to another client or account and uses normal login', async () => {
    for (const cached of [
      { ...validCache(), clientId: 'other-client' },
      { ...validCache(), info: { ...validInfo(), config: { ...validInfo().config, uid: 'different-user' } } },
      null
    ]) {
      const h = createHarness({ cached });
      const info = validInfo();
      const result = await h.controller.fetchLoginInfo(async () => info, {
        clientId: 'client', userId: '123', onOffline() {}
      });
      assert.equal(result, info);
      assert.equal(h.controller.active, false);
      assert.equal(h.calls.writes.length, 1);
    }
  });

  it('falls back to a valid cache only for network failures during online startup', async () => {
    const h = createHarness({ online: true });
    const result = await h.controller.fetchLoginInfo(async () => { throw { code: 'ERR_INTERNET_DISCONNECTED' }; }, {
      clientId: 'client', userId: '123', onOffline() {}
    });
    assert.equal(result.config.uid, '123');
    assert.equal(h.controller.active, true);
    assert.deepEqual(h.calls.statuses, ['offline']);
  });

  it('does not mask authentication or server errors with a cached session', async () => {
    const h = createHarness({ online: true });
    const error = { data: { error_code: 999 } };
    await assert.rejects(h.controller.fetchLoginInfo(async () => { throw error; }, {
      clientId: 'client', userId: '123', onOffline() {}
    }), value => value === error);
    assert.equal(h.controller.active, false);
    assert.equal(h.calls.statuses.length, 0);
  });

  it('invalidates a rejected session and stops retrying', async () => {
    const h = createHarness({ online: true });
    const error = { data: { error_code: 102 } };
    await assert.rejects(h.controller.fetchLoginInfo(async () => { throw error; }, {
      clientId: 'client', userId: '123', onOffline() {}
    }), value => value === error);
    assert.equal(h.calls.clears, 1);
    assert.deepEqual(h.calls.reports, [false, false]);
    assert.equal(h.timers.size, 0);
  });

  it('cancels pending retry timers and reports Offline inactive on unload', async () => {
    const h = createHarness();
    await h.controller.fetchLoginInfo(async () => validInfo(), {
      clientId: 'client', userId: '123', onOffline() {}
    });
    assert.equal(h.timers.size, 1);
    h.calls.unload();
    assert.equal(h.timers.size, 0);
    assert.deepEqual(h.calls.reports, [false, true, false]);
  });

  it('reloads once after successful reconnect when local UI is ready', async () => {
    const h = createHarness();
    let attempts = 0;
    await h.controller.fetchLoginInfo(async () => {
      attempts++;
      if (attempts === 1) throw { code: 'ERR_NO_NETWORK' };
      return validInfo();
    }, { clientId: 'client', userId: '123', onOffline() {} });
    const firstRetry = [...h.timers.values()][0];
    h.controller.markReady();
    firstRetry.callback();
    await new Promise(resolve => setImmediate(resolve));
    const secondRetry = [...h.timers.values()][0];
    secondRetry.callback();
    await new Promise(resolve => setImmediate(resolve));
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(attempts, 2);
    assert.equal(h.calls.reloads, 1);
    assert.deepEqual(h.calls.statuses, ['offline', 'reconnecting']);
  });
});
