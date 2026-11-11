'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const read = (file) => fs.readFileSync(path.join(root, file), 'utf8');
const dictionary = JSON.parse(read('plugins/recalled-messages/locale.json'));
const renderer = read('plugins/recalled-messages/renderer.js');
const main = read('plugins/recalled-messages/index.js');

describe('Recalled Messages language integration', () => {
  it('uses its own schema-compatible Vietnamese and English dictionary', () => {
    assert.equal(dictionary.schemaVersion, 1);
    assert.equal(dictionary.plugin, 'recalled-messages');
    assert.ok(Object.keys(dictionary.messages).length > 0);

    const keyPattern = /^[a-z0-9][a-z0-9-]*\.[a-z0-9]+(?:[.-][a-z0-9]+)*$/;
    for (const [key, pair] of Object.entries(dictionary.messages)) {
      assert.match(key, keyPattern);
      assert.ok(key.startsWith('recalled-messages.'));
      assert.deepEqual(Object.keys(pair).sort(), ['en', 'vi']);
      assert.equal(typeof pair.vi, 'string');
      assert.equal(typeof pair.en, 'string');
      const placeholders = (value) => [...value.matchAll(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g)].map((match) => match[1]).sort();
      assert.deepEqual(placeholders(pair.vi), placeholders(pair.en), `placeholder mismatch: ${key}`);
    }
  });

  it('resolves every renderer translation through the plugin dictionary', () => {
    for (const match of renderer.matchAll(/(?<![A-Za-z0-9_.])t\('([^']+)'/g)) {
      assert.ok(dictionary.messages['recalled-messages.' + match[1]], `missing translation: ${match[1]}`);
    }
    assert.doesNotMatch(renderer, /[À-ỹĐđ]/, 'renderer must not contain hardcoded Vietnamese UI text');
    assert.match(renderer, /toLocaleString\(currentLocale === 'en' \? 'en-US' : 'vi-VN'/);
  });

  it('registers with Language Core, handles readiness, and unregisters on stop', () => {
    assert.match(renderer, /id: 'recalled-messages'/);
    assert.match(renderer, /registerPlugin\(\{/);
    assert.match(renderer, /zalo-language:ready/);
    assert.match(renderer, /unregisterLanguage\(\)/);
    assert.match(main, /require\('\.\/locale\.json'\)/);
    assert.match(main, /__zaloLinuxRecalledMessages/);
  });
});
