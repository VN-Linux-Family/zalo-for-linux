'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');
const {
  findPreviousTag,
  parseGitLog,
  generateReleaseBody,
} = require('../scripts/utils/generate-release-notes');

describe('Release Notes & Changelog Generator', () => {
  it('identifies previous release tag from tag list correctly', () => {
    const tags = ['26.11.10', '26.10.20', '26.10.11', '26.10.10'];
    assert.strictEqual(findPreviousTag(tags, '26.11.10'), '26.10.20');
    assert.strictEqual(findPreviousTag(tags, '26.10.20'), '26.10.11');
    assert.strictEqual(findPreviousTag(tags, 'nonexistent'), '26.11.10');
    assert.strictEqual(findPreviousTag([], '26.11.10'), '');
  });

  it('categorizes commits into single-word level 3 headings and detects new contributors', () => {
    const mockRecords = [
      'Merge pull request #160 from ngxccc/feature/perf-patches-and-tests\x1fabc3\x1fNguyen Hoang Ky\x1fperf(patches): optimize platform patch scanning',
      'Merge pull request #170 from dont-wait/home-manager\x1fabc6\x1fNguyen Hoang Ky\x1fsupport home-manager',
      'Merge pull request #181 from ngxccc/chore/release-template\x1fabc5\x1fNguyen Hoang Ky\x1fci: centralize release template',
      'Merge pull request #182 from ngxccc/feature/zocr-signature-scanner\x1fabc2\x1fNguyen Hoang Ky\x1frefactor(patches): migrate minified runtime patches to ast-grep AST engine',
      'Merge pull request #184 from ngxccc/feature/zlang-native-engine\x1fabc1\x1fNguyen Hoang Ky\x1ffeat(zlang): implement native rust language detection engine',
      'Merge pull request #189 from nguyenphivn/fix/titlebar\x1fabc4\x1fNguyen Hoang Ky\x1ffix(patches): anchor titlebar patch to the main window',
    ];

    const priorAuthors = new Set(['dmkha241', 'nguyen hoang ky']);
    const changelog = parseGitLog(mockRecords, priorAuthors, 'VN-Linux-Family/zalo-for-linux', '26.11.10', '26.10.20');

    assert.ok(changelog.includes("## What's Changed"));
    assert.ok(changelog.includes('### Features'));
    assert.ok(changelog.includes('### Fixes'));
    assert.ok(changelog.includes('### Performance'));
    assert.ok(changelog.includes('### Refactor'));
    assert.ok(changelog.includes('### CI'));
    assert.ok(changelog.includes('### Maintenance'));
    assert.ok(changelog.includes('fix(patches): anchor titlebar patch to the main window by @nguyenphivn'));
    assert.ok(changelog.includes('feat(zlang): implement native rust language detection engine by @ngxccc'));

    // New Contributors verification
    assert.ok(changelog.includes('### New Contributors'));
    assert.ok(changelog.includes('* @ngxccc made their first contribution in https://github.com/VN-Linux-Family/zalo-for-linux/pull/160'));
    assert.ok(changelog.includes('* @nguyenphivn made their first contribution in https://github.com/VN-Linux-Family/zalo-for-linux/pull/189'));
    assert.ok(changelog.includes('* @dont-wait made their first contribution in https://github.com/VN-Linux-Family/zalo-for-linux/pull/170'));

    assert.ok(changelog.includes('**Full Changelog**: https://github.com/VN-Linux-Family/zalo-for-linux/compare/26.10.20...26.11.10'));
  });

  it('generates full release body merging template and changelog safely in shallow clone', () => {
    const tmpOut = path.join(os.tmpdir(), `test-release-body-${Date.now()}.md`);
    try {
      const body = generateReleaseBody('26.11.10', tmpOut);
      assert.ok(fs.existsSync(tmpOut));
      assert.ok(body.includes('## ⚡ Quick Run'));
      assert.ok(body.includes("## What's Changed"));
      assert.ok(body.includes('## 📦 Assets Guide'));
    } finally {
      if (fs.existsSync(tmpOut)) fs.unlinkSync(tmpOut);
    }
  });
});
