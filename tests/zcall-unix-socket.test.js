'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const net = require('net');
const path = require('path');
const vm = require('vm');
const { spawn } = require('child_process');

const patchZcallNative = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-zcall-native'));
const { loadKey, encrypt, decrypt } = require(path.join(__dirname, '..', 'zcall-native', 'tools', 'lib', 'callv2-crypto'));

describe('ZCall Unix Socket IPC & Native Patch', () => {
  const cleanMainPath = path.join(__dirname, '..', 'temp', 'clean-26.11.10', 'main-dist', 'main.js');
  const mainDistPath = path.join(__dirname, '..', 'app', 'main-dist', 'main.js');

  it('validates that all patch steps match clean main.js', () => {
    if (!fs.existsSync(cleanMainPath)) return;
    const content = fs.readFileSync(cleanMainPath, 'utf8');
    const steps = patchZcallNative.getSteps(content);

    assert.ok(steps.length >= 8, `Expected >= 8 steps, got ${steps.length}`);
    for (const [what, from] of steps) {
      const matched = typeof from === 'string' ? content.includes(from) : from.test(content);
      assert.ok(matched, `Step failed to match: ${what}`);
    }
  });

  it('verifies patched main.js supports dynamic TMPDIR unix socket paths and valid syntax', () => {
    if (!fs.existsSync(cleanMainPath)) return;
    let content = fs.readFileSync(cleanMainPath, 'utf8');
    const steps = patchZcallNative.getSteps(content);
    for (const [, from, to] of steps) {
      content = typeof from === 'string' ? content.split(from).join(to) : content.replace(from, to);
    }

    assert.ok(content.includes('ZCALL_ENGINE_JS'));
    assert.ok(content.includes('process.env.TMPDIR'));
    assert.ok(content.includes('socketzalorecv2021'));
    assert.ok(content.includes('socketzalosend2021'));
    assert.ok(!content.includes('29631'));
    assert.ok(!content.includes('29632'));

    assert.doesNotThrow(() => new vm.Script(content));
  });

  it('establishes bidirectional communication over unix domain sockets with zcall-native', async () => {
    const mainJs = fs.existsSync(mainDistPath) ? mainDistPath : (fs.existsSync(cleanMainPath) ? cleanMainPath : null);
    if (!mainJs) return;

    process.env.ZALO_MAIN_JS = mainJs;
    const key = loadKey(mainJs);

    const id = Date.now();
    const recvPath = `/tmp/test-zcall-recv-${id}.sock`;
    const sendPath = `/tmp/test-zcall-send-${id}.sock`;

    let clientOnSend = null;
    let receivedReady = false;
    let engineProcessedFrame = false;

    const recvServer = net.createServer((sock) => {
      sock.on('data', (data) => {
        const parts = data.toString().split('$').filter(Boolean);
        for (const part of parts) {
          try {
            const msg = JSON.parse(decrypt(part, key));
            if (msg.type === 'update' && msg.command === 'native-ready') {
              receivedReady = true;
              if (clientOnSend) {
                const echoMsg = { type: 'request', command: 'pingTest', data: { ok: true } };
                clientOnSend.write(encrypt(echoMsg, key) + '$');
              }
            }
          } catch (_) {}
        }
      });
    });

    const sendServer = net.createServer((sock) => {
      clientOnSend = sock;
    });

    await new Promise((resolve) => recvServer.listen(recvPath, resolve));
    await new Promise((resolve) => sendServer.listen(sendPath, resolve));

    const engineJs = path.join(__dirname, '..', 'zcall-native', 'native-engine', 'zcall-native.js');
    const proc = spawn(process.execPath, [
      engineJs,
      recvPath,
      sendPath,
      '--no-window',
      '--media', 'stub',
      '--verbose'
    ], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: Object.assign({}, process.env, { ZALO_MAIN_JS: mainJs })
    });

    proc.stderr.on('data', (d) => {
      const text = d.toString();
      if (text.includes('<- request pingTest')) {
        engineProcessedFrame = true;
      }
    });

    await new Promise((resolve) => setTimeout(resolve, 1000));

    proc.kill('SIGTERM');
    recvServer.close();
    sendServer.close();
    try { fs.unlinkSync(recvPath); } catch (_) {}
    try { fs.unlinkSync(sendPath); } catch (_) {}

    assert.ok(receivedReady, 'Engine should send native-ready frame on recv socket');
    assert.ok(engineProcessedFrame, 'Engine should receive and parse request frame on send socket');
  });

  it('creates non-existent TMPDIR automatically when initialized', async () => {
    const parentDir = path.join('/tmp', `test-tmpdir-${Date.now()}`);
    const customTmp = path.join(parentDir, 'nested');
    assert.strictEqual(fs.existsSync(customTmp), false);

    const engineJs = path.join(__dirname, '..', 'zcall-native', 'native-engine', 'zcall-native.js');
    const proc = spawn(process.execPath, [
      engineJs,
      path.join(customTmp, 'socketzalorecv2021'),
      path.join(customTmp, 'socketzalosend2021'),
      '--no-window',
      '--media', 'stub'
    ], {
      stdio: 'ignore',
      env: Object.assign({}, process.env, { TMPDIR: customTmp })
    });

    await new Promise((resolve) => setTimeout(resolve, 300));
    proc.kill('SIGTERM');

    assert.strictEqual(fs.existsSync(customTmp), true);
    fs.rmSync(parentDir, { recursive: true, force: true });
  });
});
