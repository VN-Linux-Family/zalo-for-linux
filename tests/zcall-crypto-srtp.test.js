'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const srtp = require(path.join(__dirname, '..', 'zcall-native', 'native-engine', 'srtp-aes'));

describe('SRTP AES & KDF (RFC 3711)', () => {
  const masterKeyAndSalt = Buffer.alloc(30, 0x42);

  it('generates session keys with correct lengths', () => {
    const keys = srtp.sessionKeys(masterKeyAndSalt);
    assert.ok(keys !== undefined);
    assert.ok(Buffer.isBuffer(keys.enc));
    assert.strictEqual(keys.enc.length, 16); // 128-bit AES key
    assert.ok(Buffer.isBuffer(keys.auth));
    assert.strictEqual(keys.auth.length, 20); // 160-bit HMAC-SHA1 key
    assert.ok(Buffer.isBuffer(keys.salt));
    assert.strictEqual(keys.salt.length, 14); // 112-bit salt
  });

  it('rejects master key shorter than 30 bytes', () => {
    assert.throws(() => srtp.sessionKeys(Buffer.alloc(29)), TypeError);
  });

  it('derives master key from sessId string or buffer', () => {
    const sessIdStr = '012345678901234567890123456789extra';
    const master = srtp.masterFromSessId(sessIdStr);
    assert.strictEqual(master.length, 30);
    assert.strictEqual(master.toString('utf8'), sessIdStr.slice(0, 30));

    assert.throws(() => srtp.masterFromSessId('short'), TypeError);
  });

  it('computes deterministic 16-byte packet IV', () => {
    const keys = srtp.sessionKeys(masterKeyAndSalt);
    const iv1 = srtp.packetIv(keys.salt, 0x12345678, 100, 0);
    const iv2 = srtp.packetIv(keys.salt, 0x12345678, 100, 0);
    const ivDifferentSeq = srtp.packetIv(keys.salt, 0x12345678, 101, 0);

    assert.strictEqual(iv1.length, 16);
    assert.ok(iv1.equals(iv2));
    assert.ok(!iv1.equals(ivDifferentSeq));
  });

  it('encrypts (protect) and decrypts (unprotect) RTP payload', () => {
    const keys = srtp.sessionKeys(masterKeyAndSalt);
    const ssrc = 0xdeadbeef;
    const seq = 1234;
    const roc = 0;

    // Fake 12-byte RTP header
    const rtpHeader = Buffer.alloc(12);
    rtpHeader[0] = 0x80; // V=2, P=0, X=0, CC=0
    rtpHeader[1] = 112;  // PT=112 (Opus)
    rtpHeader.writeUInt16BE(seq, 2);
    rtpHeader.writeUInt32BE(960000, 4); // timestamp
    rtpHeader.writeUInt32BE(ssrc, 8);

    const originalOpus = Buffer.from('Testing Opus Audio Frame Payload 1234567890!@#$%^&*()');

    const protectedPacket = srtp.protectRtp(rtpHeader, originalOpus, keys, seq, ssrc, roc);
    assert.strictEqual(protectedPacket.length, rtpHeader.length + originalOpus.length + 10);

    const unprotected = srtp.unprotectRtp(protectedPacket, keys, roc);
    assert.ok(unprotected !== null);
    assert.strictEqual(unprotected.seq, seq);
    assert.strictEqual(unprotected.ssrc, ssrc);
    assert.ok(unprotected.opus.equals(originalOpus));
  });

  it('rejects tampered ciphertext or modified HMAC tag', () => {
    const keys = srtp.sessionKeys(masterKeyAndSalt);
    const ssrc = 0x11223344;
    const seq = 55;
    const rtpHeader = Buffer.alloc(12);
    rtpHeader[0] = 0x80;
    rtpHeader[1] = 112;
    rtpHeader.writeUInt16BE(seq, 2);
    rtpHeader.writeUInt32BE(1000, 4);
    rtpHeader.writeUInt32BE(ssrc, 8);

    const opus = Buffer.from('Authentic audio data');
    const protectedPacket = srtp.protectRtp(rtpHeader, opus, keys, seq, ssrc, 0);

    // Tamper with payload byte
    const tamperedPayload = Buffer.from(protectedPacket);
    tamperedPayload[15] ^= 0xff;
    assert.strictEqual(srtp.unprotectRtp(tamperedPayload, keys, 0), null);

    // Tamper with auth tag byte
    const tamperedTag = Buffer.from(protectedPacket);
    tamperedTag[tamperedTag.length - 1] ^= 0x01;
    assert.strictEqual(srtp.unprotectRtp(tamperedTag, keys, 0), null);
  });
});
