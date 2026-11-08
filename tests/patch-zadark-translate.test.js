'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');

const {
  main,
  patchFile,
  OLD_TRANSLATE_MIN,
  NEW_TRANSLATE_MIN,
} = require('../scripts/patches/patch-zadark-translate');

describe('ZaDark Translate Patch', () => {
  it('exports required main and patchFile functions', () => {
    assert.strictEqual(typeof main, 'function');
    assert.strictEqual(typeof patchFile, 'function');
  });

  it('transforms legacy api.zadark.com call to clients5.google.com', () => {
    const tmpFile = path.join(os.tmpdir(), `test-zadark-translate-${Date.now()}.js`);
    fs.writeFileSync(tmpFile, `const code = ${OLD_TRANSLATE_MIN};`, 'utf8');

    try {
      const patched = patchFile(tmpFile);
      assert.strictEqual(patched, true, 'patchFile should return true on first run');

      const content = fs.readFileSync(tmpFile, 'utf8');
      assert.ok(content.includes('clients5.google.com'), 'Should include clients5.google.com');
      assert.ok(!content.includes('api.zadark.com'), 'Should no longer include api.zadark.com');

      // Idempotency
      const secondRun = patchFile(tmpFile);
      assert.strictEqual(secondRun, false, 'patchFile should return false if already patched');
    } finally {
      if (fs.existsSync(tmpFile)) {
        fs.unlinkSync(tmpFile);
      }
    }
  });

  it('returns false for missing or non-matching files', () => {
    const nonExistent = path.join(os.tmpdir(), `non-existent-${Date.now()}.js`);
    assert.strictEqual(patchFile(nonExistent), false);

    const tmpFile = path.join(os.tmpdir(), `test-other-${Date.now()}.js`);
    fs.writeFileSync(tmpFile, 'console.log("hello");', 'utf8');
    try {
      assert.strictEqual(patchFile(tmpFile), false);
    } finally {
      if (fs.existsSync(tmpFile)) {
        fs.unlinkSync(tmpFile);
      }
    }
  });
});
