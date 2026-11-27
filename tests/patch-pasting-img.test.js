'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');

const patchPastingImg = require('../scripts/patches/patch-pasting-img');

describe('patch-pasting-img (Electron 43 clipboard API)', () => {
  it('prepends valid JS before preload-render bundle marker and is idempotent', () => {
    const original = `${patchPastingImg.BUNDLE_MARKER};console.log("ok");`;
    const first = patchPastingImg.patchContent(original);
    assert.equal(first.status, 'patched');
    assert.ok(first.content.includes(patchPastingImg.PATCH_START));
    assert.ok(first.content.includes(patchPastingImg.PATCH_END));
    assert.ok(first.content.endsWith(original));

    // Must not depend on old Electron 22 workarounds
    assert.ok(!first.content.includes('wl-paste'));
    assert.ok(!first.content.includes('xclip'));
    assert.ok(!first.content.includes('dragOverlayInputbox'));
    assert.ok(!first.content.includes('DragEvent'));
    assert.ok(!first.content.includes('zalo_clip_'));

    // Valid JS syntax
    assert.doesNotThrow(() => new vm.Script(first.content));

    // Idempotent on re-run
    const second = patchPastingImg.patchContent(first.content);
    assert.equal(second.status, 'already');
    assert.equal(second.content, first.content);
  });

  it('upgrades an older Electron 22 patch in place', () => {
    const legacy = `${patchPastingImg.PATCH_START}\n// legacy wl-paste + DragEvent\n${patchPastingImg.BUNDLE_MARKER};`;
    const upgraded = patchPastingImg.patchContent(legacy);
    assert.equal(upgraded.status, 'updated');
    assert.ok(!upgraded.content.includes('legacy wl-paste'));
    assert.ok(upgraded.content.includes('ClipboardEvent'));
  });

  it('patches startGetClipboard and startRewriteClipboard in main.js and is idempotent', async () => {
    const sampleMain =
      'const obj={startGetClipboard:function(e){return new Promise((t,n)=>{ne&&ie!==b?(me[e+"-get-clipboard"]={resolve:t,reject:n},ne.getClipboard(e)):n(new Error("clipboard native not ready"))})},' +
      'restartCap:function(e){return e},' +
      'startRewriteClipboard:function(e){return new Promise((t,n)=>{ne&&ie!==b?(me[e+"-rewrite-clipboard"]={resolve:t,reject:n},ne.rewriteClipboardBitmap(e)):n(new Error("clipboard native not ready"))})}};';

    const first = patchPastingImg.patchMainContent(sampleMain);
    assert.equal(first.status, 'patched');
    assert.ok(first.content.includes(patchPastingImg.MAIN_PATCH_MARKER));
    assert.doesNotThrow(() => new vm.Script(first.content));

    const second = patchPastingImg.patchMainContent(first.content);
    assert.equal(second.status, 'already');
    assert.equal(second.content, first.content);

    // Execute patched startGetClipboard & startRewriteClipboard against a mock electron.clipboard
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-main-clip-'));
    const imgPath = path.join(tmpDir, 'pic.png');
    fs.writeFileSync(imgPath, Buffer.from([1, 2, 3]));
    try {
      let uriData = '';
      const sandbox = {
        require: (id) => {
          if (id === 'electron') {
            return {
              clipboard: {
                readBuffer: (fmt) => (fmt === 'text/uri-list' ? Buffer.from(uriData) : Buffer.alloc(0)),
                has: () => false,
              },
            };
          }
          return require(id);
        },
        Promise,
      };
      const ctx = vm.createContext(sandbox);
      vm.runInContext(`${first.content};this._obj=obj;`, ctx);

      // Bitmap image (no URI list) -> content: null, requestId matches
      const bitmapRes = await ctx._obj.startGetClipboard('user-123');
      assert.deepEqual(JSON.parse(JSON.stringify(bitmapRes)), { requestId: 'user-123', content: null });

      // File URI list -> content has local path
      uriData = `file://${encodeURI(imgPath)}\r\n`;
      const fileRes = await ctx._obj.startGetClipboard('user-456');
      assert.deepEqual(JSON.parse(JSON.stringify(fileRes)), { requestId: 'user-456', content: imgPath });

      // Rewrite clipboard -> resolves with requestId
      const rwRes = await ctx._obj.startRewriteClipboard('user-789');
      assert.deepEqual(JSON.parse(JSON.stringify(rwRes)), { requestId: 'user-789' });
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it('throws when bundle marker or main.js anchors are missing', async () => {
    assert.throws(
      () => patchPastingImg.patchContent('console.log("changed");'),
      /bundle marker/
    );
    assert.throws(
      () => patchPastingImg.patchMainContent('const x = 1;'),
      /startGetClipboard/
    );

    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-paste-test-'));
    try {
      await assert.rejects(
        () => patchPastingImg.main(tmpDir),
        /main\.js: not found/
      );
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });

  it('reads text/uri-list image files and dispatches synthetic ClipboardEvent', () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-paste-rt-'));
    const imgPath = path.join(tmpDir, 'photo with spaces.png');
    const pngBytes = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    fs.writeFileSync(imgPath, pngBytes);

    try {
      const listeners = {};
      const ipcListeners = {};
      const dispatched = [];

      class FakeFile {
        constructor(parts, name, opts = {}) {
          this.parts = parts;
          this.name = name;
          this.type = opts.type || '';
        }
      }
      class FakeDataTransfer {
        constructor() {
          const list = [];
          this.items = {
            add: (file) => list.push({ kind: 'file', type: file.type, getAsFile: () => file }),
            get length() { return list.length; },
          };
          Object.defineProperty(this, '_list', { value: list });
        }
      }
      class FakeClipboardEvent {
        constructor(type, init = {}) {
          this.type = type;
          this.clipboardData = init.clipboardData || null;
          this.bubbles = Boolean(init.bubbles);
          this.cancelable = Boolean(init.cancelable);
          this.defaultPrevented = false;
          this.propagationStopped = false;
        }
        preventDefault() { this.defaultPrevented = true; }
        stopImmediatePropagation() { this.propagationStopped = true; }
      }

      const fakeClipboard = {
        readBuffer: (fmt) => (fmt === 'text/uri-list' ? Buffer.from(`file://${encodeURI(imgPath)}\r\n`) : Buffer.alloc(0)),
        has: (fmt) => fmt === 'text/uri-list',
        availableFormats: () => ['text/uri-list', 'text/plain'],
        readImage: () => ({ isEmpty: () => true }),
      };

      const activeEl = {
        dispatchEvent: (ev) => dispatched.push(ev),
      };

      const sandbox = {
        require: (id) => {
          if (id === 'electron') {
            return {
              clipboard: fakeClipboard,
              ipcRenderer: { on: (ch, fn) => { ipcListeners[ch] = fn; } },
            };
          }
          return require(id);
        },
        window: {
          addEventListener: (ev, fn) => { listeners[ev] = fn; },
        },
        document: {
          activeElement: activeEl,
          body: activeEl,
          addEventListener: (ev, fn) => { listeners[`doc:${ev}`] = fn; },
        },
        File: FakeFile,
        DataTransfer: FakeDataTransfer,
        ClipboardEvent: FakeClipboardEvent,
        WeakSet,
        Date,
      };

      vm.runInNewContext(patchPastingImg.buildPasteHandler(), sandbox);
      listeners.DOMContentLoaded();

      // 1. Normal text paste is untouched
      fakeClipboard.has = () => false;
      const textPaste = new FakeClipboardEvent('paste', {
        clipboardData: {
          types: ['text/plain'],
          items: [{ kind: 'string', getAsFile: () => null }],
          getData: (t) => (t === 'text/plain' ? 'hello' : ''),
        },
      });
      listeners['doc:paste'](textPaste);
      assert.equal(textPaste.defaultPrevented, false);
      assert.equal(dispatched.length, 0);

      // 2. Native file paste is untouched (no duplicate paste)
      const nativeImgPaste = new FakeClipboardEvent('paste', {
        clipboardData: {
          types: ['image/png'],
          items: [{ kind: 'file', getAsFile: () => new FakeFile([pngBytes], 'image.png', { type: 'image/png' }) }],
          getData: () => '',
        },
      });
      listeners['doc:paste'](nativeImgPaste);
      assert.equal(nativeImgPaste.defaultPrevented, false);
      assert.equal(dispatched.length, 0);

      // 3. File manager text/uri-list paste (even when Chromium only exposes text/plain in data.types)
      fakeClipboard.has = (fmt) => fmt === 'text/uri-list';
      const uriPaste = new FakeClipboardEvent('paste', {
        clipboardData: {
          types: ['text/plain'],
          items: [{ kind: 'string', getAsFile: () => null }],
          getData: (t) => (t === 'text/plain' ? imgPath : ''),
        },
      });
      listeners['doc:paste'](uriPaste);
      assert.equal(uriPaste.defaultPrevented, true);
      assert.equal(uriPaste.propagationStopped, true);
      assert.equal(dispatched.length, 1);
      assert.equal(dispatched[0].clipboardData.items.length, 1);
      assert.equal(dispatched[0].clipboardData._list[0].getAsFile().name, 'photo with spaces.png');
      assert.equal(dispatched[0].clipboardData._list[0].getAsFile().type, 'image/png');

      // 4. Synthetic event itself is ignored by capture listener (no infinite loop)
      listeners['doc:paste'](dispatched[0]);
      assert.equal(dispatched.length, 1);

      // 5. Auto-paste screenshot IPC dispatches synthetic ClipboardEvent
      fakeClipboard.readBuffer = (fmt) => (fmt === 'image/png' ? pngBytes : Buffer.alloc(0));
      fakeClipboard.availableFormats = () => ['image/png'];
      fakeClipboard.has = () => false;
      ipcListeners['zalo-linux-auto-paste-screenshot']();
      assert.equal(dispatched.length, 2);
      assert.equal(dispatched[1].clipboardData._list[0].getAsFile().type, 'image/png');
    } finally {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    }
  });
});
