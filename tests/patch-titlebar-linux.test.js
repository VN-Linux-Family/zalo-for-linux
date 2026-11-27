'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const { patchContent, FRAME_VALUE, TITLEBAR_VALUE, OVERLAY_VALUE } = require('../scripts/patches/patch-titlebar-linux');

const LOGIN = 'const opts={width:420,minHeight:MIN_LOGIN_HEIGHT,frame:!global.__zaloNativeWayland,titleBarStyle:"hidden",resizable:!0};';
const POPUP = 'const popup={width:352,frame:!1,transparent:!0};';
const PHOTO = 'const photo={title:"Zalo Photo",frame:!1,titleBarStyle:"hidden",webPreferences:{partition:"persist:zalo"}})),o.maximize&&y.maximize();setPhotoViewerWindow:function(e){y=e;';
const THEME_UPDATE = 'updateBgColorByAppConfig:function(e){var t;if(!Ae)return;const n=e||me.get(\"app_theme\"),r=(null==n||null===(t=n.color)||void 0===t?void 0:t.backgroundPrimary)||null;r&&Ae.setBackgroundColor(r)}';

const MAIN_WINDOW = 'function createMainWindow(){const r=u.get(\"app_theme\"),opts={titleBarStyle:\"hidden\",backgroundColor:r.color};x=new a(opts);return x}';

function original(...parts) { return [...parts, PHOTO, THEME_UPDATE, MAIN_WINDOW].join(''); }

describe('patch-titlebar-linux', () => {
  it('keeps Zalo frameless and adds WCO to original hidden-titlebar windows', () => {
    const result = patchContent(original(POPUP, LOGIN));
    assert.equal(result.status, 'patched');
    assert.ok(result.content.includes(`frame:${FRAME_VALUE}`));
    assert.ok(result.content.includes(`titleBarStyle:${TITLEBAR_VALUE},titleBarOverlay:${OVERLAY_VALUE}`));
    assert.ok(result.content.includes('symbolColor:r&&r.theme===\"dark\"?\"#ffffff\":\"#000000\"'));
    assert.ok(result.content.startsWith(POPUP));
  });

  it('enables WCO on every original hidden-titlebar Zalo window', () => {
    const windows = [
      'const login={minHeight:MIN_LOGIN_HEIGHT,frame:!1,titleBarStyle:"hidden"};',
      'const child={frame:!1,titleBarStyle:"hidden",show:!1};',
      'const photo={title:"Zalo Photo",frame:!1,titleBarStyle:"hidden",show:!1}})),o.maximize&&y.maximize();',
      'const media={frame:!1,titleBarStyle:"hidden",show:!1};'
    ].join('');
    const { content } = patchContent(windows + 'setPhotoViewerWindow:function(e){y=e;' + THEME_UPDATE + MAIN_WINDOW);
    assert.equal((content.match(/titleBarOverlay:/g) || []).length, 5);
    assert.equal((content.match(/titleBarStyle:"hidden"/g) || []).length, 5);
  });

  it('preserves Zalo original app-theme background update without adding overlay mutation', () => {
    const source = original(LOGIN) + 'Ke.setMainWindow(Ae);';
    const { content } = patchContent(source);
    assert.ok(content.includes('rgba(0,0,0,0)'));
    assert.ok(content.includes(THEME_UPDATE));
    assert.ok(content.includes('symbolColor:r&&r.theme===\"dark\"?\"#ffffff\":\"#000000\"'));
  });

  it('tags the Photo Viewer through its original construction and registration paths', () => {
    const { content } = patchContent(original(LOGIN));
    assert.ok(content.includes('symbolColor:"#ffffff"'));
    assert.ok(content.includes('y.__zaloPhotoViewer=!0'));
    assert.ok(content.includes('setPhotoViewerWindow:function(e){y=e,e&&(e.__zaloPhotoViewer=!0);'));
  });

  it('rejects patched input instead of repairing or accepting it', () => {
    const once = patchContent(original(LOGIN)).content;
    assert.throws(() => patchContent(once), /expected original Zalo window options without titleBarOverlay/);
  });

  it('fails closed when the original login window options are absent', () => {
    assert.throws(() => patchContent(POPUP + PHOTO), /original login window options not found/);
  });
});
