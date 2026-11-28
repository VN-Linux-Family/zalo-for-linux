'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const { patchContent, patchStylesheet, MARKER, HOVER_MARKER, LOCK_TITLE_MARKER } = require('../scripts/patches/patch-app-lock-titlebar');

const ORIGINAL = 'class Titlebar{_showAppLock(){}render(){return r.a.createElement("div",{id:"titleBar"},null,this.state.pendingUpdate?r.a.createElement("div",null):null)}}';

describe('patch-app-lock-titlebar', () => {
  it('restores the Windows app-lock button in the titlebar before the update control', () => {
    const result = patchContent(ORIGINAL);
    assert.equal(result.status, 'patched');
    assert.ok(result.content.includes('fa-Lock_24_Line'));
    assert.ok(result.content.includes('onClick:this._showAppLock.bind(this)'));
    assert.ok(result.content.includes('this.props.isPopupWindow||!this.props.status||this.props.status.isAppLock?null'));
    assert.ok(result.content.indexOf(MARKER) < result.content.indexOf('this.state.pendingUpdate'));
    assert.doesNotThrow(() => new Function(result.content));
  });

  it('discovers the React alias instead of depending on a minified variable name', () => {
    const source = ORIGINAL.replaceAll('r.a', 'ReactAlias.a');
    const result = patchContent(source);
    assert.ok(result.content.includes('ReactAlias.a.createElement(\"i\",{className:\"btn titlebar__menu__btn fa fa-Lock_24_Line\"'));
    assert.doesNotThrow(() => new Function(result.content));
  });

  it('upgrades an already-injected lock to the native window-controls safe area', () => {
    const first = patchContent(ORIGINAL);
    const oldStyle = 'style:{position:\"fixed\",left:\"calc(env(titlebar-area-width) - 46px)\",top:\"0\",width:\"46px\",minWidth:\"46px\",flex:\"0 0 46px\",height:\"32px\",maxHeight:\"32px\",display:\"flex\",alignItems:\"center\",justifyContent:\"center\",zIndex:3},';
    const old = first.content.replace(oldStyle, '');
    const upgraded = patchContent(old);
    assert.equal(upgraded.status, 'upgraded');
    assert.ok(upgraded.content.includes('left:\"calc(env(titlebar-area-width) - 46px)\"'));
    assert.ok(upgraded.content.includes('position:\"fixed\"'));
    assert.doesNotThrow(() => new Function(upgraded.content));
  });

  it('is idempotent', () => {
    const first = patchContent(ORIGINAL);
    const second = patchContent(first.content);
    assert.equal(second.status, 'already');
    assert.equal(second.content, first.content);
  });

  it('adds the Windows titlebar hover and pressed feedback once', () => {
    const css = '.titlebar__btns>.btn:hover{background:var(--button-secondary-neutral-hover)}';
    const first = patchStylesheet(css);
    assert.equal(first.status, 'patched');
    assert.ok(first.content.includes(HOVER_MARKER));
    assert.ok(first.content.includes(':hover{background:var(--button-secondary-neutral-hover)}'));
    assert.ok(first.content.includes(':active{background:var(--button-secondary-neutral-hover);transition:none}'));
    assert.ok(first.content.includes(LOCK_TITLE_MARKER));
    assert.ok(first.content.includes('#titleBar.locked .title-name{padding-left:0!important}'));
    assert.equal(patchStylesheet(first.content).status, 'already');
  });

  it('fails closed when the titlebar insertion point is ambiguous or missing', () => {
    assert.throws(() => patchContent('class Titlebar{_showAppLock(){}render(){return "titleBar"}}'), /insertion point/);
    assert.throws(() => patchContent('class Titlebar{render(){return "titleBar"}}'), /render method/);
  });
});
