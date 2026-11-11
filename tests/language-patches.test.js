'use strict';

(function testGroup_language_preload_transport_patch_test_js() {
const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const patch = require('../scripts/patches/patch-zalo-language-transport');

describe('Language transport preload patch', () => {
  it('adds a narrow locale-only API and remains idempotent', () => {
    const result = patch.patchContent('const { contextBridge } = require("electron");');
    assert.equal(result.status, 'patched');
    assert.match(result.content, /publishLocale\(locale\)/);
    assert.match(result.content, /locale !== 'vi' && locale !== 'en'/);
    assert.equal(patch.patchContent(result.content).status, 'already');
  });

  it('patches both standard and Compact App preloads', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-language-preloads-'));
    try {
      for (const name of ['preload-render.js', 'compact-app-preload.js']) {
        fs.writeFileSync(path.join(dir, name), 'const electron = require("electron");');
      }
      const first = await patch.main(dir);
      assert.equal(first.length, 2);
      assert.deepEqual(first.map((entry) => entry.status), ['patched', 'patched']);
      for (const name of ['preload-render.js', 'compact-app-preload.js']) {
        assert.match(fs.readFileSync(path.join(dir, name), 'utf8'), /zalo-language-main-transport/);
      }
      const second = await patch.main(dir);
      assert.deepEqual(second.map((entry) => entry.status), ['already', 'already']);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('refuses malformed preload source', () => {
    assert.equal(patch.patchContent('const = ;').status, 'parse-error');
  });

  it('exposes only a vi/en publish capability in context-isolated renderers', () => {
    const sent = [];
    const context = {
      process: { contextIsolated: true },
      require(name) {
        assert.equal(name, 'electron');
        return {
          contextBridge: { exposeInMainWorld(name, api) { context.exposed = { name, api }; } },
          ipcRenderer: { send(channel, locale) { sent.push([channel, locale]); } }
        };
      }
    };
    require('node:vm').runInNewContext(patch.TRANSPORT, context);
    assert.equal(context.exposed.name, '__zaloLanguageTransport');
    assert.equal(context.exposed.api.publishLocale('en'), true);
    assert.equal(context.exposed.api.publishLocale('vi'), true);
    assert.equal(context.exposed.api.publishLocale('fr'), false);
    assert.deepEqual(sent, [
      ['zalo-language:publish-locale', 'en'],
      ['zalo-language:publish-locale', 'vi']
    ]);
  });

  it('does not call contextBridge when context isolation is disabled', () => {
    const sent = [];
    const context = {
      process: { contextIsolated: false },
      window: {},
      require() {
        return {
          contextBridge: { exposeInMainWorld() { throw new Error('must not be called'); } },
          ipcRenderer: { send(channel, locale) { sent.push([channel, locale]); } }
        };
      }
    };
    require('node:vm').runInNewContext(patch.TRANSPORT, context);
    assert.equal(typeof context.window.__zaloLanguageTransport.publishLocale, 'function');
    assert.equal(context.window.__zaloLanguageTransport.publishLocale('en'), true);
    assert.deepEqual(sent, [['zalo-language:publish-locale', 'en']]);
  });
});

})();


(function testGroup_patch_zalo_language_bridge_test_js() {
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  main: patchMain,
  patchContent,
  VALID_INDEX_GUARD,
  EVENT_BRIDGE,
  validateJavaScript
} = require('../scripts/patches/patch-zalo-language-bridge');

function fixture() {
  return `const manager = new class {
    constructor() { this.currentLanguageIndex = -1; this.updateCallback = []; }
    changeToLang(index, force = false, nodes = []) {
      if (this.currentLanguageIndex !== index || force) {
        this.currentLanguageIndex = index;
        this.translate(nodes);
        this.updateCallback.forEach((callback) => callback());
        storage.setGeneralFlag('language', this.currentLanguageIndex);
      }
    }
    getCurrentLanguageName() { return this.currentLanguageIndex === 1 ? 'en' : 'vi'; }
    currentLanguage() { return this.currentLanguageIndex; }
    callUpdate(callback) { this.updateCallback.push(callback); }
    removeUpdate(callback) { this.updateCallback = this.updateCallback.filter((item) => item !== callback); }
    translate() {}
  };
  module.exports = manager;`;
}

test('patches clean login/main and Compact App bundles into the current bridge state', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-language-bridge-'));
  const lazy = path.join(dir, 'lazy');
  fs.mkdirSync(lazy);
  const compact = path.join(dir, 'compact-app-pc.test.js');
  const login = path.join(lazy, 'default-login-main-startup-shared-worker-znotification.test.js');
  const original = fixture();
  fs.writeFileSync(compact, original);
  fs.writeFileSync(login, original);
  try {
    const first = await patchMain(dir);
    assert.deepEqual(first.results.map(result => result.status), ['patched', 'patched']);
    for (const file of [compact, login]) {
      const patched = fs.readFileSync(file, 'utf8');
      validateJavaScript(patched);
      assert.ok(patched.includes(EVENT_BRIDGE));
      assert.ok(patched.includes(VALID_INDEX_GUARD));
      assert.equal(patched.split('zalo-language:changed').length - 1, 1);
    }
    const second = await patchMain(dir);
    assert.deepEqual(second.results.map(result => result.status), ['already', 'already']);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test('inserts the current locale event after the original Zalo persistence call', () => {
  const original = fixture();
  const result = patchContent(original);
  assert.equal(result.status, 'patched');
  validateJavaScript(result.content);
  assert.ok(result.content.includes(EVENT_BRIDGE));
  assert.ok(result.content.includes(VALID_INDEX_GUARD));
  assert.ok(result.content.indexOf("storage.setGeneralFlag('language'") < result.content.indexOf('zalo-language:changed'));
  assert.equal(result.content.split('zalo-language:changed').length - 1, 1);
  assert.equal(result.content.replace(EVENT_BRIDGE, ''), original);
});

test('does not change a bundle that is already in the current patched state', () => {
  const first = patchContent(fixture());
  const second = patchContent(first.content);
  assert.equal(first.status, 'patched');
  assert.equal(second.status, 'already');
  assert.equal(second.content, first.content);
});

test('fails closed when the clean source has a missing or ambiguous Language Manager', () => {
  assert.throws(() => patchContent('const value = {};'), /Expected exactly one Zalo Language Manager/);
  assert.throws(() => patchContent(fixture() + fixture()), /Expected exactly one Zalo Language Manager/);
});


})();
