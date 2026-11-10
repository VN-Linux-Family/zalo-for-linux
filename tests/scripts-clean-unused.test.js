'use strict';

const { describe, it, beforeEach, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const cleanUnused = require('../scripts/clean-unused');

function makeNativelibs(files) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-clean-'));
  for (const [rel, content] of Object.entries(files)) {
    const full = path.join(dir, rel);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, content);
  }
  return dir;
}

const TREE = {
  // zlang: one Linux binary plus the platform dirs and build-only files that
  // the official macOS app ships.
  'zlang/linux-x64/zlang.linux-x64.node': 'elf',
  'zlang/darwin-arm64/zlang.darwin-arm64.node': 'macho',
  'zlang/darwin-arm64/build-info.json': '{}',
  'zlang/darwin-x64/zlang.darwin-x64.node': 'macho',
  'zlang/win32-x64/zlang.win32-x64.node': 'pe',
  'zlang/index.js': 'module.exports = {}',
  'zlang/package.json': '{}',
  'zlang/index.ts': 'export {}',
  'zlang/tsconfig.json': '{}',
  'zlang/scripts/build-node.js': 'build',
  'zlang/src/lib.rs': 'fn main() {}',
  // Other addons: the darwin directory carries no linux in its path, and the
  // sqlite binding keeps its Linux sibling.
  'sqlite3/binding/napi-v6-darwin-arm64/node_sqlite3.node': 'macho',
  'sqlite3/binding/napi-v6-linux-x64/node_sqlite3.node': 'elf',
  'os-sys-info/darwin_x64/os_sys_info.node': 'macho',
};

describe('clean-unused nativelib sweep', () => {
  let dir;

  beforeEach(() => {
    dir = makeNativelibs(TREE);
  });

  afterEach(() => {
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('removes non-Linux binaries, platform dirs and build-only addon files', async () => {
    const result = await cleanUnused.cleanNativelibs(dir);

    assert.ok(fs.existsSync(path.join(dir, 'zlang/linux-x64/zlang.linux-x64.node')));
    assert.ok(fs.existsSync(path.join(dir, 'zlang/index.js')));
    assert.ok(fs.existsSync(path.join(dir, 'zlang/package.json')));
    assert.ok(fs.existsSync(path.join(dir, 'sqlite3/binding/napi-v6-linux-x64/node_sqlite3.node')));

    assert.ok(!fs.existsSync(path.join(dir, 'zlang/darwin-arm64')));
    assert.ok(!fs.existsSync(path.join(dir, 'zlang/darwin-x64')));
    assert.ok(!fs.existsSync(path.join(dir, 'zlang/win32-x64')));
    assert.ok(!fs.existsSync(path.join(dir, 'zlang/index.ts')));
    assert.ok(!fs.existsSync(path.join(dir, 'zlang/tsconfig.json')));
    assert.ok(!fs.existsSync(path.join(dir, 'zlang/scripts')));
    assert.ok(!fs.existsSync(path.join(dir, 'zlang/src')));
    assert.ok(!fs.existsSync(path.join(dir, 'os-sys-info/darwin_x64')));
    // Emptied by the sweep, then pruned.
    assert.ok(!fs.existsSync(path.join(dir, 'sqlite3/binding/napi-v6-darwin-arm64')));

    assert.deepEqual(result.leftovers, []);
    assert.ok(result.binaries >= 4);
  });

  it('is idempotent', async () => {
    await cleanUnused.cleanNativelibs(dir);
    const second = await cleanUnused.cleanNativelibs(dir);

    assert.deepEqual(second, {
      binaries: 0,
      addonFiles: 0,
      platformDirs: 0,
      emptyDirs: 0,
      leftovers: [],
    });
  });
});
