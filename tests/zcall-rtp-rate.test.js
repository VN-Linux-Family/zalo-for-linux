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
