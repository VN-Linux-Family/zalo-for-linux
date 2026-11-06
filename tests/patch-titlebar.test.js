'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');

const { patchContent } = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-titlebar'));

const FLAG = 'frame:!global.__zaloNativeWayland';

// Shapes copied from the minified main-dist/main.js of Zalo 26.10.x / 26.11.x.
const LOGIN_WINDOW =
  'i={width:me.DEFAULT_LOGIN_WIDTH,height:me.DEFAULT_LOGIN_HEIGHT,minWidth:me.MIN_LOGIN_WIDTH,' +
  'minHeight:me.MIN_LOGIN_HEIGHT,frame:!1,titleBarStyle:"hidden",resizable:!0,backgroundColor:"#1f1f1f"};';
// Added in Zalo 26.11 and placed before the login window in main.js.
const APP_USAGE_WINDOW =
  'const w=new u({width:m.width,height:m.height,title:"Zalo App Usage",show:!1,resizable:!0,' +
  'autoHideMenuBar:!0,frame:!1,backgroundColor:"#1e1e1e"});';
const POPUP_WINDOW = 'const S={width:352,height:82,frame:!1,transparent:!0,skipTaskbar:!0};';

function loginFrame(content) {
  return content.match(/MIN_LOGIN_HEIGHT,(frame:[^,]+)/)[1];
}

describe('patch-titlebar', () => {
  it('gives the login/main window a native frame', () => {
    const { content, status } = patchContent(POPUP_WINDOW.replace('frame:!1,', '') + LOGIN_WINDOW);
    assert.equal(status, 'patched');
    assert.equal(loginFrame(content), FLAG);
  });

  it('targets the main window when the App Usage window precedes it (#188)', () => {
    const { content, status } = patchContent(APP_USAGE_WINDOW + LOGIN_WINDOW);
    assert.equal(status, 'patched');
    assert.equal(loginFrame(content), FLAG);
    assert.match(content, /title:"Zalo App Usage"[^}]*frame:!1/);
  });

  it('leaves frameless popups alone', () => {
    const { content } = patchContent(POPUP_WINDOW + LOGIN_WINDOW);
    assert.ok(content.startsWith(POPUP_WINDOW));
  });

  it('is idempotent', () => {
    const once = patchContent(APP_USAGE_WINDOW + LOGIN_WINDOW).content;
    const twice = patchContent(once);
    assert.equal(twice.status, 'already');
    assert.equal(twice.content, once);
  });

  it('reports missing when the main window is not found', () => {
    const { status } = patchContent(APP_USAGE_WINDOW);
    assert.equal(status, 'missing');
  });
});
