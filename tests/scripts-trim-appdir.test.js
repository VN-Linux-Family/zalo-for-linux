'use strict';

const { describe, it, beforeEach, afterEach } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');

const trim = require('../scripts/trim-appdir');

const ENV_KEYS = ['ZALO_KEEP_LOCALES', 'ZALO_TRIM_SWIFTSHADER', 'ZALO_TRIM_VULKAN'];

function makeAppDir(files) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-trim-'));
  for (const [rel, content] of Object.entries(files)) {
    const full = path.join(dir, rel);
    fs.mkdirSync(path.dirname(full), { recursive: true });
    fs.writeFileSync(full, content);
  }
  return dir;
}

function listLocales(dir) {
  return fs.readdirSync(path.join(dir, 'locales')).sort();
}

const LOCALES = {
  'locales/vi.pak': 'vi',
  'locales/en-US.pak': 'en-US',
  'locales/en-GB.pak': 'en-GB',
  'locales/fr.pak': 'fr',
  'locales/de.pak': 'de',
  'locales/ja.pak': 'ja',
};

function savedEnv() {
  return Object.fromEntries(ENV_KEYS.map((key) => [key, process.env[key]]));
}

function restoreEnv(snapshot) {
  for (const key of ENV_KEYS) {
    if (snapshot[key] === undefined) delete process.env[key];
    else process.env[key] = snapshot[key];
  }
}

describe('trim-appdir', () => {
  let envSnapshot;
  let dirs = [];

  beforeEach(() => {
    envSnapshot = savedEnv();
    for (const key of ENV_KEYS) delete process.env[key];
    dirs = [];
  });

  afterEach(() => {
    restoreEnv(envSnapshot);
    for (const dir of dirs) fs.rmSync(dir, { recursive: true, force: true });
  });

  it('keeps only the Vietnamese and English locale packs by default', async () => {
    const dir = makeAppDir({
      ...LOCALES,
      'libvk_swiftshader.so': 'swiftshader',
      'libvulkan.so.1': 'vulkan',
    });
    dirs.push(dir);

    const result = await trim.main(dir);

    assert.deepEqual(listLocales(dir), ['en-GB.pak', 'en-US.pak', 'vi.pak']);
    assert.ok(result.removedBytes > 0);
    // GPU fallbacks are opt-in: they stay by default.
    assert.ok(fs.existsSync(path.join(dir, 'libvk_swiftshader.so')));
    assert.ok(fs.existsSync(path.join(dir, 'libvulkan.so.1')));
  });

  it('is idempotent', async () => {
    const dir = makeAppDir(LOCALES);
    dirs.push(dir);

    await trim.main(dir);
    const second = await trim.main(dir);

    assert.strictEqual(second.removedBytes, 0);
    assert.deepEqual(listLocales(dir), ['en-GB.pak', 'en-US.pak', 'vi.pak']);
  });

  it('honours ZALO_KEEP_LOCALES', async () => {
    const dir = makeAppDir(LOCALES);
    dirs.push(dir);
    process.env.ZALO_KEEP_LOCALES = '1';

    await trim.main(dir);

    assert.deepEqual(listLocales(dir), ['de.pak', 'en-GB.pak', 'en-US.pak', 'fr.pak', 'ja.pak', 'vi.pak']);
  });

  it('drops GPU fallbacks only when explicitly requested', async () => {
    const dir = makeAppDir({
      ...LOCALES,
      'libvk_swiftshader.so': 'swiftshader',
      'libvk_swiftshader_icd.json': '{}',
      'libvulkan.so.1': 'vulkan',
    });
    dirs.push(dir);
    process.env.ZALO_TRIM_SWIFTSHADER = '1';
    process.env.ZALO_TRIM_VULKAN = '1';

    await trim.main(dir);

    assert.ok(!fs.existsSync(path.join(dir, 'libvk_swiftshader.so')));
    assert.ok(!fs.existsSync(path.join(dir, 'libvk_swiftshader_icd.json')));
    assert.ok(!fs.existsSync(path.join(dir, 'libvulkan.so.1')));
  });

  it('does not fail when the AppDir is missing', async () => {
    const result = await trim.main(path.join(os.tmpdir(), 'zalo-trim-does-not-exist'));

    assert.deepStrictEqual(result, { removedBytes: 0 });
  });
});
