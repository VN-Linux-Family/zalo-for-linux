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

  it('categorizes commits into single-word headings', () => {
    const mockLines = [
      'Merge pull request #184 from ngxccc/feature/zlang-native-engine|abc1|Nguyen Hoang Ky|feat(zlang): implement native rust language detection engine',
      'Merge pull request #182 from ngxccc/feature/zocr-signature-scanner|abc2|Nguyen Hoang Ky|refactor(patches): migrate minified runtime patches to ast-grep AST engine',
      'Merge pull request #160 from ngxccc/feature/perf-patches-and-tests|abc3|Nguyen Hoang Ky|perf(patches): optimize platform patch scanning',
      'Merge pull request #163 from DMKha241/fix/offline-startup|abc4|Nguyen Hoang Ky|fix: update offline startup and OCR UI regex patterns',
      'Merge pull request #181 from ngxccc/chore/release-template|abc5|Nguyen Hoang Ky|ci: centralize release template',
      'Merge pull request #170 from dont-wait/home-manager|abc6|Nguyen Hoang Ky|support home-manager',
    ];

    const priorAuthors = new Set(['dmkha241']);
    const changelog = parseGitLog(mockLines, priorAuthors, 'VN-Linux-Family/zalo-for-linux', '26.11.10', '26.10.20');

    assert.ok(changelog.includes('### What\'s Changed'));
    assert.ok(changelog.includes('#### Features'));
    assert.ok(changelog.includes('#### Fixes'));
    assert.ok(changelog.includes('#### Performance'));
    assert.ok(changelog.includes('#### Refactor'));
    assert.ok(changelog.includes('#### CI'));
    assert.ok(changelog.includes('#### Maintenance'));
    assert.ok(changelog.includes('**Full Changelog**: https://github.com/VN-Linux-Family/zalo-for-linux/compare/26.10.20...26.11.10'));
  });

  it('lists all contributors and marks first-time contributors', () => {
    const mockLines = [
      'Merge pull request #184 from ngxccc/feature/zlang|abc1|Nguyen Hoang Ky|feat(zlang): test',
      'Merge pull request #163 from DMKha241/fix/offline|abc2|Nguyen Hoang Ky|fix: test',
      'Merge pull request #170 from dont-wait/home-manager|abc3|Nguyen Hoang Ky|support home-manager',
    ];

    // DMKha241 is an existing contributor; ngxccc and dont-wait are new
    const priorAuthors = new Set(['dmkha241']);
    const changelog = parseGitLog(mockLines, priorAuthors, 'VN-Linux-Family/zalo-for-linux', '26.11.10', '26.10.20');

    assert.ok(changelog.includes('#### Contributors'));
    // All 3 should be listed
    assert.ok(changelog.includes('* @DMKha241'));
    assert.ok(changelog.includes('* @ngxccc (first contribution)'));
    assert.ok(changelog.includes('* @dont-wait (first contribution)'));
    // DMKha241 should NOT have first contribution
    assert.ok(!changelog.includes('* @DMKha241 (first contribution)'));
  });

  it('generates full release body merging template and changelog safely in shallow clone', () => {
    const tmpOut = path.join(os.tmpdir(), `test-release-body-${Date.now()}.md`);
    try {
      const body = generateReleaseBody('26.11.10', tmpOut);
      assert.ok(fs.existsSync(tmpOut));
      assert.ok(body.includes('### ⚡ Quick Run'));
      assert.ok(body.includes('### What\'s Changed'));
      assert.ok(body.includes('### 📦 Assets Guide'));
    } finally {
      if (fs.existsSync(tmpOut)) fs.unlinkSync(tmpOut);
    }
  });
});
