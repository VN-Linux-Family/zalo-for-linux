'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const fs = require('fs');

const { ROOT, NATIVE_ADDONS_DIR } = require('../scripts/utils/paths');
const ZIMAGE_NODE = [
  path.join(ROOT, 'target', 'release', 'zimage.node'),
  path.join(NATIVE_ADDONS_DIR, 'zimage', 'target', 'release', 'zimage.node'),
].find(p => fs.existsSync(p)) || path.join(ROOT, 'target', 'release', 'zimage.node');
const hasBinary = fs.existsSync(ZIMAGE_NODE);

// 1x1 transparent PNG.
const PNG_1X1 = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==',
  'base64'
);

describe('zimage Native Thumbnail Addon', () => {
  it('thumbnail calls back (null, Buffer) on success', { skip: !hasBinary ? 'zimage.node not compiled in clean checkout' : false }, async () => {
    const mod = require(ZIMAGE_NODE);
    const out = await new Promise((resolve, reject) => {
      mod.thumbnail(
        { buffer: PNG_1X1, width: 1, height: 1, format: 'png' },
        (err, buf) => {
          try {
            assert.strictEqual(err, null);
            assert.ok(Buffer.isBuffer(buf));
            resolve(buf);
          } catch (error) {
            reject(error);
          }
        }
      );
    });
    assert.ok(out.length > 0);
  });

  it('thumbnail calls back (err) first on failure', { skip: !hasBinary ? 'zimage.node not compiled in clean checkout' : false }, async () => {
    const mod = require(ZIMAGE_NODE);
    await new Promise((resolve, reject) => {
      mod.thumbnail(
        { buffer: Buffer.from([1, 2, 3]), width: 1, height: 1, format: 'png' },
        (err, buf) => {
          try {
            assert.ok(err instanceof Error);
            assert.strictEqual(buf, undefined);
            resolve();
          } catch (error) {
            reject(error);
          }
        }
      );
    });
  });
});
