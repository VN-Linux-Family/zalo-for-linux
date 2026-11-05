'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const fs = require('fs');

const ZLANG_NODE = path.join(__dirname, '..', 'nativelibs', 'zlang', 'target', 'release', 'zlang.node');

describe('zlang Native Language Detection Addon', () => {
  it('loads zlang.node and validates metadata methods', () => {
    assert.ok(fs.existsSync(ZLANG_NODE), `zlang.node binary must exist at ${ZLANG_NODE}`);
    const zlang = require(ZLANG_NODE);

    assert.strictEqual(typeof zlang.version, 'function');
    assert.strictEqual(typeof zlang.backend, 'function');
    assert.strictEqual(typeof zlang.available, 'function');
    assert.strictEqual(typeof zlang.scores, 'function');
    assert.strictEqual(typeof zlang.detect, 'function');

    assert.strictEqual(zlang.available(), true);
    assert.strictEqual(zlang.backend(), 'whatlang-linux');
    assert.strictEqual(zlang.scores(), 'probability');
    assert.strictEqual(typeof zlang.version(), 'string');
  });

  it('detects Vietnamese text accurately', async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('Xin chào các bạn, đây là kiểm tra ngôn ngữ tiếng Việt trên hệ điều hành Linux.');

    assert.ok(Array.isArray(results));
    assert.ok(results.length > 0);
    assert.strictEqual(results[0].tag, 'vi');
    assert.ok(typeof results[0].confidence === 'number');
    assert.ok(results[0].confidence > 0.8);
  });

  it('detects English text accurately', async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('Hello world, this is a clean-room native language detection test.');

    assert.ok(Array.isArray(results));
    assert.ok(results.length > 0);
    assert.strictEqual(results[0].tag, 'en');
    assert.ok(typeof results[0].confidence === 'number');
    assert.ok(results[0].confidence > 0.8);
  });

  it('detects Japanese text accurately', async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('こんにちは世界、これは日本語の言語認識テストです。');

    assert.ok(Array.isArray(results));
    assert.ok(results.length > 0);
    assert.strictEqual(results[0].tag, 'ja');
    assert.ok(typeof results[0].confidence === 'number');
    assert.ok(results[0].confidence > 0.8);
  });

  it('returns empty array for very short text as per contract', async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('hi');

    assert.ok(Array.isArray(results));
    assert.strictEqual(results.length, 0);
  });
});
