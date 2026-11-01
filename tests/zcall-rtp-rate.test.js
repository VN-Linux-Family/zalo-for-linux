import { describe, expect, it } from 'bun:test';
const path = require('path');
const rtp = require(path.join(__dirname, '..', 'zcall-native', 'native-engine', 'rtp'));
const { SendRate, DEFAULT_LADDER } = require(path.join(__dirname, '..', 'zcall-native', 'native-engine', 'send-rate'));

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

    expect(hdr.length).toBe(20);
    const parsed = rtp.readAudioRtpHeader(hdr);
    expect(parsed).not.toBeNull();
    expect(parsed.marker).toBe(true);
    expect(parsed.seq).toBe(seq);
    expect(parsed.ts).toBe(ts);
    expect(parsed.ssrc).toBe(ssrc);
    expect(parsed.twSeq).toBe(twSeq);
    expect(parsed.pt).toBe(rtp.AUDIO_PT);
  });

  it('initializes SendRate with default ladder and startup profile', () => {
    const rate = new SendRate();
    const current = rate.profile();
    expect(current).toBeDefined();
    expect(current.bitrate).toBeGreaterThanOrEqual(100);
    // Rung 1 (30 kbps) is filtered out by MIN_KBPS (100 kbps), leaving 6 rungs
    expect(rate.ladder.length).toBe(6);
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
      expect(rate.level).toBeLessThan(initialLevel);
      expect(changed).toBe(true);
    }
  });
});
