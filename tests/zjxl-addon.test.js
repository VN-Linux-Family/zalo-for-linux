'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const fs = require('fs');

const { ROOT, NATIVE_ADDONS_DIR } = require('../scripts/utils/paths');
const ZJXL_NODE = [
  path.join(ROOT, 'target', 'release', 'zjxl.node'),
  path.join(NATIVE_ADDONS_DIR, 'zjxl', 'target', 'release', 'zjxl.node'),
].find(p => fs.existsSync(p)) || path.join(ROOT, 'target', 'release', 'zjxl.node');
const hasBinary = fs.existsSync(ZJXL_NODE);

describe('zjxl Native Callback Contract', () => {
  it('getJxlInfo error callback is (err, null, 0)', { skip: !hasBinary ? 'zjxl.node not compiled in clean checkout' : false }, async () => {
    const mod = require(ZJXL_NODE);
    await new Promise((resolve, reject) => {
      mod.getJxlInfo(Buffer.from('not a jxl'), (err, value, flag) => {
        try {
          assert.ok(err instanceof Error);
          assert.strictEqual(value, null);
          assert.strictEqual(flag, 0);
          resolve();
        } catch (error) {
          reject(error);
        }
      });
    });
  });

  it('getJxlInfo success callback is (null, info, 1)', { skip: !hasBinary ? 'zjxl.node not compiled in clean checkout' : false }, async () => {
    const mod = require(ZJXL_NODE);
    const jxl = mod.bitmapToJxl({ buffer: Buffer.from([0x11, 0x22, 0x33]), width: 1, height: 1 });
    assert.ok(Buffer.isBuffer(jxl));

    await new Promise((resolve, reject) => {
      mod.getJxlInfo(jxl, (err, info, flag) => {
        try {
          assert.strictEqual(err, null);
          assert.strictEqual(info.width, 1);
          assert.strictEqual(info.height, 1);
          assert.strictEqual(flag, 1);
          resolve();
        } catch (error) {
          reject(error);
        }
      });
    });
  });
});
