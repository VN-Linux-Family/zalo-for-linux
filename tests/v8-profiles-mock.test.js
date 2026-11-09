'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
const patchV8Profiles = require('../scripts/patches/patch-v8-profiles');
const V8_PROFILES_INDEX = path.join(ROOT, 'app', 'native', 'nativelibs', 'v8-profiles', 'index.js');

describe('v8-profiles Zero-Overhead JS Mock Stub', () => {
  it('exports main function from patch-v8-profiles.js', () => {
    assert.strictEqual(typeof patchV8Profiles.main, 'function');
  });

  it('installs mock stub and validates runtime behavior under Node.js', async () => {
    await patchV8Profiles.main();

    assert.ok(fs.existsSync(V8_PROFILES_INDEX), 'v8-profiles/index.js must exist');

    delete require.cache[require.resolve(V8_PROFILES_INDEX)];
    const profiler = require(V8_PROFILES_INDEX);

    assert.strictEqual(typeof profiler.startProfiling, 'function');
    assert.strictEqual(typeof profiler.stopProfiling, 'function');
    assert.strictEqual(typeof profiler.setSamplingInterval, 'function');
    assert.strictEqual(typeof profiler.deleteAllProfiles, 'function');
    assert.strictEqual(typeof profiler.profiles, 'object');

    // Test startProfiling
    profiler.startProfiling('benchmark-test');

    // Test stopProfiling returns compliant CpuProfile
    const profile = profiler.stopProfiling('benchmark-test');
    assert.ok(profile, 'profile must be returned');
    assert.strictEqual(profile.title, 'benchmark-test');
    assert.strictEqual(profile.typeId, 'CPU');
    assert.ok(typeof profile.startTime === 'number');
    assert.ok(typeof profile.endTime === 'number');
    assert.ok(Array.isArray(profile.samples));
    assert.ok(Array.isArray(profile.timestamps));
    assert.ok(profile.head);
    assert.strictEqual(profile.head.functionName, '(root)');
    assert.ok(Array.isArray(profile.head.children));

    const header = profile.getHeader();
    assert.deepStrictEqual(header, {
      typeId: 'CPU',
      uid: 1,
      title: 'benchmark-test'
    });

    // Verify process.profiler integration
    assert.strictEqual(process.profiler, profiler);

    // Verify deleteAllProfiles
    profiler.deleteAllProfiles();
    assert.strictEqual(Object.keys(profiler.profiles).length, 0);
  });
});
