'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const { patchContent, ORIGINAL_RULE, LOGIN_TITLEBAR_RULE } = require('../scripts/patches/patch-titlebar-linux-css');

describe('patch-titlebar-linux-css', () => {
  it('applies the login settings-button adjustment to the original Zalo CSS', () => {
    const result = patchContent(ORIGINAL_RULE);
    assert.equal(result.status, 'patched');
    assert.equal(result.content, ORIGINAL_RULE + LOGIN_TITLEBAR_RULE);
  });

  it('is idempotent for the final patched bundle', () => {
    const first = patchContent(ORIGINAL_RULE);
    const second = patchContent(first.content);
    assert.equal(second.status, 'already');
    assert.equal(second.content, first.content);
  });

  it('fails closed if the original Zalo CSS rule changes', () => {
    assert.throws(() => patchContent('.titlebar{left:70px}'), /expected original Zalo titlebar rule not found/);
  });
});
