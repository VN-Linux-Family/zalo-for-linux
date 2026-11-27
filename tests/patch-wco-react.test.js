'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const {
  patchMainContent,
  patchPreloadContent,
  patchReactContent,
  IPC_CHANNEL,
  MAIN_MARKER,
  REACT_MARKER,
  findThemeHookModuleId,
  findCompactTargets,
} = require('../scripts/patches/patch-wco-react');

const THEME_HOOK_MODULE = 'var modules={"theme-module-v2":function(e,t,n){const a=n("react");function getThemeSetting(){return {theme:"light"}}const events={SETTING_UPDATED:"app_theme_setting_updated"};const THEME={DARK:"dark"};const state=Object(a.useState)(()=>getThemeSetting());Object(a.useEffect)(()=>events.SETTING_UPDATED);const isDark=state.theme===THEME.DARK;return {isDark}}};';
const withThemeHook = source => `${THEME_HOOK_MODULE}${source}`;

describe('patch-wco-react', () => {
  it('adds the main-process WCO theme IPC listener once', () => {
    const original = 'const { ipcMain, BrowserWindow } = require("electron");';
    const first = patchMainContent(original);
    assert.equal(first.status, 'patched');
    assert.ok(first.content.includes(IPC_CHANNEL));
    assert.ok(first.content.includes('setTitleBarOverlay'));
    assert.equal(patchMainContent(first.content).status, 'already');
  });

  it('adds a WCO setter to Zalo preload IPC API map', () => {
    const original = 'const api={getAppVisibilityStatus:{apiKey:"visibility",type:h.a.REQUEST},other:{apiKey:"x",type:h.a.RAW_SEND}};';
    const result = patchPreloadContent(original);
    assert.equal(result.status, 'patched');
    assert.ok(result.content.includes(`setWcoTheme:{apiKey:"${IPC_CHANNEL}",type:h.a.RAW_SEND}`));
    assert.equal(patchPreloadContent(result.content).status, 'already');
  });

  it('keeps the observer at module scope for arrow-function titlebar components', () => {
    const original = 'function module(e,t,n){var ReactAlias=n(\"react\");const Titlebar=({platform:e})=>{return ReactAlias.a.createElement(\"div\",{id:\"titleBar\",className:\"login-title-bar\"},ReactAlias.a.createElement(\"div\",{className:\"title-name  +  macos\"}))}}';
    const result = patchReactContent(withThemeHook(original));
    assert.equal(result.status, 'patched');
    assert.ok(result.content.indexOf(`const ${REACT_MARKER}`) < result.content.indexOf('const Titlebar='));
    assert.ok(result.content.includes('ReactAlias.a.Fragment'));
    assert.doesNotThrow(() => new Function(result.content));
  });

  it('injects a React-owned theme observer into titlebar render using discovered React alias', () => {
    const original = 'function module(e,t,n){"use strict";var ReactAlias=n("react");class Titlebar{render(){return ReactAlias.a.createElement("div",{id:"titleBar",className:"titlebar"},ReactAlias.a.createElement("div",{className:"title-name  +  macos"},"title"))}}t.a=Titlebar}';
    const result = patchReactContent(withThemeHook(original));
    assert.equal(result.status, 'patched');
    assert.ok(result.content.includes(REACT_MARKER));
    assert.ok(result.content.includes('n(\"theme-module-v2\").a'));
    assert.ok(result.content.includes('isDark?\"#ffffff\":\"#000000\"'));
    assert.ok(result.content.includes('app_theme_setting_updated') || result.content.includes('xPHB'));
    assert.ok(!result.content.includes('unfocus'));
    assert.ok(result.content.includes('__zaloWcoUseTheme')); 
    assert.ok(result.content.includes('ReactAlias.a.useEffect'));
    assert.ok(result.content.indexOf('"use strict";') < result.content.indexOf('const __zaloWcoUseTheme'));
    assert.ok(result.content.includes('ReactAlias.a.Fragment'));
    assert.equal(patchReactContent(result.content).status, 'already');
    assert.doesNotThrow(() => new Function(result.content));
  });
  it('discovers compact-app-pc bundles by filename pattern rather than a build hash', () => {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'wco-targets-'));
    try {
      fs.writeFileSync(path.join(directory, 'compact-app-pc.new-build-hash.js'), '');
      fs.writeFileSync(path.join(directory, 'compact-app-pc.another.hash.js'), '');
      fs.writeFileSync(path.join(directory, 'default-login.js'), '');
      fs.mkdirSync(path.join(directory, 'nested'));
      fs.writeFileSync(path.join(directory, 'nested', 'compact-app-pc.nested.js'), '');
      assert.deepEqual(findCompactTargets(directory).map(file => path.basename(file)), [
        'compact-app-pc.another.hash.js',
        'compact-app-pc.new-build-hash.js',
      ]);
    } finally {
      fs.rmSync(directory, { recursive: true, force: true });
    }
  });

  it('does not mistake a neighboring React module for the theme hook', () => {
    const adjacentModules = THEME_HOOK_MODULE.replace(
      'var modules={',
      'var modules={"slider-module":function(e,t,n){const a=n("react");a.useState(0);a.useEffect(()=>{});},'
    );
    assert.equal(findThemeHookModuleId(adjacentModules), 'theme-module-v2');
  });

  it('discovers the theme hook by behavior instead of a fixed webpack module ID', () => {
    assert.equal(findThemeHookModuleId(THEME_HOOK_MODULE), 'theme-module-v2');
    assert.equal(findThemeHookModuleId(`${THEME_HOOK_MODULE}${THEME_HOOK_MODULE.replace('theme-module-v2', 'another-theme-module')}`), 'theme-module-v2');
  });

  it('refuses legacy bridge input instead of migrating it', () => {
    const legacy = `${THEME_HOOK_MODULE}/*wco-theme-zalo-hook-v4*/const __zaloWcoThemeBridge=()=>null;`;
    assert.throws(() => patchReactContent(legacy), /legacy or unknown bridge found; expected pristine Zalo bundle/);
  });

});
