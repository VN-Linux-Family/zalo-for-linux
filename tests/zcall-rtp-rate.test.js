'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { ZCALL_DIR } = require('../scripts/utils/paths');
const rtp = require(path.join(ZCALL_DIR, 'native-engine', 'rtp'));
const { SendRate, DEFAULT_LADDER } = require(path.join(ZCALL_DIR, 'native-engine', 'send-rate'));

describe('RTP & SendRate Adaptation', () => {
  it('builds and parses audio RTP headers accurately', () => {
    const seq = 105;
    const ts = 960320;
    const ssrc = 987654321;
    const twSeq = 502;

    const hdr = rtp.buildAudioRtpHeader({
      seq,
      ts,
      ssrc,
      twSeq,
      marker: true
    });

    assert.strictEqual(hdr.length, 20);
    const parsed = rtp.readAudioRtpHeader(hdr);
    assert.ok(parsed !== null);
    assert.strictEqual(parsed.marker, true);
    assert.strictEqual(parsed.seq, seq);
    assert.strictEqual(parsed.ts, ts);
    assert.strictEqual(parsed.ssrc, ssrc);
    assert.strictEqual(parsed.twSeq, twSeq);
    assert.strictEqual(parsed.pt, rtp.AUDIO_PT);
  });

  it('initializes SendRate with default ladder and startup profile', () => {
    const rate = new SendRate();
    const current = rate.profile();
    assert.ok(current !== undefined);
    assert.ok(current.bitrate >= 100);
    // Rung 1 (30 kbps) is filtered out by MIN_KBPS (100 kbps), leaving 6 rungs
    assert.strictEqual(rate.ladder.length, 6);
  });

  it('falls back to the default ladder when no server rung fits', () => {
    // Every bwProfile is under MIN_KBPS (or too big for maxWidth): the
    // constructor used to throw on an empty ladder.
    for (const config of [
      { bwProfiles: [{ id: 1, res: 240, fps: 8, bitrate: 30 }] },
      { bwProfiles: [{ id: 7, res: 720, fps: 24, bitrate: 1100 }], maxWidth: 640 },
    ]) {
      const rate = new SendRate(config, { log: () => {} });
      const kbps = rate.ladder.map((p) => p.kbps);
      assert.deepStrictEqual(kbps, DEFAULT_LADDER.filter((p) => p.bitrate >= 100).map((p) => p.bitrate));
      assert.strictEqual(rate.profile().bitrate, 300000);
    }
  });

  it('steps down on packet loss and respects backoff', () => {
    let changed = false;
    const rate = new SendRate({}, {
      onProfile: () => { changed = true; }
    });

    const initialLevel = rate.level;
    // Downlink audio loss of 50% triggers immediate step down
    rate.tick(true, 0.5);

    if (initialLevel > 0) {
      assert.ok(rate.level < initialLevel);
      assert.strictEqual(changed, true);
    }
  });
});
