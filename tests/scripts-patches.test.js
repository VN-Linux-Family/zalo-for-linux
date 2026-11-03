'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const logger = require(path.join(__dirname, '..', 'scripts', 'utils', 'logger'));

describe('Scripts & Patches Integration', () => {
  it('loads logger and invokes logging methods without error', () => {
    assert.strictEqual(typeof logger.info, 'function');
    assert.strictEqual(typeof logger.warn, 'function');
    assert.strictEqual(typeof logger.error, 'function');
    assert.strictEqual(typeof logger.success, 'function');
    assert.strictEqual(typeof logger.dim, 'function');
  });

  it('validates that all patch scripts in scripts/patches load and export main', () => {
    const patchesDir = path.join(__dirname, '..', 'scripts', 'patches');
    const files = fs.readdirSync(patchesDir).filter(f => f.endsWith('.js'));

    assert.ok(files.length >= 25, `Expected >= 25 patch files, got ${files.length}`);

    for (const file of files) {
      const fullPath = path.join(patchesDir, file);
      const mod = require(fullPath);
      assert.strictEqual(typeof mod.main, 'function', `${file} does not export main function`);
    }
  });
});
