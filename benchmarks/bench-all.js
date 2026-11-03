'use strict';

const path = require('path');
const srtp = require(path.join(__dirname, '..', 'zcall-native', 'native-engine', 'srtp-aes'));
const zrtp = require(path.join(__dirname, '..', 'zcall-native', 'native-engine', 'zrtp-packet'));
const { renderTrayBadge, encodePng, decodePng } = require(path.join(__dirname, '..', 'plugins', 'tray-badge', 'badge-icon'));

function runBench(name, fn, iterations = 1000) {
  // Warmup
  for (let i = 0; i < Math.min(iterations, 100); i++) {
    fn();
  }

  const startMem = process.memoryUsage().heapUsed;
  const start = performance.now();
  for (let i = 0; i < iterations; i++) {
    fn();
  }
  const elapsedMs = performance.now() - start;
  const endMem = process.memoryUsage().heapUsed;

  const opsPerSec = Math.round((iterations / elapsedMs) * 1000);
  const avgUs = ((elapsedMs / iterations) * 1000).toFixed(2);
  const memDeltaKb = Math.round((endMem - startMem) / 1024);

  console.log(`| ${name.padEnd(38)} | ${String(iterations).padStart(8)} | ${elapsedMs.toFixed(2).padStart(9)} ms | ${String(opsPerSec).padStart(10)} ops/s | ${avgUs.padStart(8)} µs/op | ${String(memDeltaKb).padStart(8)} KB |`);
  return { name, iterations, elapsedMs, opsPerSec, avgUs, memDeltaKb };
}

console.log('='.repeat(96));
console.log(`BENCHMARK SUITE - Runtime: ${typeof Bun !== 'undefined' ? `Bun v${Bun.version}` : `Node.js ${process.version}`}`);
console.log('='.repeat(96));
console.log(`| ${'Target / Function'.padEnd(38)} | ${'Runs'.padStart(8)} | ${'Elapsed'.padStart(12)} | ${'Throughput'.padStart(16)} | ${'Avg Latency'.padStart(14)} | ${'Heap Mem'.padStart(8)} |`);
console.log('-'.repeat(96));

// 1. SRTP Encryption / Decryption Benchmarks
const masterKey = Buffer.alloc(30, 0x5a);
const keys = srtp.sessionKeys(masterKey);
const opusPayload = Buffer.alloc(160, 0xab); // 20ms 16kHz audio frame
const rtpHeader = Buffer.alloc(20);
rtpHeader[0] = 0x90;
rtpHeader[1] = 112;

runBench('SRTP protectRtp (Audio 160B)', () => {
  srtp.protectRtp(rtpHeader, opusPayload, keys, 100, 123456, 0);
}, 2000);

const protectedAudio = srtp.protectRtp(rtpHeader, opusPayload, keys, 100, 123456, 0);
runBench('SRTP unprotectRtp (Audio 160B)', () => {
  srtp.unprotectRtp(protectedAudio, keys, 0);
}, 2000);

const videoPayload = Buffer.alloc(1200, 0xcd); // Typical video MTU frame
const protectedVideo = srtp.protectRtp(rtpHeader, videoPayload, keys, 100, 123456, 0);
runBench('SRTP protectRtp (Video 1200B)', () => {
  srtp.protectRtp(rtpHeader, videoPayload, keys, 100, 123456, 0);
}, 1000);

runBench('SRTP unprotectRtp (Video 1200B)', () => {
  srtp.unprotectRtp(protectedVideo, keys, 0);
}, 1000);

// 2. ZRTP Packet Protocol Benchmarks
runBench('ZRTP wrapMediaPacket', () => {
  zrtp.wrapMediaPacket({ msgType: zrtp.MSG_TYPES.AUDIO_RTP, token: 12345, payload: opusPayload });
}, 50000);

const mediaPacket = zrtp.wrapMediaPacket({ msgType: zrtp.MSG_TYPES.AUDIO_RTP, token: 12345, payload: opusPayload });
runBench('ZRTP unwrapPacket', () => {
  zrtp.unwrapPacket(mediaPacket);
}, 50000);

runBench('ZRTP buildControlHeader', () => {
  zrtp.buildControlHeader({ msgType: zrtp.MSG_TYPES.CONTROL, seq: 10, uid: 123, token: 456, cmd: zrtp.COMMANDS.PING });
}, 50000);

// 3. Tray Badge Image Processing Benchmarks
const blank32 = Buffer.alloc(32 * 32 * 4, 128);
const png32 = encodePng(32, 32, blank32);

runBench('Badge decodePng (32x32 RGBA)', () => {
  decodePng(png32);
}, 500);

runBench('Badge encodePng (32x32 RGBA, zlib 9)', () => {
  encodePng(32, 32, blank32);
}, 500);

runBench('Badge renderTrayBadge full (count=5)', () => {
  renderTrayBadge(png32, 5);
}, 500);

runBench('Badge renderTrayBadge full (count=150)', () => {
  renderTrayBadge(png32, 150);
}, 500);

console.log('='.repeat(96));
