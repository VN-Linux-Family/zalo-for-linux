'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');

const { install, createOpenPath } = require('../plugins/shell-open');

function fakeChild() {
  const handlers = new Map();
  return {
    handlers,
    unrefCalled: false,
    once(event, handler) {
      handlers.set(event, handler);
      return this;
    },
    unref() {
      this.unrefCalled = true;
    },
    emit(event, value) {
      const handler = handlers.get(event);
      assert.ok(handler, `no handler registered for '${event}'`);
      handler(value);
    },
  };
}

function installFake(shell, calls = []) {
  const child = fakeChild();
  install(shell, {
    spawn: (...args) => {
      calls.push(args);
      return child;
    },
  });
  return child;
}

test('plugins/shell-open: detaches xdg-open and settles on spawn, not on exit', async () => {
  const calls = [];
  const child = fakeChild();
  const shell = {
    openPath: () => {
      throw new Error('the original blocking implementation must not run');
    },
  };

  assert.equal(install(shell, { spawn: (...args) => { calls.push(args); return child; } }), true);

  const started = Date.now();
  const pending = shell.openPath('/home/u/Downloads/form.docx');
  child.emit('spawn');
  const result = await pending;
  const elapsed = Date.now() - started;

  assert.equal(result, '');
  assert.ok(elapsed < 100, `openPath took ${elapsed}ms`);
  assert.deepEqual(calls, [[
    'xdg-open',
    ['/home/u/Downloads/form.docx'],
    { detached: true, stdio: 'ignore' },
  ]]);
  assert.equal(child.unrefCalled, true);
});

test('plugins/shell-open: a spawn failure resolves with the message instead of rejecting', async () => {
  const shell = { openPath: () => Promise.resolve('') };
  install(shell, {
    spawn: () => {
      throw new Error('spawn xdg-open EACCES');
    },
  });

  assert.equal(await shell.openPath('/tmp/missing.docx'), 'spawn xdg-open EACCES');
});

test('plugins/shell-open: an async spawn error (missing xdg-open) is reported', async () => {
  const shell = { openPath: () => Promise.resolve('') };
  const child = installFake(shell);
  const pending = shell.openPath('/tmp/doc.txt');
  child.emit('error', new Error('spawn xdg-open ENOENT'));
  assert.equal(await pending, 'spawn xdg-open ENOENT');
});

test('plugins/shell-open: the target is passed as one argv entry, never through a shell', async () => {
  const calls = [];
  const shell = { openPath: () => Promise.resolve('') };
  const child = installFake(shell, calls);

  const nasty = '/tmp/a b; touch /tmp/pwned $(id).docx';
  const pending = shell.openPath(nasty);
  child.emit('spawn');
  await pending;

  assert.deepEqual(calls[0][1], [nasty]);
});

test('plugins/shell-open: installing twice does not wrap again', () => {
  const shell = { openPath: () => Promise.resolve('') };
  assert.equal(install(shell, { spawn: () => fakeChild() }), true);
  const installed = shell.openPath;
  assert.equal(install(shell, { spawn: () => fakeChild() }), false);
  assert.equal(shell.openPath, installed);
});

test('plugins/shell-open: nothing is installed without an openPath to replace', () => {
  assert.equal(install({}, { spawn: () => fakeChild() }), false);
  assert.equal(install(null, { spawn: () => fakeChild() }), false);
});

test('plugins/shell-open: createOpenPath can be built directly', async () => {
  const child = fakeChild();
  const openPath = createOpenPath(() => child);
  const pending = openPath('/tmp/x');
  child.emit('spawn');
  assert.equal(await pending, '');
  assert.equal(child.unrefCalled, true);
});
