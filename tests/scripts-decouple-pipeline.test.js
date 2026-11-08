'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { execFileSync } = require('child_process');

const extract = require('../scripts/extract');
const applyPatches = require('../scripts/apply-patches');
const prepareApp = require('../scripts/prepare-app');
const { PATCHES_DIR, ROOT } = require('../scripts/utils/paths');

describe('Pipeline Decoupling & Pure Patch Hygiene', () => {
  it('exports expected contract from scripts/extract.js', () => {
    assert.strictEqual(typeof extract.main, 'function');
    assert.strictEqual(typeof extract.extractDMG, 'function');
    assert.strictEqual(typeof extract.extractWindows, 'function');
    assert.strictEqual(typeof extract.extractAppAsar, 'function');
    assert.strictEqual(typeof extract.parseVersion, 'function');
    assert.strictEqual(typeof extract.compareVersions, 'function');
  });

  it('correctly parses semantic versions from filenames in extract.js', () => {
    const v1 = extract.parseVersion('ZaloSetup-universal-26.11.10.dmg');
    assert.deepStrictEqual(v1, { major: 26, minor: 11, patch: 10, raw: '26.11.10' });

    const v2 = extract.parseVersion('ZaloSetup-25.04.01.exe');
    assert.deepStrictEqual(v2, { major: 25, minor: 4, patch: 1, raw: '25.04.01' });

    const invalid = extract.parseVersion('Zalo-nonversion.dmg');
    assert.strictEqual(invalid, null);
  });

  it('correctly compares version objects', () => {
    const vOld = { major: 25, minor: 1, patch: 0 };
    const vNew = { major: 26, minor: 0, patch: 0 };
    const vPatch = { major: 26, minor: 0, patch: 1 };

    assert.ok(extract.compareVersions(vNew, vOld) > 0);
    assert.ok(extract.compareVersions(vOld, vNew) < 0);
    assert.ok(extract.compareVersions(vPatch, vNew) > 0);
    assert.strictEqual(extract.compareVersions(vNew, vNew), 0);
  });

  it('exports expected contract from scripts/apply-patches.js and scripts/prepare-app.js', () => {
    assert.strictEqual(typeof applyPatches.main, 'function');
    assert.strictEqual(typeof prepareApp.main, 'function');
  });

  it('verifies all scripts in scripts/patches/ are pure-patch (no inline builder execution)', () => {
    const patchFiles = fs.readdirSync(PATCHES_DIR).filter(f => f.startsWith('patch-') && f.endsWith('.js'));
    assert.ok(patchFiles.length >= 25, `Expected at least 25 patch files, found ${patchFiles.length}`);

    for (const file of patchFiles) {
      const filePath = path.join(PATCHES_DIR, file);
      const content = fs.readFileSync(filePath, 'utf8');

      // Assert no patch script directly executes BUILDER_SCRIPT or cargo build
      assert.doesNotMatch(
        content,
        /execSync\([^)]*BUILDER_SCRIPT/,
        `${file} must not execute BUILDER_SCRIPT inline during patching`
      );
      assert.doesNotMatch(
        content,
        /execSync\(['"`]cargo build/,
        `${file} must not execute cargo build inline during patching`
      );
    }
  });

  it('verifies all scripts in scripts/patches/ import all paths symbols they reference', () => {
    const pathsModule = require('../scripts/utils/paths');
    const pathKeys = Object.keys(pathsModule);
    const patchFiles = fs.readdirSync(PATCHES_DIR).filter(f => f.startsWith('patch-') && f.endsWith('.js'));

    for (const file of patchFiles) {
      const content = fs.readFileSync(path.join(PATCHES_DIR, file), 'utf8');
      const importLine = content.split('\n').find(l => l.includes("require('../utils/paths')") || l.includes('require("../utils/paths")'));
      assert.ok(importLine, `${file} should import from paths.js`);

      for (const key of pathKeys) {
        if (content.includes(key)) {
          assert.ok(
            importLine.includes(key) || content.includes(key + ' ='),
            `${file} references ${key} without importing it from paths.js`
          );
        }
      }
    }
  });

  it('supports path overrides via ZALO_APP_DIR, ZALO_TEMP_DIR, and ZALO_PREBUILT_DIR', () => {
    const testCode = `
      const paths = require('./scripts/utils/paths');
      const fs = require('fs');
      const path = require('path');
      console.log(JSON.stringify({
        appDir: paths.APP_DIR,
        tempDir: paths.TEMP_DIR,
        distDir: paths.DIST_DIR,
        prebuiltDir: paths.PREBUILT_DIR,
      }));
    `;

    const customApp = '/tmp/test-zalo-app-override';
    const customTemp = '/tmp/test-zalo-temp-override';
    const customDist = '/tmp/test-zalo-dist-override';
    const customPrebuilt = '/tmp/test-zalo-prebuilt-override';

    const output = execFileSync(process.execPath, ['-e', testCode], {
      cwd: ROOT,
      env: {
        ...process.env,
        ZALO_APP_DIR: customApp,
        ZALO_TEMP_DIR: customTemp,
        ZALO_DIST_DIR: customDist,
        ZALO_PREBUILT_DIR: customPrebuilt,
      },
      encoding: 'utf8',
    });

    const parsed = JSON.parse(output.trim());
    assert.strictEqual(parsed.appDir, customApp);
    assert.strictEqual(parsed.tempDir, customTemp);
    assert.strictEqual(parsed.distDir, customDist);
    assert.strictEqual(parsed.prebuiltDir, customPrebuilt);
  });
});
