'use strict';

// Patches whose anchors broke silently with Zalo 26.11.20 (renamed minified
// locals, a new DevTools gate). Fixtures are copied from the real bundles.

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');

const devtools = require('../scripts/patches/patch-devtools');
const callSignal = require('../scripts/patches/patch-call-signal-fix');
const networkSync = require('../scripts/patches/patch-network-and-sync-fix');

const GATE_26_11_20 = '"Qd+r":function(e,t,n){"use strict";function r(){return!1}n.r(t),n.d(t,"canOpenDevTools",function(){return r})},';
// webpack output before 26.11.20 wrapped the getter in parentheses.
const GATE_PARENS = '"Qd+r":function(e,t,n){"use strict";function r(){return!1}n.r(t),n.d(t,"canOpenDevTools",(function(){return r}))},';

// Queue + controller from compact-app-pc (idle constant `w`) and from the
// login worker (idle constant `D`), as shipped in 26.11.20.
const queue = (idle, running, pause, timeout, fn, state, setter) =>
  `const ${idle}="idle",${running}="running",${pause}="pause",${timeout}=3e4;function ${fn}(e={}){const{limit:t=1/0,maxTimeout:n=${timeout}}=e,a=[];let ${state};const ${setter}=e=>{${state}=e};` +
  'return{}}class C{constructor(){' + `this.requestQueue=${fn}(),this.retryQueue=[],this.isRetryQueueFree=!0}}` +
  'function send(a){if(a)if(h.default.call.using_queue){return 1}return 0}var cfg={call:{using_queue:1}};';
const COMPACT = queue('w', 'M', 'D', 'L', 'P', 'i', 'r');
const LOGIN = queue('D', 'M', 'w', 'P', 'L', 's', 'i');

describe('Zalo 26.11.20 patches', () => {
  it('lets Dev Mode open DevTools', () => {
    for (const source of [GATE_26_11_20, GATE_PARENS]) {
      const { content, status } = devtools.patchContent(source);
      assert.equal(status, 'patched');
      assert.ok(content.includes('function r(){return!0}'));
      assert.equal(devtools.patchContent(content).status, 'already');
      // Also when another patch already flipped the gate with a space.
      assert.equal(devtools.patchContent(source.replace('return!1', 'return !0')).status, 'already');
    }
    assert.equal(devtools.patchContent('no gate here').status, 'absent');
  });

  it('fails when canOpenDevTools changes shape', async () => {
    assert.equal(devtools.patchContent('function r(){return x()}n.d(t,"canOpenDevTools",function(){return r})').status, 'missing');
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-devtools-'));
    fs.writeFileSync(path.join(dir, 'main.js'), 'n.d(t,"canOpenDevTools",function(){return q})');
    await assert.rejects(devtools.main(dir), /canOpenDevTools changed in: main\.js/);
  });


  it('starts both call signal queues idle and keeps calls off them', () => {
    for (const [source, idle, state] of [[COMPACT, 'w', 'i'], [LOGIN, 'D', 's']]) {
      const { content, missing } = callSignal.patchContent(source);
      assert.deepEqual(missing, []);
      assert.ok(content.includes(`let ${state}=${idle};`), `state starts at the "idle" constant ${idle}`);
      assert.ok(content.includes('.requestQueue.dequeue(),'));
      assert.ok(content.includes('if(a)if(!1&&h.default.call.using_queue)'));
      assert.ok(content.includes('using_queue:0'));
      assert.doesNotThrow(() => new vm.Script(content));
      assert.equal(callSignal.patchContent(content).content, content, 'idempotent');
    }
  });

  it('reports a call signal queue it cannot find', () => {
    const { missing } = callSignal.patchContent('this.requestQueue=new Q(),this.retryQueue=[]');
    assert.deepEqual(missing, ['queue initial state', 'queue dequeue', 'using_queue guard']);
  });

  it('forces call settings on whatever the minifier named the locals', () => {
    for (const [obj, err] of [['at', 'ht'], ['rt', 'ft']]) {
      const source = `try{let t=1==e.settings.chat.enable_call;${obj}.enableCall=t}catch(${err}){}` +
        `try{let t=1==e.settings.chat.enable_video_call;${obj}.enableVideoCall=t}catch(${err}){}`;
      const patched = source
        .replace(networkSync.CALL_SETTING('enable_call', 'enableCall'), '$1!0}')
        .replace(networkSync.CALL_SETTING('enable_video_call', 'enableVideoCall'), '$1!0}');
      assert.equal(patched, source.replace(`${obj}.enableCall=t`, `${obj}.enableCall=!0`).replace(`${obj}.enableVideoCall=t`, `${obj}.enableVideoCall=!0`));
    }
  });

  it('enables the SQLite backup flags in main.js and utility-process-sqlite.js', () => {
    for (const source of [
      'const a=!1,s="--backupdata",c="ABORT_BACKING_UP_FOR_ALL_SESSION",l="CREATE_BACKING_UP_FOR_NO_SESSION";const u=!1}',
      'const s=!1,a="--backupdata",c="ABORT_BACKING_UP_FOR_ALL_SESSION",l="CREATE_BACKING_UP_FOR_NO_SESSION";const u=!1}',
    ]) {
      const patched = source.replace(networkSync.BACKUP_FLAGS, '$1!0$2!0');
      assert.equal(patched, source.replace(/=!1/g, '=!0'));
    }
  });
});
