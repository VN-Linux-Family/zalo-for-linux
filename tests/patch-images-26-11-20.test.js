'use strict';

// Sending and receiving photos on Zalo 26.11.20, which is built for
// Electron 43 while we still run Electron 22 (Chromium 108).

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');

const getPathForFile = require('../scripts/patches/patch-get-path-for-file');
const jxl = require('../scripts/patches/patch-jxl-feature');

describe('Zalo 26.11.20 photo patches', () => {
  it('falls back to File.path where webUtils is missing', () => {
    const source = 'const r=require("electron");module.exports={getPathForFile:e=>r.webUtils.getPathForFile(e)};';
    const { content, status } = getPathForFile.patchContent(source);
    assert.equal(status, 'patched');
    assert.equal(getPathForFile.patchContent(content).status, 'already');

    const run = (electron) => {
      const module = { exports: {} };
      vm.runInNewContext(content, { require: () => electron, module });
      return module.exports.getPathForFile;
    };
    assert.equal(run({})({ path: '/tmp/a.jpg' }), '/tmp/a.jpg');
    assert.equal(run({ webUtils: { getPathForFile: () => '/x/b.png' } })({ path: '/tmp/a.jpg' }), '/x/b.png');
  });

  it('reports webUtils usage it does not recognise', () => {
    assert.equal(getPathForFile.patchContent('x.webUtils.somethingElse(f)').status, 'missing');
    assert.equal(getPathForFile.patchContent('no electron here').status, 'absent');
  });

  it('enables JPEG XL under the Chromium 108 name as well', () => {
    const v20 = 'kr.commandLine.appendSwitch("disable-pinch"),kr.commandLine.appendSwitch("enable-features","JXLImageFormat"),';
    const r = jxl.patchContent(v20);
    assert.equal(r.status, 'patched');
    assert.ok(r.content.includes('appendSwitch("enable-features","JXLImageFormat,JXL")'));
    assert.equal(jxl.patchContent(r.content).status, 'already');
    // 26.11.10 used the old name.
    assert.ok(jxl.patchContent('Tn.commandLine.appendSwitch("enable-features","JXL");').content
      .includes('"JXL,JXLImageFormat"'));
    // Sentry's own enable-features switch is left alone and does not count.
    assert.equal(jxl.patchContent('i.app.commandLine.appendSwitch("enable-features","DocumentPolicyIncludeJSCallStacksInCrashReports")').status, 'missing');
  });
});
