const test = require('node:test');
const assert = require('node:assert/strict');
const { spawn } = require('node:child_process');
const path = require('node:path');
const fs = require('node:fs');

test('Native zcall-audio engine full IPC protocol and state transitions', async (t) => {
  const binaryPath = path.join(__dirname, '..', 'target', 'release', 'zcall-audio');
  if (!fs.existsSync(binaryPath)) {
    t.skip('zcall-audio release binary not built yet');
    return;
  }

  const proc = spawn(binaryPath, ['--tone'], {
    stdio: ['pipe', 'pipe', 'pipe'],
  });

  let stderrLogs = '';
  proc.stderr.on('data', (d) => {
    stderrLogs += d.toString('utf8');
  });

  const frames = [];
  let buffer = Buffer.alloc(0);

  const readNextFrame = () => {
    return new Promise((resolve) => {
      const check = () => {
        while (buffer.length >= 4) {
          if (buffer[0] === 0x4c) {
            const level = buffer[1];
            const len = buffer.readUInt16BE(2);
            if (buffer.length < 4 + len) break;
            const payload = buffer.subarray(4, 4 + len);
            buffer = buffer.subarray(4 + len);
            return resolve({ level, len, payload });
          }
          buffer = buffer.subarray(1);
        }
        proc.stdout.once('data', (chunk) => {
          buffer = Buffer.concat([buffer, chunk]);
          check();
        });
      };
      check();
    });
  };

  // 1. Verify initial audio frames with test tone (should have speech/tone loudness level < 127)
  const initialFrame = await readNextFrame();
  assert.equal(typeof initialFrame.level, 'number', 'Audio level must be a number');
  assert.ok(initialFrame.level < 100, `Tone frame level (${initialFrame.level} dBov) must reflect audible signal (<100)`);
  assert.ok(initialFrame.len > 0, 'Opus payload must not be empty');
  // Verify that zcall-audio announced initial mic volume, auto mode, and probed hardware at startup
  assert.match(stderrLogs, /\[audio-io\] mic volume: \d+%/, 'Engine must announce initial mic volume at startup');
  assert.match(stderrLogs, /\[audio-io\] mic mode: (auto|manual)/, 'Engine must announce initial mic mode at startup');
  assert.match(stderrLogs, /\[audio-io\] hw: audio backend=PulseAudio\/PipeWire/, 'Engine must probe audio backend');


  // 2. Test Volume adjustment 'V' (percent: 75) -> transitions to manual mode
  proc.stdin.write(Buffer.from([0x56, 75]));
  await new Promise((r) => setTimeout(r, 50));
  assert.ok(stderrLogs.includes('mic volume: 75%'), 'Stderr must acknowledge volume change to 75%');
  assert.ok(stderrLogs.includes('mic mode: manual'), 'Manual volume change must switch mode to manual');

  // 2b. Test Auto Gain toggle 'A' (on: 1) -> restores auto mode
  proc.stdin.write(Buffer.from([0x41, 1]));
  await new Promise((r) => setTimeout(r, 50));
  assert.match(stderrLogs.slice(stderrLogs.lastIndexOf('mic mode: manual')), /mic mode: auto/, 'CMD_IN_AUTO_GAIN=1 must restore auto mode');

  // 3. Test Mute command 'M' (mute: 1)
  proc.stdin.write(Buffer.from([0x4d, 1]));
  // Wait a couple frames to let mute take effect
  await readNextFrame();
  const mutedFrame = await readNextFrame();
  assert.equal(mutedFrame.level, 127, 'Muted audio frame must have RFC 6464 level 127 (silence)');

  // 4. Test Unmute command 'M' (mute: 0)
  proc.stdin.write(Buffer.from([0x4d, 0]));
  await readNextFrame();
  const unmutedFrame = await readNextFrame();
  assert.ok(unmutedFrame.level < 100, `Unmuted audio frame level (${unmutedFrame.level}) must return to audible`);

  // 5. Test Inbound frame playback 'F' using the captured valid Opus packet stream (prime buffer)
  for (let seq = 1; seq <= 4; seq++) {
    const inboundHdr = Buffer.alloc(11);
    inboundHdr[0] = 0x46; // 'F'
    inboundHdr.writeUInt32BE(seq, 1);
    inboundHdr.writeUInt32BE(0x1234, 5); // ssrc
    inboundHdr.writeUInt16BE(unmutedFrame.payload.length, 9);
    proc.stdin.write(Buffer.concat([inboundHdr, unmutedFrame.payload]));
  }
  await new Promise((r) => setTimeout(r, 80));
  assert.match(stderrLogs, /\[audio-io\] info: jitter buffer primed/, 'JitterBuffer must prime after buffering target');

  // 6. Test Speaker off command 'S' (off: 1)
  proc.stdin.write(Buffer.from([0x53, 1]));

  // 7. Test Device switch command 'D' (mic device)
  const devName = Buffer.from('default_test_mic', 'utf8');
  const devHdr = Buffer.from([0x44, 0x69, devName.length >> 8, devName.length & 0xff]);
  proc.stdin.write(Buffer.concat([devHdr, devName]));
  await new Promise((r) => setTimeout(r, 50));
  assert.ok(stderrLogs.includes('default_test_mic'), 'Stderr must acknowledge device switch');

  // 8. Clean shutdown on stdin EOF
  proc.stdin.end();

  const exitCode = await new Promise((resolve) => {
    proc.on('exit', (code) => resolve(code));
  });

  assert.equal(exitCode, 0, 'zcall-audio process must exit cleanly with code 0 on EOF');
});

test('ZrtcMediaBackend network handover, P2P fallback to relay, and SRTP ROC recovery', () => {
  const { ZrtcMediaBackend } = require('../native/engines/zcall/native-engine/backends/zrtc-media');
  const {
    COMMANDS,
    P2P,
    buildChangeAddressPacket,
    buildP2pSignal,
    unwrapPacket,
  } = require('../native/engines/zcall/native-engine/zrtp-packet');
  const { buildAudioRtpHeader } = require('../native/engines/zcall/native-engine/rtp');
  const { protectRtp } = require('../native/engines/zcall/native-engine/srtp-aes');

  // 1. Verify ChangeAddress (cmd 14) and P2P Signal (kind 5) wire packets
  const changeAddrPkt = buildChangeAddressPacket({ uid: 123456, token: 0xdeadbeef, seq: 1, sessId: 'sess-xyz' });
  const parsedChange = unwrapPacket(changeAddrPkt);
  assert.equal(parsedChange.cmd, COMMANDS.CHANGE_ADDRESS);
  assert.equal(parsedChange.subCmd, 1);
  assert.equal(parsedChange.uid, 123456);
  assert.equal(parsedChange.token, 0xdeadbeef);

  const p2pStopPkt = buildP2pSignal({ role: 'caller', callId: 999, subCmd: 2, seq: 0 });
  assert.equal(p2pStopPkt.length, 14);
  assert.equal(p2pStopPkt[7], P2P.SIGNAL);
  assert.equal(p2pStopPkt[9], 2);

  // 2. Verify ZrtcMediaBackend SRTP sequence reset recovery & P2P-to-relay fallback
  const receivedFrames = [];
  const sentPackets = [];
  const media = new ZrtcMediaBackend({
    sendUdp: false,
    audio: false,
    onAudioPacket: (opus) => receivedFrames.push(opus),
  });

  media.onNegotiated({
    sessId: '0123456789abcdef0123456789abcdef',
    callId: 42,
    role: 'caller',
    localUid: 1001,
    peerUid: 2002,
    servers: [{ rtpaddr: '1.2.3.4:4200', rtcpaddr: '1.2.3.4:4200' }],
    config: {},
  });
  media.active = true;
  media.startedAt = Date.now() - 5000;
  media.activeServer = { host: '1.2.3.4', port: 4200 };
  media.serverToken = 0x11223344;
  media._send = (pkt, dst, what) => sentPackets.push({ pkt, dst, what });

  const makeSrtpAudio = (seq, roc = 0) => {
    const hdr = buildAudioRtpHeader({ seq, ts: seq * 320, ssrc: 2002, twSeq: seq, level: 50 });
    return protectRtp(hdr, Buffer.from([0xf8, 0xff, 0xfe]), media.srtpContext, seq, 2002, roc);
  };

  // Establish P2P path and receive packet at seq = 50000
  media._p2pUp({ host: '192.168.1.50', port: 55000 });
  assert.ok(media.p2pPath, 'P2P path must be active');
  media._onAudio(makeSrtpAudio(50000, 0), true);
  assert.equal(receivedFrames.length, 1);

  // Twin duplicate packet arriving via relay must be deduplicated
  media._onAudio(makeSrtpAudio(50000, 0), false);
  assert.equal(receivedFrames.length, 1, 'Duplicate relay+P2P packet must be ignored');

  // Simulate network change: local IP changes -> must stop P2P, notify peer via ReqForward subCmd 2, and send ChangeAddress
  media.localIpSig = '192.168.1.10';
  media._getLocalIpSig = () => '10.0.0.25';
  media._checkNetworkAndAudio();
  assert.equal(media.p2pPath, null, 'P2P path must be cleared on local network change');
  assert.ok(
    sentPackets.some((s) => s.what === 'ReqForward stopP2P'),
    'Must send ReqForward subCmd 2 to instruct peer to switch audio to relay'
  );
  assert.ok(
    sentPackets.some((s) => s.what.startsWith('ChangeAddress')),
    'Must send ChangeAddress (cmd 14) to relay server'
  );

  // Simulate peer audio sequence reset backward from 50000 to 1000 (with roc = 0) after network switch
  // Naive RocTracker.guess(1000) would guess roc = 1 (since 50000 - 1000 > 0x8000), which must recover to roc = 0!
  media._onAudio(makeSrtpAudio(1000, 0), false);
  media._onAudio(makeSrtpAudio(1001, 0), false);
  assert.equal(receivedFrames.length, 3, 'Audio packets after backward sequence reset must decrypt and play');
  assert.equal(media.counters.rxBad, 0, 'Must have zero SRTP auth failures after ROC fallback recovery');
});
