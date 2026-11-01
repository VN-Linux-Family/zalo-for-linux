import { describe, expect, it } from 'bun:test';
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

    expect(packet.length).toBe(zrtp.MEDIA_HEADER_LEN + payload.length);
    expect(packet[0]).toBe(zrtp.MSG_TYPES.AUDIO_RTP);
    expect(packet.readUInt32LE(1)).toBe(token);
    expect(packet.subarray(5).equals(payload)).toBe(true);

    const unwrapped = zrtp.unwrapPacket(packet);
    expect(unwrapped.isMedia).toBe(true);
    expect(unwrapped.msgType).toBe(zrtp.MSG_TYPES.AUDIO_RTP);
    expect(unwrapped.token).toBe(token);
    expect(unwrapped.payload.equals(payload)).toBe(true);
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

    expect(hdr.length).toBe(zrtp.CONTROL_HEADER_LEN);
    expect(hdr[0]).toBe(zrtp.MSG_TYPES.CONTROL);
    expect(hdr[1]).toBe(zrtp.MAGIC_BYTE);
    expect(hdr.readUInt32LE(6)).toBe(42);
    expect(hdr.readUInt32LE(10)).toBe(10001);
    expect(hdr.readUInt32LE(14)).toBe(9999);
    expect(hdr.readUInt16LE(18)).toBe(zrtp.COMMANDS.PING);
    expect(hdr[20]).toBe(0);
  });

  it('builds Ping and EndCall packets', () => {
    const ping = zrtp.buildPingPacket({
      uid: 12345,
      token: 67890,
      sessId: 'session-id-123'
    });
    expect(ping.length).toBeGreaterThan(zrtp.CONTROL_HEADER_LEN);
    expect(ping[1]).toBe(0x01); // Ping magic is 0x01 per Zalo protocol

    const endCall = zrtp.buildEndCallPacket({
      uid: 12345,
      token: 67890,
      callId: 999
    });
    expect(endCall.length).toBe(zrtp.CONTROL_HEADER_LEN + 4);
    expect(endCall.readUInt32LE(zrtp.CONTROL_HEADER_LEN)).toBe(999);
  });

  it('builds InitZRTP packet for caller and callee', () => {
    const callerPacket = zrtp.buildInitZrtpPacket({
      role: 'caller',
      uid: 1001,
      callId: 2002,
      peerUid: 3003,
      sessId: 'my-session-token'
    });

    expect(callerPacket[0]).toBe(zrtp.MSG_TYPES.CONTROL);
    expect(callerPacket.readUInt16LE(18)).toBe(zrtp.COMMANDS.INIT_CALL_CALLER);

    const calleePacket = zrtp.buildInitZrtpPacket({
      role: 'callee',
      uid: 3003,
      callId: 2002,
      peerUid: 1001,
      sessId: 'my-session-token'
    });

    expect(calleePacket.readUInt16LE(18)).toBe(zrtp.COMMANDS.INIT_CALL_CALLEE);
  });
});
