'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');
const { ROOT, NATIVE_ADDONS_DIR } = require('../scripts/utils/paths');

const NODE_PATH = [
  path.join(ROOT, 'target', 'release', 'zfile-native.node'),
  path.join(NATIVE_ADDONS_DIR, 'zfile', 'target', 'release', 'zfile-native.node')
].find(p => fs.existsSync(p)) || path.join(ROOT, 'target', 'release', 'zfile-native.node');
const hasBinary = fs.existsSync(NODE_PATH);

describe('zfile Native Rust Addon', () => {
  it('validates patch-zfile.js exports and structure', () => {
    const patchZfile = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-zfile.js'));
    assert.strictEqual(typeof patchZfile.main, 'function');
  });

  it('binary exists and is a valid Linux ELF shared library', { skip: !hasBinary ? 'zfile-native.node not compiled in clean checkout' : false }, () => {
    assert.ok(fs.existsSync(NODE_PATH), `zfile-native.node must exist at ${NODE_PATH}`);
    const stat = fs.statSync(NODE_PATH);
    assert.ok(stat.size > 0, 'binary must not be empty');

    const fd = fs.openSync(NODE_PATH, 'r');
    const header = Buffer.alloc(4);
    fs.readSync(fd, header, 0, 4, 0);
    fs.closeSync(fd);

    assert.strictEqual(header[0], 0x7f);
    assert.strictEqual(header[1], 0x45); // 'E'
    assert.strictEqual(header[2], 0x4c); // 'L'
    assert.strictEqual(header[3], 0x46); // 'F'
  });

  it('exports all 7 required functions with expected types', { skip: !hasBinary ? 'zfile-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);
    const expected = [
      'canRead',
      'canWrite',
      'canReadAndWrite',
      'getDiskInfo',
      'getInfo',
      'copyFolder',
      'cancelCopy',
    ];

    for (const fn of expected) {
      assert.strictEqual(typeof mod[fn], 'function', `Expected export ${fn} to be a function`);
    }
  });

  it('performs synchronous permission access checks correctly', { skip: !hasBinary ? 'zfile-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);
    assert.strictEqual(mod.canRead('/etc/passwd'), true);
    assert.strictEqual(mod.canWrite('/etc/passwd'), false);
    assert.strictEqual(mod.canReadAndWrite(os.tmpdir()), true);
    assert.strictEqual(mod.canRead(''), false);
  });

  it('getDiskInfo returns volume mount map with valid structures', { skip: !hasBinary ? 'zfile-native.node not compiled in clean checkout' : false }, async () => {
    const mod = require(NODE_PATH);
    const disks = await mod.getDiskInfo();
    assert.ok(typeof disks === 'object' && disks !== null);

    const keys = Object.keys(disks);
    assert.ok(keys.length > 0, 'getDiskInfo must discover at least one mount point');
    assert.ok(keys.includes('/'), 'getDiskInfo must include root mount point');

    const rootDisk = disks['/'];
    assert.strictEqual(rootDisk.name, '/');
    assert.strictEqual(rootDisk.path, '/');
    assert.strictEqual(typeof rootDisk.isExternal, 'boolean');
    assert.ok(rootDisk.totalSpace > 0, 'totalSpace must be greater than 0');
    assert.ok(rootDisk.usedSpace >= 0, 'usedSpace must be non-negative');
  });

  it('getInfo returns accurate metadata and handles non-existent paths', { skip: !hasBinary ? 'zfile-native.node not compiled in clean checkout' : false }, async () => {
    const mod = require(NODE_PATH);

    // Test directory
    const dirInfo = await mod.getInfo(__dirname, true);
    assert.strictEqual(dirInfo.isDirectory, true);
    assert.strictEqual(typeof dirInfo.size, 'number');
    assert.strictEqual(typeof dirInfo.fileCount, 'number');
    assert.ok(dirInfo.fileCount > 0);

    // Test file
    const fileInfo = await mod.getInfo(__filename, false);
    assert.strictEqual(fileInfo.isDirectory, false);
    assert.strictEqual(fileInfo.name, path.basename(__filename));
    assert.ok(fileInfo.size > 0);
    assert.strictEqual(fileInfo.fileCount, undefined);

    // Test non-existent path
    await assert.rejects(
      async () => mod.getInfo('/path/that/definitely/does/not/exist-' + Date.now()),
      /ENOENT/
    );
  });

  it('copyFolder copies recursive directory trees and respects cancelCopy', { skip: !hasBinary ? 'zfile-native.node not compiled in clean checkout' : false }, async () => {
    const mod = require(NODE_PATH);
    const testRoot = path.join(os.tmpdir(), 'zfile-test-' + Date.now());
    const srcDir = path.join(testRoot, 'src');
    const dstDir = path.join(testRoot, 'dst');

    fs.mkdirSync(path.join(srcDir, 'sub'), { recursive: true });
    fs.writeFileSync(path.join(srcDir, 'file.txt'), 'test content');
    fs.writeFileSync(path.join(srcDir, 'sub', 'nested.txt'), 'nested content');

    try {
      const res = await mod.copyFolder(srcDir, dstDir);
      assert.strictEqual(res, dstDir);
      assert.strictEqual(fs.readFileSync(path.join(dstDir, 'file.txt'), 'utf8'), 'test content');
      assert.strictEqual(fs.readFileSync(path.join(dstDir, 'sub', 'nested.txt'), 'utf8'), 'nested content');
    } finally {
      fs.rmSync(testRoot, { recursive: true, force: true });
    }
  });
});
