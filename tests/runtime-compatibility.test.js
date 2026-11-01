import { describe, expect, it } from 'bun:test';

describe('Bun Runtime Compatibility with Zalo-for-Linux Stack', () => {
  it('loads node builtins correctly under Bun', () => {
    const crypto = require('crypto');
    const zlib = require('zlib');
    const fs = require('fs');
    const path = require('path');
    const os = require('os');
    const child_process = require('child_process');

    expect(typeof crypto.createCipheriv).toBe('function');
    expect(typeof zlib.deflateSync).toBe('function');
    expect(typeof fs.readFileSync).toBe('function');
    expect(typeof path.join).toBe('function');
    expect(typeof os.platform).toBe('function');
    expect(typeof child_process.execSync).toBe('function');
  });

  it('loads and verifies @electron/asar package', () => {
    const asar = require('@electron/asar');
    expect(typeof asar.listPackage).toBe('function');
    expect(typeof asar.extractAll).toBe('function');
    expect(typeof asar.createPackage).toBe('function');
  });

  it('loads and runs sqlite3 in-memory database', async () => {
    const sqlite3 = require('sqlite3');
    expect(typeof sqlite3.Database).toBe('function');

    const db = new sqlite3.Database(':memory:');
    const result = await new Promise((resolve, reject) => {
      db.serialize(() => {
        db.run('CREATE TABLE test_table (id INT, val TEXT)');
        db.run('INSERT INTO test_table VALUES (1, "bun_works")');
        db.get('SELECT val FROM test_table WHERE id = 1', (err, row) => {
          if (err) reject(err);
          else resolve(row);
        });
      });
    });

    expect(result).toBeDefined();
    expect(result.val).toBe('bun_works');
    db.close();
  });

  it('verifies Bun global properties and high-resolution timer', () => {
    expect(typeof Bun).toBe('object');
    expect(typeof Bun.version).toBe('string');
    expect(typeof performance.now).toBe('function');
    expect(performance.now()).toBeGreaterThan(0);
  });
});
