'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');
const vm = require('node:vm');
const { spawn } = require('node:child_process');
const crypto = require('node:crypto');

const root = path.join(__dirname, '..');
const patchZcallNative = require(path.join(root, 'scripts', 'patches', 'patch-zcall-native'));
const binaryPath = path.join(root, 'target', 'release', 'zcall');

function loadTransportKey(mainJs) {
  const source = fs.readFileSync(mainJs, 'utf8');
  const match = source.match(/callV2TransportAesKey:\"([^\"]+)\"/);
  assert.ok(match, 'Zalo main bundle must contain the call-v2 transport key');
  const key = Buffer.from(match[1], 'base64');
  assert.equal(key.length, 16, 'call-v2 transport key must be 128 bits');
  return key;
}

function encryptFrame(value, key) {
  const cipher = crypto.createCipheriv('aes-128-cbc', key, Buffer.alloc(16));
  return Buffer.concat([cipher.update(JSON.stringify(value), 'utf8'), cipher.final()]).toString('hex');
}

function decryptFrame(value, key) {
  const decipher = crypto.createDecipheriv('aes-128-cbc', key, Buffer.alloc(16));
  return Buffer.concat([decipher.update(Buffer.from(value, 'hex')), decipher.final()]).toString('utf8');
}

describe('Rust zcall helper and existing Zalo transports', () => {
  const cleanMainPath = path.join(root, 'temp', 'clean-26.11.10', 'main-dist', 'main.js');

  it('matches all native-helper patch steps against the clean Zalo bundle', () => {
    if (!fs.existsSync(cleanMainPath)) return;
    const content = fs.readFileSync(cleanMainPath, 'utf8');
    const steps = patchZcallNative.getSteps(content);
    assert.ok(steps.length >= 8);
    assert.ok(steps.some(([name]) => name === 'engine path on Linux (close)'));
    assert.ok(!steps.some(([name]) => name === 'spawn the engine'));
    for (const [name, from] of steps) {
      assert.ok(typeof from === 'string' ? content.includes(from) : (from.lastIndex = 0, from.test(content)),
        'patch step did not match: ' + name);
    }
  });

  it('replaces only the Linux helper executable and keeps Zalo as process owner', () => {
    if (!fs.existsSync(cleanMainPath)) return;
    let content = fs.readFileSync(cleanMainPath, 'utf8');
    for (const [, from, to] of patchZcallNative.getSteps(content)) {
      content = typeof from === 'string' ? content.split(from).join(to) : content.replace(from, to);
    }
    assert.ok(content.includes('o.join(__dirname,"..","native","zcall","zcall")'));
    assert.ok(!content.includes('ZCALL_ENGINE_JS'));
    assert.ok(!content.includes('ELECTRON_RUN_AS_NODE'));
    assert.ok(!content.includes('zcall-engine-start'));
    assert.ok(content.includes('socketzalorecv2021'));
    assert.ok(content.includes('socketzalosend2021'));
    assert.doesNotThrow(() => new vm.Script(content));
  });

  it('decodes Rust local-preview frames with the existing binary TCP framing', () => {
    const window = require(path.join(root, 'plugins', 'zcall', 'window.js'));
    const token = 'frontend-test-token';
    const payload = Buffer.from([0, 0, 1, 0x65, 0xaa]);
    const header = Buffer.from(JSON.stringify({ type: 'localVideo', key: true, codec: 'h264', bin: payload.length }) + '\n');
    const state = { buf: Buffer.alloc(0), pending: null, authed: false };
    const result = window.readEngineMessages(state, Buffer.concat([Buffer.from(token + '\n'), header, payload]), token);
    assert.equal(result.fatal, false);
    assert.equal(result.authed, true);
    assert.equal(result.messages.length, 1);
    assert.equal(result.messages[0].type, 'localVideo');
    assert.deepEqual(result.messages[0].data, payload);
  });

  it('keeps the installed Zalo helper spawn free of the temporary main-process bridge', () => {
    const installedMain = path.join(root, 'app', 'main-dist', 'main.js');
    if (!fs.existsSync(installedMain)) return;
    const content = fs.readFileSync(installedMain, 'utf8');
    assert.ok(content.includes('o.join(__dirname,"..","native","zcall","zcall")'));
    assert.ok(!content.includes('zcall-engine-start'));
    assert.ok(content.includes('client: stdout data'), 'Zalo keeps its original helper stdout diagnostics');
  });

  it('keeps the frontend transport inside the Rust ui module without a host layer', () => {
    const source = path.join(root, 'native', 'engines', 'zcall', 'src');
    const ui = fs.readFileSync(path.join(source, 'ui', 'mod.rs'), 'utf8');
    const tcp = fs.readFileSync(path.join(source, 'ui', 'tcp.rs'), 'utf8');
    const main = fs.readFileSync(path.join(source, 'main.rs'), 'utf8');
    assert.ok(ui.includes('pub use tcp::TcpUiTransport'));
    assert.ok(tcp.includes('impl UiTransport for TcpUiTransport'));
    assert.ok(main.includes('use zcall::ui::TcpUiTransport'));
    assert.ok(main.includes('fn load_callv2_key'));
    assert.ok(!fs.existsSync(path.join(source, 'host')));
  });

  it('keeps the existing plugin TCP UI protocol and does not add a main-process bridge', () => {
    const plugin = fs.readFileSync(path.join(root, 'plugins', 'zcall', 'index.js'), 'utf8');
    const window = fs.readFileSync(path.join(root, 'plugins', 'zcall', 'window.js'), 'utf8');
    const main = fs.readFileSync(path.join(root, 'main.js'), 'utf8');
    const patch = fs.readFileSync(path.join(root, 'scripts', 'patches', 'patch-zcall-native.js'), 'utf8');
    const renderer = fs.readFileSync(path.join(root, 'plugins', 'zcall', 'ui', 'call.js'), 'utf8');
    assert.match(plugin, /callWindow\.start\(\)/);
    assert.match(plugin, /callWindow\.stop\(\)/);
    assert.doesNotMatch(plugin, /child_process|spawnSync|engineProcesses|ZCALL_ENGINE_JS/);
    assert.match(window, /net\.createServer\(onConnection\)/);
    assert.match(window, /ZCALL_UI_PORT/);
    assert.match(window, /ZCALL_UI_TOKEN/);
    assert.match(window, /readEngineMessages/);
    assert.match(window, /m\.type === 'localVideo'/);
    assert.match(window, /m\.type === 'video' \|\| m\.type === 'localVideo'/);
    assert.doesNotMatch(main, /zcall-engine-start|zcall-engine-action|zcall-engine-message/);
    assert.doesNotMatch(patch, /process\.emit\(\"zcall-engine-start\",/);
    assert.doesNotMatch(patch, /filter helper UI messages from stdout diagnostics|ELECTRON_RUN_AS_NODE/);
    assert.doesNotMatch(renderer, /getUserMedia|VideoEncoder|MediaStreamTrackProcessor|desktopCapturer/);
  });

  it('exchanges encrypted call-v2 frames and the existing authenticated TCP UI protocol', async (t) => {
    if (!fs.existsSync(binaryPath)) {
      t.skip('Rust zcall release binary has not been built');
      return;
    }
    const mainJs = fs.existsSync(cleanMainPath)
      ? cleanMainPath
      : path.join(root, 'app', 'main-dist', 'main.js');
    if (!fs.existsSync(mainJs)) {
      t.skip('Zalo main bundle with the call-v2 transport key is unavailable');
      return;
    }

    const key = loadTransportKey(mainJs);
    const socketId = process.pid + '-' + Date.now();
    const recvPath = path.join('/tmp', 'zcall-rust-recv-' + socketId + '.sock');
    const sendPath = path.join('/tmp', 'zcall-rust-send-' + socketId + '.sock');
    const uiToken = 'zcall-ui-test-' + socketId;

    let clientOnSend = null;
    let stderr = '';
    let uiBuffer = '';
    let uiAuthenticated = false;
    let receivedReady = false;
    let engineProcessedFrame = false;
    let uiDevicesResult = null;
    let readyResolve;
    let processedResolve;
    let uiDevicesResolve;
    const ready = new Promise((resolve) => { readyResolve = resolve; });
    const processed = new Promise((resolve) => { processedResolve = resolve; });
    const uiDevices = new Promise((resolve) => { uiDevicesResolve = resolve; });

    const recvServer = net.createServer((sock) => {
      sock.on('data', (data) => {
        for (const part of data.toString().split('$').filter(Boolean)) {
          try {
            const message = JSON.parse(decryptFrame(part, key));
            if (message.type === 'update' && message.command === 'native-ready') {
              receivedReady = true;
              readyResolve();
              if (clientOnSend) {
                clientOnSend.write(encryptFrame({ type: 'request', command: 'rustIpcProbe', data: { ok: true } }, key) + '$');
              }
            }
          } catch (_) {}
        }
      });
    });

    const sendServer = net.createServer((sock) => {
      clientOnSend = sock;
      if (receivedReady) {
        sock.write(encryptFrame({ type: 'request', command: 'rustIpcProbe', data: { ok: true } }, key) + '$');
      }
    });

    const uiServer = net.createServer((sock) => {
      sock.setEncoding('utf8');
      sock.on('data', (chunk) => {
        uiBuffer += chunk;
        for (;;) {
          const newline = uiBuffer.indexOf('\n');
          if (newline < 0) break;
          const line = uiBuffer.slice(0, newline).trim();
          uiBuffer = uiBuffer.slice(newline + 1);
          if (!uiAuthenticated) {
            assert.equal(line, uiToken, 'Rust helper must authenticate with the frontend token');
            uiAuthenticated = true;
            sock.write(JSON.stringify({ action: 'listDevices', requestId: 'tcp-device-probe' }) + '\n');
            continue;
          }
          if (!line) continue;
          try {
            const message = JSON.parse(line);
            if (message.type === 'devices' && message.requestId === 'tcp-device-probe') {
              uiDevicesResult = message;
              uiDevicesResolve(message);
            }
          } catch (_) {}
        }
      });
    });

    let proc;
    try {
      await Promise.all([
        new Promise((resolve) => recvServer.listen(recvPath, resolve)),
        new Promise((resolve) => sendServer.listen(sendPath, resolve)),
        new Promise((resolve) => uiServer.listen(0, '127.0.0.1', resolve))
      ]);

      proc = spawn(binaryPath, [recvPath, sendPath, '--verbose'], {
        stdio: ['ignore', 'ignore', 'pipe'],
        env: {
          ...process.env,
          ZALO_MAIN_JS: mainJs,
          ZCALL_UI_PORT: String(uiServer.address().port),
          ZCALL_UI_TOKEN: uiToken,
          ZCALL_SEND_UDP: '0',
          ZCALL_GROUP: '0'
        }
      });
      proc.stderr.setEncoding('utf8');
      proc.stderr.on('data', (data) => {
        stderr += data;
        if (stderr.includes('unhandled request rustIpcProbe')) {
          engineProcessedFrame = true;
          processedResolve();
        }
      });

      await Promise.race([
        Promise.all([ready, processed, uiDevices]),
        new Promise((_, reject) => setTimeout(() => reject(new Error('Rust helper transport round-trip timed out. stderr=' + stderr)), 5000))
      ]);
      assert.equal(receivedReady, true, 'Rust engine must publish native-ready over the existing encrypted socket');
      assert.equal(engineProcessedFrame, true, 'Rust engine must parse an encrypted request from Zalo');
      assert.equal(uiAuthenticated, true, 'Rust helper must authenticate to the existing plugin TCP listener');
      assert.ok(uiDevicesResult && uiDevicesResult.data, 'Rust UI response must return over the existing TCP socket');
      assert.equal(uiDevicesResult.requestId, 'tcp-device-probe');
    } finally {
      if (proc && proc.exitCode === null) {
        proc.kill('SIGTERM');
        await new Promise((resolve) => proc.once('exit', resolve));
      }
      await Promise.all([
        new Promise((resolve) => recvServer.close(resolve)),
        new Promise((resolve) => sendServer.close(resolve)),
        new Promise((resolve) => uiServer.close(resolve))
      ]);
      try { fs.unlinkSync(recvPath); } catch (_) {}
      try { fs.unlinkSync(sendPath); } catch (_) {}
    }
  });
});
