'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { buildGroupAudioRtpHeader, buildGroupVideoRtpHeader } = require('../native/engines/zcall/native-engine/rtp');

// One-byte extension elements (RFC 8285) of an RTP header: [{ id, len }].
function extIds(h) {
  assert.strictEqual(h.readUInt16BE(12), 0xbede);
  const end = 16 + 4 * h.readUInt16BE(14);
  const out = [];
  for (let o = 16; o < end;) {
    if (h[o] === 0) { o++; continue; }
    const id = h[o] >> 4, len = (h[o] & 0x0f) + 1;
    out.push({ id, len });
    o += 1 + len;
  }
  return out;
}

test('group audio: audio level and capture time, no transport-cc (as the phone sends it)', () => {
  const h = buildGroupAudioRtpHeader({ seq: 0x3bcd, ts: 1, ssrc: 0x07642455, level: 80 });
  assert.strictEqual(h.length, 28);
  assert.strictEqual(h.readUInt16BE(14), 3);
  assert.deepStrictEqual(extIds(h), [{ id: 2, len: 1 }, { id: 13, len: 7 }]);
  assert.strictEqual(h[17], 80);
});

test('group video keeps transport-cc', () => {
  const h = buildGroupVideoRtpHeader({ seq: 1, ts: 1, ssrc: 1, twSeq: 7, pt: 97, sfu: { id: 12, long: false } });
  assert.ok(extIds(h).some((e) => e.id === 5));
});

test('group audio does not take transport-wide sequence numbers; 1-1 audio does', () => {
  const { ZrtcMediaBackend } = require('../native/engines/zcall/native-engine/backends/zrtc-media');
  for (const group of [true, false]) {
    const b = new ZrtcMediaBackend({});
    const sent = [];
    b._send = (buf) => sent.push(buf);
    b.sendRate = { onSent: (tw) => sent.push(tw) };
    Object.assign(b, { active: true, activeServer: { host: '127.0.0.1', port: 1 }, plainMedia: true, group, params: { localUid: 1 }, serverToken: 1 });
    const before = b.twSeq;
    b._sendAudioFrame(Buffer.from([0xf8, 0xff, 0xfe]));
    assert.strictEqual(b.twSeq, group ? before : (before + 1) & 0xffff);
    assert.strictEqual(sent.filter((x) => typeof x === 'number').length, group ? 0 : 1);
  }
});
