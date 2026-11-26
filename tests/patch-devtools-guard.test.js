'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { patchDevtoolsGuardContent, isDevtoolsGuardApplied, patchFile } = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-devtools-guard'));

const MARKER = '/* zalo-linux-devtools-guard */';
const OPEN = 'canOpenDevTools';
const CLOSE = 'closeDevTools';
const WITH_OPEN = `const w = new i.BrowserWindow({webPreferences:{devTools:!0}});${OPEN}()=>{};`;

describe('patch-devtools-guard', () => {
  it('exposes the expected helpers', () => {
    assert.strictEqual(typeof patchDevtoolsGuardContent, 'function');
    assert.strictEqual(typeof isDevtoolsGuardApplied, 'function');
    assert.strictEqual(typeof patchFile, 'function');
  });

  it('forces hardcoded truthy devTools webPreferences back to disabled', () => {
    const content = `const w = new i.BrowserWindow({webPreferences:{devTools:!0}});${OPEN}();`;
    const patched = patchDevtoolsGuardContent(content);
    assert.ok(patched.includes('devTools:!1'), 'devTools must be disabled after the patch');
    assert.ok(!patched.includes('devTools:!0'), 'no hardcoded truthy devTools may remain');
    assert.ok(patched.includes(MARKER), 'patch must be marked idempotent');
  });

  it('is idempotent', () => {
    const content = `const w = new i.BrowserWindow({webPreferences:{devTools:!0}});${OPEN}();`;
    const first = patchDevtoolsGuardContent(content);
    assert.strictEqual(patchDevtoolsGuardContent(first), first);
  });

  it('treats already guarded content as applied', () => {
    const content = `${MARKER}\nconst w = new i.BrowserWindow({webPreferences:{devTools:!1}});${OPEN}();`;
    assert.strictEqual(isDevtoolsGuardApplied(content), true);
    assert.strictEqual(isDevtoolsGuardApplied(`const w = new i.BrowserWindow({webPreferences:{devTools:!0}});${OPEN}();`), false);
  });

  it('does not add a marker when no truthy devTools literal is present', () => {
    const content = `const w = new i.BrowserWindow({webPreferences:{devTools:!1}});${OPEN}();`;
    assert.strictEqual(patchDevtoolsGuardContent(content), content);
  });

  it('re-enables the devtools module gate so DevTools can start', () => {
    const moduleDef = '"Qd+r":function(e,t,n){"use strict";function r(){return!1}n.r(t),n.d(t,"canOpenDevTools",function(){return r})';
    const patched = patchDevtoolsGuardContent(moduleDef);
    assert.ok(patched.includes('function r(){return!0}'), 'devtools module gate must allow devtools');
    assert.ok(!patched.includes('function r(){return!1}'), 'devtools module gate must not block devtools');
  });

  it('fails closed when no devtools open/close guard is present', () => {
    assert.throws(
      () => patchDevtoolsGuardContent('const w = new i.BrowserWindow({webPreferences:{devTools:!0}});'),
      /no devtools open\/close guard found/
    );
  });

  it('patchFile is idempotent and returns false for already guarded files', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-devtools-'));
    const filePath = path.join(dir, 'main.js');
    fs.writeFileSync(filePath, `const w = new i.BrowserWindow({webPreferences:{devTools:!0}});${OPEN}();`);

    assert.strictEqual(patchFile(filePath), true);
    assert.strictEqual(patchFile(filePath), false);
    assert.ok(fs.readFileSync(filePath, 'utf8').includes('devTools:!1'));
    assert.ok(fs.readFileSync(filePath, 'utf8').includes(MARKER));
  });

  it('patchFile returns false when the file is already guarded', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-devtools-'));
    const filePath = path.join(dir, 'main.js');
    fs.writeFileSync(filePath, `${MARKER}\nconst w = new i.BrowserWindow({webPreferences:{devTools:!1}});${OPEN}();`);
    assert.strictEqual(patchFile(filePath), false);
  });
});