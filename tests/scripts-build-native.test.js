'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const { getAvailableTargets } = require('../scripts/build-native');
const { getOnnxRuntimeUrl, ONNXRUNTIME_VERSION } = require('../scripts/download');
const { ROOT } = require('../scripts/utils/paths');

describe('Native Build Orchestrator & Downloads', () => {
  it('discovers all 10 native addons and 2 native engines (pure Rust)', () => {
    const targets = getAvailableTargets();
    const names = targets.map(t => t.name);

    const expectedAddons = [
      'db-cross-v4',
      'file-utilities',
      'file-utils',
      'mp4thumb',
      'os-sys-info',
      'zfile',
      'zimage',
      'zjxl',
      'zlang',
      'zwalker',
    ];

    for (const addon of expectedAddons) {
      assert.ok(names.includes(addon), `Expected addon ${addon} to be discovered in native targets`);
      const t = targets.find(item => item.name === addon);
      assert.strictEqual(t.category, 'addon');
      assert.ok(fs.existsSync(path.join(t.dir, 'Cargo.toml')), `Addon ${addon} must have Cargo.toml (pure Rust)`);
    }

    assert.ok(names.includes('zocr'), 'Expected engine zocr to be discovered');
    assert.ok(names.includes('zcall-audio'), 'Expected engine zcall-audio to be discovered');
  });

  it('generates correct OnnxRuntime download URL for current architecture', () => {
    assert.ok(ONNXRUNTIME_VERSION, 'ONNXRUNTIME_VERSION must be defined');
    const url = getOnnxRuntimeUrl();
    assert.ok(url.startsWith('https://github.com/microsoft/onnxruntime/releases/download/'));
    assert.ok(url.includes(`v${ONNXRUNTIME_VERSION}`));
    const arch = process.arch === 'arm64' ? 'aarch64' : 'x64';
    assert.ok(url.includes(`onnxruntime-linux-${arch}-${ONNXRUNTIME_VERSION}.tgz`));
  });
});
