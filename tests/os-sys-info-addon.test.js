'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const fs = require('fs');

const { ROOT, NATIVE_ADDONS_DIR } = require('../scripts/utils/paths');
const OS_SYS_INFO_NODE = [
  path.join(ROOT, 'target', 'release', 'os-sys-info.node'),
  path.join(NATIVE_ADDONS_DIR, 'os-sys-info', 'target', 'release', 'os-sys-info.node')
].find(p => fs.existsSync(p)) || path.join(ROOT, 'target', 'release', 'os-sys-info.node');
const hasBinary = fs.existsSync(OS_SYS_INFO_NODE);

describe('os-sys-info Native Process Snapshot Addon', () => {
  it('validates patch-os-sys-info.js exports and structure', () => {
    const patchOsSysInfo = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-os-sys-info.js'));
    assert.strictEqual(typeof patchOsSysInfo.main, 'function');
  });

  it('validates compiled binary ELF header', { skip: !hasBinary ? 'os-sys-info.node not compiled in clean checkout' : false }, () => {
    const fd = fs.openSync(OS_SYS_INFO_NODE, 'r');
    const header = Buffer.alloc(4);
    fs.readSync(fd, header, 0, 4, 0);
    fs.closeSync(fd);

    assert.strictEqual(header[0], 0x7f);
    assert.strictEqual(header.toString('ascii', 1, 4), 'ELF');
  });

  it('loads os-sys-info.node and validates metadata methods', { skip: !hasBinary ? 'os-sys-info.node not compiled in clean checkout' : false }, () => {
    const addon = require(OS_SYS_INFO_NODE);

    assert.strictEqual(typeof addon.getOsArchitecture, 'function');
    assert.strictEqual(typeof addon.getOsMachine, 'function');
    assert.strictEqual(typeof addon.getProcessSnapshot, 'function');

    assert.strictEqual(addon.getOsArchitecture(), '64-bit');
    const machine = addon.getOsMachine();
    assert.ok(machine === 'x64' || machine === 'arm64');
  });

  it('collects accurate process snapshot for current process', { skip: !hasBinary ? 'os-sys-info.node not compiled in clean checkout' : false }, async () => {
    const addon = require(OS_SYS_INFO_NODE);
    const result = await addon.getProcessSnapshot([process.pid]);

    assert.ok(result && typeof result === 'object');
    assert.ok(Array.isArray(result.samples));
    assert.ok(Array.isArray(result.errors));

    assert.strictEqual(result.samples.length, 1);
    assert.strictEqual(result.errors.length, 0);

    const sample = result.samples[0];
    assert.strictEqual(sample.pid, process.pid);
    assert.strictEqual(typeof sample.ppid, 'number');
    assert.ok(sample.ppid > 0);

    // Memory metrics
    assert.strictEqual(typeof sample.workingSet, 'number');
    assert.ok(sample.workingSet > 0);
    assert.strictEqual(typeof sample.privateBytes, 'number');
    assert.ok(sample.privateBytes >= 0);

    // Time metrics
    assert.strictEqual(typeof sample.creation100ns, 'number');
    assert.ok(sample.creation100ns > 0);
    assert.strictEqual(typeof sample.kernelTime100ns, 'number');
    assert.ok(sample.kernelTime100ns >= 0);
    assert.strictEqual(typeof sample.userTime100ns, 'number');
    assert.ok(sample.userTime100ns >= 0);

    // Wall time near Date.now()
    assert.strictEqual(typeof sample.wallUnixMs, 'number');
    assert.ok(Math.abs(sample.wallUnixMs - Date.now()) < 10000);
  });

  it('handles PID deduplication and empty inputs', { skip: !hasBinary ? 'os-sys-info.node not compiled in clean checkout' : false }, async () => {
    const addon = require(OS_SYS_INFO_NODE);

    const emptyResult = await addon.getProcessSnapshot([]);
    assert.deepStrictEqual(emptyResult, { samples: [], errors: [] });

    const duplicateResult = await addon.getProcessSnapshot([process.pid, process.pid, process.pid]);
    assert.strictEqual(duplicateResult.samples.length, 1);
    assert.strictEqual(duplicateResult.errors.length, 0);
  });

  it('filters invalid PIDs and reports non-existent PIDs in errors without panic', { skip: !hasBinary ? 'os-sys-info.node not compiled in clean checkout' : false }, async () => {
    const addon = require(OS_SYS_INFO_NODE);
    const nonExistentPid = 999999999;

    // -1, 0, 2.5 should be silently filtered out; nonExistentPid should be recorded in errors
    const result = await addon.getProcessSnapshot([-1, 0, 2.5, nonExistentPid]);

    assert.strictEqual(result.samples.length, 0);
    assert.strictEqual(result.errors.length, 1);
    assert.strictEqual(result.errors[0].pid, nonExistentPid);
    assert.strictEqual(result.errors[0].op, 'readProcessStat');
  });
});
