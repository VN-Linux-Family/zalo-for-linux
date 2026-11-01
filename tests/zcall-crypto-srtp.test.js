import { describe, expect, it } from 'bun:test';
const path = require('path');
const srtp = require(path.join(__dirname, '..', 'zcall-native', 'native-engine', 'srtp-aes'));

describe('SRTP AES & KDF (RFC 3711)', () => {
  const masterKeyAndSalt = Buffer.alloc(30, 0x42);

  it('generates session keys with correct lengths', () => {
    const keys = srtp.sessionKeys(masterKeyAndSalt);
    expect(keys).toBeDefined();
    expect(Buffer.isBuffer(keys.enc)).toBe(true);
    expect(keys.enc.length).toBe(16); // 128-bit AES key
    expect(Buffer.isBuffer(keys.auth)).toBe(true);
    expect(keys.auth.length).toBe(20); // 160-bit HMAC-SHA1 key
    expect(Buffer.isBuffer(keys.salt)).toBe(true);
    expect(keys.salt.length).toBe(14); // 112-bit salt
  });

  it('rejects master key shorter than 30 bytes', () => {
    expect(() => srtp.sessionKeys(Buffer.alloc(29))).toThrow(TypeError);
  });

  it('derives master key from sessId string or buffer', () => {
    const sessIdStr = '012345678901234567890123456789extra';
    const master = srtp.masterFromSessId(sessIdStr);
    expect(master.length).toBe(30);
    expect(master.toString('utf8')).toBe(sessIdStr.slice(0, 30));

    expect(() => srtp.masterFromSessId('short')).toThrow(TypeError);
  });

  it('computes deterministic 16-byte packet IV', () => {
    const keys = srtp.sessionKeys(masterKeyAndSalt);
    const iv1 = srtp.packetIv(keys.salt, 0x12345678, 100, 0);
    const iv2 = srtp.packetIv(keys.salt, 0x12345678, 100, 0);
    const ivDifferentSeq = srtp.packetIv(keys.salt, 0x12345678, 101, 0);

    expect(iv1.length).toBe(16);
    expect(iv1.equals(iv2)).toBe(true);
    expect(iv1.equals(ivDifferentSeq)).toBe(false);
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
    expect(protectedPacket.length).toBe(rtpHeader.length + originalOpus.length + 10);

    const unprotected = srtp.unprotectRtp(protectedPacket, keys, roc);
    expect(unprotected).not.toBeNull();
    expect(unprotected.seq).toBe(seq);
    expect(unprotected.ssrc).toBe(ssrc);
    expect(unprotected.opus.equals(originalOpus)).toBe(true);
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
    expect(srtp.unprotectRtp(tamperedPayload, keys, 0)).toBeNull();

    // Tamper with auth tag byte
    const tamperedTag = Buffer.from(protectedPacket);
    tamperedTag[tamperedTag.length - 1] ^= 0x01;
    expect(srtp.unprotectRtp(tamperedTag, keys, 0)).toBeNull();
  });
});
