'use strict';

const path = require('path');
const { ZCALL_DIR } = require('../scripts/utils/paths');
const srtp = require(path.join(ZCALL_DIR, 'native-engine', 'srtp-aes'));
const zrtp = require(path.join(ZCALL_DIR, 'native-engine', 'zrtp-packet'));
const { renderTrayBadge, encodePng, decodePng } = require(path.join(__dirname, '..', 'plugins', 'tray-badge', 'badge-icon'));

function runBench(name, fn, iterations = 1000) {
  for (let i = 0; i < Math.min(iterations, 100); i++) fn();

  if (global.gc) global.gc();
  const startMem = process.memoryUsage();
  let peakMem = startMem;
  const start = performance.now();

  for (let i = 0; i < iterations; i++) {
    fn();
    if ((i + 1) % 100 === 0 || i === iterations - 1) {
      const current = process.memoryUsage();
      peakMem = {
        rss: Math.max(peakMem.rss, current.rss),
        heapUsed: Math.max(peakMem.heapUsed, current.heapUsed),
        external: Math.max(peakMem.external, current.external),
        arrayBuffers: Math.max(peakMem.arrayBuffers, current.arrayBuffers),
      };
    }
  }

  const elapsedMs = performance.now() - start;
  const endMem = process.memoryUsage();
  const opsPerSec = Math.round((iterations / elapsedMs) * 1000);
  const avgUs = (elapsedMs / iterations) * 1000;
  const mib = (bytes) => (bytes / 1024 / 1024).toFixed(2);

  console.log(`\n${name} (${iterations.toLocaleString()} runs)`);
  console.log(`  Time: ${elapsedMs.toFixed(2)} ms | ${opsPerSec.toLocaleString()} ops/s | ${avgUs.toFixed(2)} µs/op`);
  console.log(`  RSS: start ${mib(startMem.rss)} MiB | peak sampled ${mib(peakMem.rss)} MiB | end ${mib(endMem.rss)} MiB | delta ${mib(endMem.rss - startMem.rss)} MiB`);
  console.log(`  Heap: start ${mib(startMem.heapUsed)} MiB | peak sampled ${mib(peakMem.heapUsed)} MiB | end ${mib(endMem.heapUsed)} MiB | delta ${mib(endMem.heapUsed - startMem.heapUsed)} MiB`);
  console.log(`  External: start ${mib(startMem.external)} MiB | peak sampled ${mib(peakMem.external)} MiB | end ${mib(endMem.external)} MiB | delta ${mib(endMem.external - startMem.external)} MiB`);
  console.log(`  ArrayBuffers: start ${mib(startMem.arrayBuffers)} MiB | peak sampled ${mib(peakMem.arrayBuffers)} MiB | end ${mib(endMem.arrayBuffers)} MiB`);

  return { name, iterations, elapsedMs, opsPerSec, avgUs, startMem, peakMem, endMem };
}

console.log('='.repeat(96));
console.log(`BENCHMARK SUITE - Runtime: ${typeof Bun !== 'undefined' ? `Bun v${Bun.version}` : `Node.js ${process.version}`}`);
console.log(`Logical CPUs: ${require('os').availableParallelism?.() || require('os').cpus().length}`);
console.log('Memory peaks are sampled every 100 iterations; run with --expose-gc for GC before each case.');
console.log('='.repeat(96));

const masterKey = Buffer.alloc(30, 0x5a);
const keys = srtp.sessionKeys(masterKey);
const opusPayload = Buffer.alloc(160, 0xab);
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

const videoPayload = Buffer.alloc(1200, 0xcd);
const protectedVideo = srtp.protectRtp(rtpHeader, videoPayload, keys, 100, 123456, 0);
runBench('SRTP protectRtp (Video 1200B)', () => {
  srtp.protectRtp(rtpHeader, videoPayload, keys, 100, 123456, 0);
}, 1000);

runBench('SRTP unprotectRtp (Video 1200B)', () => {
  srtp.unprotectRtp(protectedVideo, keys, 0);
}, 1000);

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

const blank32 = Buffer.alloc(32 * 32 * 4, 128);
const png32 = encodePng(32, 32, blank32);

runBench('Badge decodePng (32x32 RGBA)', () => decodePng(png32), 500);
runBench('Badge encodePng (32x32 RGBA, zlib 9)', () => encodePng(32, 32, blank32), 500);
runBench('Badge renderTrayBadge full (count=5)', () => renderTrayBadge(png32, 5), 500);
runBench('Badge renderTrayBadge full (count=150)', () => renderTrayBadge(png32, 150), 500);

console.log('\n' + '='.repeat(96));
