'use strict';

// 1-1 calls: Ping every server that answered InitZRTP while the phone rings
// (macOS CallController::_sendRequestZRTPPing), then only the kept one. Without
// it a call answered after a 13 s ring got nothing from the relay. Runs
// against two fake relays; ZCALL_PING_MS shortens the 6 s period.

process.env.ZCALL_PING_MS = '100';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const dgram = require('dgram');
const path = require('path');
const { ZCALL_DIR } = require('../scripts/utils/paths');

const ENG = path.join(ZCALL_DIR, 'native-engine');
const z = require(path.join(ENG, 'zrtp-packet'));
const { ZrtcMediaBackend } = require(path.join(ENG, 'backends', 'zrtc-media'));

const LOCAL = 111222333, PEER = 444555666, CALL = 777888;
const sessId = 'S'.repeat(20) + '0123456789abcdefghijklmnopqrstuvwxyz';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// One fake relay: answers InitZRTP with its token and every Ping with a
// MsgType 2 reply; remembers the pings (token, time) and the EndCall.
async function relay(t, token) {
  const sock = dgram.createSocket('udp4');
  t.after(() => sock.close());
  await new Promise((r) => sock.bind(0, '127.0.0.1', r));
  const r = { sock, token, pings: [], endCallAt: 0, addr: () => `127.0.0.1:${sock.address().port}` };
  sock.on('message', (m, rinfo) => {
    if (m[0] !== 1) return;
    const p = z.unwrapPacket(m);
    if (!p) return;
    if (p.cmd === 11) {
      const h = z.buildControlHeader({ msgType: 2, cmd: 11, subCmd: 2, uid: LOCAL });
      const addr = Buffer.from(`127.0.0.1|${rinfo.port}`, 'latin1');
      const body = Buffer.alloc(14);
      body.writeUInt32LE(CALL, 4); body.writeUInt32LE(token, 8); body.writeUInt16LE(addr.length, 12);
      sock.send(Buffer.concat([h, body, addr]), rinfo.port, rinfo.address);
    } else if (p.cmd === 2) {
      r.pings.push({ token: m.readUInt32LE(14), at: Date.now() });
      sock.send(z.buildControlHeader({ msgType: 2, cmd: 2, subCmd: 2, uid: LOCAL, token }), rinfo.port, rinfo.address);
    } else if (p.cmd === 3 && !r.endCallAt) r.endCallAt = Date.now();
  });
  return r;
}

function negotiate(media, a, b) {
  media.onNegotiated({ servers: [{ rtpaddr: a.addr() }, { rtpaddr: b.addr() }], config: {}, srtpMode: 1, sessId,
    callId: CALL, role: 'caller', localUid: LOCAL, peerUid: PEER });
}

// Ring, answer on server a, hang up.
async function ringThenAnswer(t, media, logs) {
  t.after(() => { try { media.stop(); } catch (_) {} }); // also when an assertion fails
  const a = await relay(t, 0x0a0a0a0a);
  const b = await relay(t, 0x0b0b0b0b);
  negotiate(media, a, b);
  const best = await media.prepare('caller');
  assert.ok(best, 'InitZRTP answered');
  await sleep(450);
  for (const r of [a, b]) {
    assert.ok(r.pings.length >= 2, `pings to ${r.addr()} while ringing (${r.pings.length})`);
    assert.ok(r.pings.every((p) => p.token === r.token), 'each server is pinged with its own token');
  }
  assert.match(logs.join('\n'), /media: ping before answer: 2 server\(s\) every 0\.1 s/);

  const startAt = Date.now();
  media.start('caller', a.addr());
  await sleep(60);
  assert.ok(a.pings.some((p) => p.at >= startAt && p.at - startAt < 60), 'the kept server is pinged at once on start');
  assert.ok(b.endCallAt >= startAt, 'EndCall to the server not chosen');
  const bPings = b.pings.length;
  const aPings = a.pings.length;
  await sleep(350);
  assert.strictEqual(b.pings.length, bPings, 'no more pings to the server not chosen');
  assert.ok(a.pings.length >= aPings + 2, 'the kept server is still pinged');
  assert.ok(a.pings.every((p) => p.token === a.token));
  const line = logs.find((l) => l.includes('media: ping before answer:') && l.includes(' sent, '));
  assert.ok(line, 'one log line on start with what ringing got');
  const m = line.match(/(\d+) sent, (\d+) replies \(MsgType 2\) in [\d.]+ s of ringing/);
  assert.ok(m && +m[1] >= 4 && +m[2] >= 4, line);

  media.stop();
  await sleep(150);
  const after = a.pings.length + b.pings.length;
  await sleep(350);
  assert.strictEqual(a.pings.length + b.pings.length, after, 'no pings after stop');
}

// Stop while still ringing (the caller gave up): the ring ping ends too.
async function stopWhileRinging(t, media, logs) {
  t.after(() => { try { media.stop(); } catch (_) {} }); // also when an assertion fails
  const a = await relay(t, 0x0a0a0a0a);
  const b = await relay(t, 0x0b0b0b0b);
  negotiate(media, a, b);
  assert.ok(await media.prepare('caller'));
  await sleep(250);
  assert.ok(a.pings.length >= 1 && b.pings.length >= 1);
  media.stop();
  await sleep(150);
  const after = a.pings.length + b.pings.length;
  await sleep(350);
  assert.strictEqual(a.pings.length + b.pings.length, after, 'no pings after stop');
  assert.ok(!logs.some((l) => / sent, \d+ replies/.test(l)), 'the summary line is for answered calls only');
}

describe('ring ping: zrtc-media.js', () => {
  const make = () => {
    const logs = [];
    return { logs, media: new ZrtcMediaBackend({ log: (...a) => logs.push(a.join(' ')), sendUdp: true, audio: false }) };
  };
  it('pings every server while ringing, then only the kept one', { timeout: 10000 }, async (t) => {
    const { media, logs } = make();
    await ringThenAnswer(t, media, logs);
  });
  it('stops pinging when the call ends before the answer', { timeout: 10000 }, async (t) => {
    const { media, logs } = make();
    await stopWhileRinging(t, media, logs);
  });
});
