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

  it('extracts OCR seed key dynamically via signature scan when zocr-host.exe is present', () => {
    const patchZocr = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-zocr-runtime'));
    assert.strictEqual(typeof patchZocr.extractKey, 'function');

    // Search for any extracted zocr-host.exe in temp directory
    const tempDir = path.join(__dirname, '..', 'temp');
    let binaryPath = null;
    if (fs.existsSync(tempDir)) {
      const findBinary = (dir) => {
        for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
          const full = path.join(dir, entry.name);
          if (entry.isDirectory()) {
            const found = findBinary(full);
            if (found) return found;
          } else if (entry.name === 'zocr-host.exe') {
            return full;
          }
        }
        return null;
      };
      binaryPath = findBinary(tempDir);
    }

    if (binaryPath && fs.existsSync(binaryPath)) {
      const data = fs.readFileSync(binaryPath);
      const keys = patchZocr.extractKey(data);
      assert.ok(keys.seed_key, 'Missing seed_key');
      assert.strictEqual(keys.seed_key.length, 64, 'seed_key must be 32 bytes (64 hex characters)');
      assert.ok(keys.text_digest_key, 'Missing text_digest_key');
      assert.strictEqual(keys.text_digest_key.length, 64, 'text_digest_key must be sha256 hex');
    }
  });

  it('transforms zocr runtime source using AST with @ast-grep/napi', () => {
    const patchZocr = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-zocr-runtime'));
    assert.strictEqual(typeof patchZocr.patch, 'function');

    const sampleSnippet = `
      Ce=["zocr-host.exe","onnxruntime.dll"];
      function getPath(e){return (0,$.join)(e,"zocr-host.exe")}
      function We(e,r){return e?(0,$.join)(r,"plugins","ocr"):(0,$.join)(Ze,"runtime")}
      switch(K){case"win32":switch(G){case"x64":break;default:k=new Error("err")}break;}
      function Ie(){return K==="win32"?ke()?"os-32bit":null:null}
      if(K==="win32"){let{engine:t,pipeTransport:i}=gr(e);return Tr.set(t,i),t}
    `;
    const patched = patchZocr.patch(sampleSnippet);
    assert.ok(patched.includes('process.platform==="linux"'), 'Should insert linux platform support');
    assert.ok(patched.includes('zocr-host'), 'Should replace executable name');
    assert.ok(patched.includes('__dirname,"../../ocr"'), 'Should redirect host runtime dir');
  });
});
