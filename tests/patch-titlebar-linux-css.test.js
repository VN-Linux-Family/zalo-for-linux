'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const { patchContent, patchLayoutContent, ORIGINAL_RULE, NAV_RULE, BANNER_RULE, RULE } = require('../scripts/patches/patch-titlebar-linux-css');

describe('patch-titlebar-linux-css', () => {
  it('transforms the original Zalo titlebar rule and keeps its visual styling', () => {
    const result = patchContent(ORIGINAL_RULE);
    assert.equal(result.status, 'patched');
    assert.equal(result.content, ORIGINAL_RULE + RULE);
    assert.ok(RULE.includes('.titlebar{left:0!important;width:100%!important;'));
    assert.ok(!RULE.includes('.nav-content'));
    assert.ok(RULE.includes('.login-title-bar__setting-button:has(> div[icon="icon-outline-menu"].z--btn--v2.btn-tertiary-neutral.large.--rounded.icon-only){left:0!important;right:auto!important}'));
  });

  it('applies the navigation offset independently when a stylesheet has no titlebar rule', () => {
    const source = '#main-tab{width:70px;position:relative;margin-top:-32px}';
    const result = patchLayoutContent(source);
    assert.equal(result.content, source + NAV_RULE);
    assert.ok(result.content.includes('#sidebarNav:before{content:"";display:block;flex:0 0 70px;width:70px;min-width:70px}'), 'reserve nav width in flex flow');
    assert.ok(result.content.includes('#main-tab:not(.WEB){position:fixed!important;left:0!important;top:32px!important;bottom:0!important;'), 'anchor nav independently of banner flow');
    assert.ok(result.content.includes('#main-tab{width:70px;'), 'keep original sidebar width');
    assert.doesNotMatch(result.content, /margin-top:-64px/);
    assert.doesNotMatch(result.content, /\.nav__tabs__header\{margin-top:/);
    assert.equal(result.changed, true);
  });

  it('removes the macOS-reserved top gap from both nav header variants', () => {
    const source = '.nav__tabs__header{display:flex;flex-direction:column;gap:6px;margin-top:32px}.nav__tabs.WEB .nav__tabs__header{margin-top:4px}';
    const result = patchLayoutContent(source);
    assert.equal(result.content, '.nav__tabs__header{display:flex;flex-direction:column;gap:6px}');
    assert.equal(result.changed, true);
  });

  it('fails closed if the original Zalo CSS rule changes', () => {
    assert.throws(() => patchContent('.titlebar{left:70px}'), /expected original Zalo titlebar rule not found/);
  });
});
