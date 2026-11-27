'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { sessionKeys, protectRtp, unprotectRtp } = require('../native/engines/zcall/native-engine/srtp-aes');

const keys = sessionKeys(Buffer.from('0123456789abcdefghijklmnopqrst'));
const hdr = (seq) => Buffer.from(`90f0${seq.toString(16).padStart(4, '0')}6520edc207642455bede00032050d6b35d374de7429a0000`, 'hex');

test('noSeq: AES-CM with seq 0, no tag; the header keeps the real seq', () => {
  const opus = Buffer.from('opus frame');
  const pkt = protectRtp(hdr(0x3bcd), opus, keys, 0x3bcd, 0x07642455, 5, true);
  assert.strictEqual(pkt.readUInt16BE(2), 0x3bcd);
  assert.strictEqual(pkt.length, hdr(0).length + opus.length, 'null auth: no tag');
  // Same keystream for every seq: only the SSRC and the salt enter the IV.
  const other = protectRtp(hdr(0x0001), opus, keys, 0x0001, 0x07642455, 0, true);
  assert.deepStrictEqual(pkt.subarray(4), other.subarray(4));
  const got = unprotectRtp(pkt, keys, 0, true);
  assert.strictEqual(got.seq, 0x3bcd);
  assert.strictEqual(got.opus.toString(), 'opus frame');
  assert.strictEqual(pkt.readUInt16BE(2), 0x3bcd, 'input untouched');
});
