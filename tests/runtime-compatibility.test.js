'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');

describe('Node.js Runtime Compatibility with Zalo-for-Linux Stack', () => {
  it('loads node builtins correctly', () => {
    const crypto = require('crypto');
    const zlib = require('zlib');
    const fs = require('fs');
    const path = require('path');
    const os = require('os');
    const child_process = require('child_process');

    assert.strictEqual(typeof crypto.createCipheriv, 'function');
    assert.strictEqual(typeof zlib.deflateSync, 'function');
    assert.strictEqual(typeof fs.readFileSync, 'function');
    assert.strictEqual(typeof path.join, 'function');
    assert.strictEqual(typeof os.platform, 'function');
    assert.strictEqual(typeof child_process.execSync, 'function');
  });

  it('loads and verifies @electron/asar package', () => {
    const asar = require('@electron/asar');
    assert.strictEqual(typeof asar.listPackage, 'function');
    assert.strictEqual(typeof asar.extractAll, 'function');
    assert.strictEqual(typeof asar.createPackage, 'function');
  });

  it('loads and runs sqlite3 in-memory database', async () => {
    const sqlite3 = require('sqlite3');
    assert.strictEqual(typeof sqlite3.Database, 'function');

    const db = new sqlite3.Database(':memory:');
    const result = await new Promise((resolve, reject) => {
      db.serialize(() => {
        db.run('CREATE TABLE test_table (id INT, val TEXT)');
        db.run('INSERT INTO test_table VALUES (1, "sqlite_works")');
        db.get('SELECT val FROM test_table WHERE id = 1', (err, row) => {
          if (err) reject(err);
          else resolve(row);
        });
      });
    });

    assert.ok(result !== undefined);
    assert.strictEqual(result.val, 'sqlite_works');
    db.close();
  });

  it('verifies Node runtime properties and high-resolution timer', () => {
    assert.strictEqual(typeof process.versions.node, 'string');
    assert.strictEqual(typeof performance.now, 'function');
    assert.ok(performance.now() > 0);
  });
});
