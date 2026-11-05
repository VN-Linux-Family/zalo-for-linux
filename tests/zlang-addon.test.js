'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const fs = require('fs');

const ZLANG_NODE = path.join(__dirname, '..', 'nativelibs', 'zlang', 'target', 'release', 'zlang.node');
const hasBinary = fs.existsSync(ZLANG_NODE);

describe('zlang Native Language Detection Addon', () => {
  it('validates patch-zlang.js exports and structure', () => {
    const patchZlang = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-zlang.js'));
    assert.strictEqual(typeof patchZlang.main, 'function');
  });

  it('loads zlang.node and validates metadata methods', { skip: !hasBinary ? 'zlang.node not compiled in clean checkout' : false }, () => {
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

  it('detects Vietnamese text accurately', { skip: !hasBinary ? 'zlang.node not compiled in clean checkout' : false }, async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('Xin chào các bạn, đây là kiểm tra ngôn ngữ tiếng Việt trên hệ điều hành Linux.');

    assert.ok(Array.isArray(results));
    assert.ok(results.length > 0);
    assert.strictEqual(results[0].tag, 'vi');
    assert.ok(typeof results[0].confidence === 'number');
    assert.ok(results[0].confidence > 0.8);
  });

  it('detects English text accurately', { skip: !hasBinary ? 'zlang.node not compiled in clean checkout' : false }, async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('Hello world, this is a clean-room native language detection test.');

    assert.ok(Array.isArray(results));
    assert.ok(results.length > 0);
    assert.strictEqual(results[0].tag, 'en');
    assert.ok(typeof results[0].confidence === 'number');
    assert.ok(results[0].confidence > 0.8);
  });

  it('detects Japanese text accurately', { skip: !hasBinary ? 'zlang.node not compiled in clean checkout' : false }, async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('こんにちは世界、これは日本語の言語認識テストです。');

    assert.ok(Array.isArray(results));
    assert.ok(results.length > 0);
    assert.strictEqual(results[0].tag, 'ja');
    assert.ok(typeof results[0].confidence === 'number');
    assert.ok(results[0].confidence > 0.8);
  });

  it('detects Chinese text accurately', { skip: !hasBinary ? 'zlang.node not compiled in clean checkout' : false }, async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('你好世界，这是一个中文语言测试。');

    assert.ok(Array.isArray(results));
    assert.ok(results.length > 0);
    assert.ok(results[0].tag === 'zh-Hans' || results[0].tag === 'zh');
    assert.ok(typeof results[0].confidence === 'number');
    assert.ok(results[0].confidence > 0.8);
  });

  it('returns empty array for very short text as per contract', { skip: !hasBinary ? 'zlang.node not compiled in clean checkout' : false }, async () => {
    const zlang = require(ZLANG_NODE);
    const results = await zlang.detect('hi');

    assert.ok(Array.isArray(results));
    assert.strictEqual(results.length, 0);
  });
});
