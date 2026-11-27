'use strict';

const test = require('node:test');
const assert = require('node:assert');
const fs = require('fs');
const path = require('path');
const vm = require('vm');

const callUi = fs.readFileSync(path.join(__dirname, '..', 'plugins/zcall/ui/call.js'), 'utf8');

// Just enough DOM for placeSelf: a tree, getElementById that only sees attached
// nodes (as in a browser), and textContent = '' detaching every child.
class Node {
  constructor(id) { this.id = id || ''; this.parentNode = null; this.children = []; }
  _detach() { if (this.parentNode) { const c = this.parentNode.children; c.splice(c.indexOf(this), 1); this.parentNode = null; } }
  insertBefore(node, ref) {
    node._detach();
    const i = ref ? this.children.indexOf(ref) : -1;
    this.children.splice(i < 0 ? this.children.length : i, 0, node);
    node.parentNode = this;
  }
  appendChild(node) { this.insertBefore(node, null); }
  prepend(node) { this.insertBefore(node, this.children[0] || null); }
  remove() { this._detach(); }
  set textContent(v) { for (const c of [...this.children]) c._detach(); }
  find(id) {
    if (this.id === id) return this;
    for (const c of this.children) { const f = c.find(id); if (f) return f; }
    return null;
  }
}

function load() {
  const start = callUi.indexOf('function placeSelf()');
  const end = callUi.indexOf('\n}\n', start) + 3;
  assert.ok(start > 0 && end > start, 'placeSelf not found in call.js');

  const body = new Node('body');
  const pip = new Node('pip'); const stage = new Node('stage'); const canvas = new Node('self');
  body.appendChild(pip); body.appendChild(stage); pip.appendChild(canvas);
  const selfTile = new Node(); const selfLabel = new Node(); selfTile.appendChild(selfLabel);
  const ctx = {
    $: (id) => body.find(id), state: {}, selfTile, selfLabel, selfCanvas: canvas,
    updateSelfTile() {}, layoutTiles() {},
  };
  vm.runInNewContext(callUi.slice(start, end) + '\nthis.placeSelf = placeSelf;', ctx);
  return { ctx, pip, stage, canvas, selfTile };
}

test('call.js keeps the preview canvas by reference', () => {
  assert.match(callUi, /^const selfCanvas = \$\('self'\);$/m);
});

test('placeSelf survives the end of a group call emptying #stage', () => {
  const { ctx, pip, stage, canvas, selfTile } = load();
  ctx.state = { group: true, phase: 'connected' };
  ctx.placeSelf();
  assert.strictEqual(canvas.parentNode, selfTile);
  assert.strictEqual(selfTile.parentNode, stage);

  // stopVideo() at the end of a group call: placeSelf, then the stage is emptied
  // with the preview inside; the body-class observer calls placeSelf again after.
  ctx.state = { group: true, phase: 'ended' };
  stage.textContent = '';
  assert.doesNotThrow(() => ctx.placeSelf());
  assert.strictEqual(canvas.parentNode, pip, 'the preview goes back to #pip');
  assert.strictEqual(selfTile.parentNode, null, 'the emptied stage stays empty');

  // The next group call in the same window gets its preview tile back.
  ctx.state = { group: true, phase: 'outgoing' };
  ctx.placeSelf();
  assert.strictEqual(canvas.parentNode, selfTile);
  assert.strictEqual(selfTile.parentNode, stage);
});

test('placeSelf keeps the 1-1 preview in #pip', () => {
  const { ctx, pip, canvas, selfTile } = load();
  ctx.state = { group: false, phase: 'connected' };
  ctx.placeSelf();
  assert.strictEqual(canvas.parentNode, pip);
  assert.strictEqual(selfTile.parentNode, null);
});
