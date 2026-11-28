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
  findThemeHookModuleId,
  findCompactTargets,
} = require('../scripts/patches/patch-wco-react');

const THEME_HOOK_MODULE = 'var modules={"theme-module-v2":function(e,t,n){const a=n("react");function getThemeSetting(){return {theme:"dark"}}const events={SETTING_UPDATED:"app_theme_setting_updated"};const THEME={DARK:"dark"};const useTheme=()=>{const[state,setState]=Object(a.useState)(()=>getThemeSetting());Object(a.useEffect)(()=>events.SETTING_UPDATED);const{theme:o}=state||{};return{themeSetting:state,theme:o,themeSettingOpt:1,token:null,isDark:o===THEME.DARK}};return useTheme()}};'
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

  it('syncs WCO from Zalo theme state inside the theme hook before titlebar render', () => {
    const result = patchReactContent(THEME_HOOK_MODULE);
    assert.equal(result.status, 'patched');
    assert.ok(result.content.includes('window.$zapp.setWcoTheme(o===THEME.DARK?\"#ffffff\":\"#000000\")'));
    assert.ok(result.content.indexOf('useWcoThemeSync') === -1);
    assert.ok(result.content.includes('__zaloWcoThemeHookSync'));
    assert.ok(result.content.indexOf('useEffect)(()=>{if(window.$zapp') < result.content.indexOf('return{themeSetting'));
    assert.doesNotThrow(() => new Function(result.content));
    assert.equal(patchReactContent(result.content).status, 'already');
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

});
