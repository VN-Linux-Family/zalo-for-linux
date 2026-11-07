'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const paths = require('../scripts/utils/paths');

describe('Project Paths Centralization', () => {
  it('exports all expected source and destination path constants', () => {
    const expectedKeys = [
      'ROOT',
      'APP_DIR',
      'TEMP_DIR',
      'DIST_DIR',
      'PLUGINS_DIR',
      'NODE_MODULES_DIR',
      'VENDOR_DIR',
      'ZADARK_VENDOR_DIR',
      'NATIVELIBS_DIR',
      'ZOCR_DIR',
      'ZCALL_DIR',
      'SCRIPTS_DIR',
      'PATCHES_DIR',
      'BUILDER_SCRIPT',
      'BUILDER_RUST_SCRIPT',
      'APP_MAIN_DIST',
      'APP_PC_DIST',
      'APP_NATIVE',
      'APP_NATIVE_LIBS',
      'APP_OCR_DIR',
      'APP_ZCALL_ASSETS',
    ];

    for (const key of expectedKeys) {
      assert.ok(paths[key], `Missing exported path: ${key}`);
      assert.strictEqual(typeof paths[key], 'string', `${key} must be a string`);
      assert.ok(path.isAbsolute(paths[key]), `${key} must be an absolute path: ${paths[key]}`);
    }
  });

  it('resolves core repository directories that exist in the working tree', () => {
    assert.ok(fs.existsSync(paths.ROOT), 'ROOT directory must exist');
    assert.ok(fs.existsSync(paths.SCRIPTS_DIR), 'SCRIPTS_DIR must exist');
    assert.ok(fs.existsSync(paths.PATCHES_DIR), 'PATCHES_DIR must exist');
    assert.ok(fs.existsSync(paths.NATIVELIBS_DIR), 'NATIVELIBS_DIR must exist');
    assert.ok(fs.existsSync(paths.PLUGINS_DIR), 'PLUGINS_DIR must exist');
    assert.ok(fs.existsSync(paths.VENDOR_DIR), 'VENDOR_DIR must exist');
    assert.ok(fs.existsSync(paths.ZADARK_VENDOR_DIR), 'ZADARK_VENDOR_DIR must exist');
    assert.ok(fs.existsSync(paths.ZCALL_DIR), 'ZCALL_DIR must exist');
    assert.ok(fs.existsSync(paths.BUILDER_SCRIPT), 'BUILDER_SCRIPT must exist');
    assert.ok(fs.existsSync(paths.BUILDER_RUST_SCRIPT), 'BUILDER_RUST_SCRIPT must exist');
  });

  it('anchors all extracted app runtime destinations under APP_DIR', () => {
    const appDestinations = [
      paths.APP_MAIN_DIST,
      paths.APP_PC_DIST,
      paths.APP_NATIVE,
      paths.APP_NATIVE_LIBS,
      paths.APP_OCR_DIR,
      paths.APP_ZCALL_ASSETS,
    ];

    for (const dest of appDestinations) {
      assert.ok(
        dest.startsWith(paths.APP_DIR + path.sep),
        `${dest} must be an immediate or nested child of ${paths.APP_DIR}`
      );
    }
  });

  it('enforces immutability via Object.freeze', () => {
    assert.strictEqual(Object.isFrozen(paths), true, 'paths object must be frozen');
    assert.throws(() => {
      paths.NEW_KEY = 'test';
    }, TypeError);
  });
});
