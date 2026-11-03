'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const zrtp = require(path.join(__dirname, '..', 'zcall-native', 'native-engine', 'zrtp-packet'));

describe('ZRTP Packet Protocol', () => {
  it('wraps and unwraps media packets correctly', () => {
    const payload = Buffer.from('audio-payload-sample-bytes');
    const token = 0x12345678;
    const packet = zrtp.wrapMediaPacket({
      msgType: zrtp.MSG_TYPES.AUDIO_RTP,
      token,
      payload
    });

    assert.strictEqual(packet.length, zrtp.MEDIA_HEADER_LEN + payload.length);
    assert.strictEqual(packet[0], zrtp.MSG_TYPES.AUDIO_RTP);
    assert.strictEqual(packet.readUInt32LE(1), token);
    assert.ok(packet.subarray(5).equals(payload));

    const unwrapped = zrtp.unwrapPacket(packet);
    assert.strictEqual(unwrapped.isMedia, true);
    assert.strictEqual(unwrapped.msgType, zrtp.MSG_TYPES.AUDIO_RTP);
    assert.strictEqual(unwrapped.token, token);
    assert.ok(unwrapped.payload.equals(payload));
  });

  it('builds control packet headers with correct magic and structure', () => {
    const hdr = zrtp.buildControlHeader({
      msgType: zrtp.MSG_TYPES.CONTROL,
      seq: 42,
      uid: 10001,
      token: 9999,
      cmd: zrtp.COMMANDS.PING,
      subCmd: 0
    });

    assert.strictEqual(hdr.length, zrtp.CONTROL_HEADER_LEN);
    assert.strictEqual(hdr[0], zrtp.MSG_TYPES.CONTROL);
    assert.strictEqual(hdr[1], zrtp.MAGIC_BYTE);
    assert.strictEqual(hdr.readUInt32LE(6), 42);
    assert.strictEqual(hdr.readUInt32LE(10), 10001);
    assert.strictEqual(hdr.readUInt32LE(14), 9999);
    assert.strictEqual(hdr.readUInt16LE(18), zrtp.COMMANDS.PING);
    assert.strictEqual(hdr[20], 0);
  });

  it('builds Ping and EndCall packets', () => {
    const ping = zrtp.buildPingPacket({
      uid: 12345,
      token: 67890,
      sessId: 'session-id-123'
    });
    assert.ok(ping.length > zrtp.CONTROL_HEADER_LEN);
    assert.strictEqual(ping[1], 0x01); // Ping magic is 0x01 per Zalo protocol

    const endCall = zrtp.buildEndCallPacket({
      uid: 12345,
      token: 67890,
      callId: 999
    });
    assert.strictEqual(endCall.length, zrtp.CONTROL_HEADER_LEN + 4);
    assert.strictEqual(endCall.readUInt32LE(zrtp.CONTROL_HEADER_LEN), 999);
  });

  it('builds InitZRTP packet for caller and callee', () => {
    const callerPacket = zrtp.buildInitZrtpPacket({
      role: 'caller',
      uid: 1001,
      callId: 2002,
      peerUid: 3003,
      sessId: 'my-session-token'
    });

    assert.strictEqual(callerPacket[0], zrtp.MSG_TYPES.CONTROL);
    assert.strictEqual(callerPacket.readUInt16LE(18), zrtp.COMMANDS.INIT_CALL_CALLER);

    const calleePacket = zrtp.buildInitZrtpPacket({
      role: 'callee',
      uid: 3003,
      callId: 2002,
      peerUid: 1001,
      sessId: 'my-session-token'
    });

    assert.strictEqual(calleePacket.readUInt16LE(18), zrtp.COMMANDS.INIT_CALL_CALLEE);
  });
});
