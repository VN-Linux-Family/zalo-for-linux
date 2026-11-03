'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const path = require('path');
const { renderTrayBadge, decodePng, encodePng } = require(path.join(__dirname, '..', 'plugins', 'tray-badge', 'badge-icon'));

describe('Badge Icon PNG Decoder, Renderer, and Encoder', () => {
  function createBlankImage(width, height) {
    const data = new Uint8Array(width * height * 4);
    // Fill with semi-transparent blue
    for (let i = 0; i < data.length; i += 4) {
      data[i] = 20;     // R
      data[i + 1] = 120; // G
      data[i + 2] = 220; // B
      data[i + 3] = 255; // A
    }
    return encodePng(width, height, data);
  }

  it('encodes and decodes PNG roundtrip correctly', () => {
    const width = 32;
    const height = 32;
    const pngBuffer = createBlankImage(width, height);

    assert.ok(Buffer.isBuffer(pngBuffer));
    // PNG signature: 89 50 4E 47 0D 0A 1A 0A
    assert.strictEqual(pngBuffer[0], 0x89);
    assert.strictEqual(pngBuffer[1], 0x50);
    assert.strictEqual(pngBuffer[2], 0x4e);
    assert.strictEqual(pngBuffer[3], 0x47);

    const decoded = decodePng(pngBuffer);
    assert.strictEqual(decoded.width, width);
    assert.strictEqual(decoded.height, height);
    assert.strictEqual(decoded.data.length, width * height * 4);

    // Verify pixel values
    assert.strictEqual(decoded.data[0], 20);
    assert.strictEqual(decoded.data[1], 120);
    assert.strictEqual(decoded.data[2], 220);
    assert.strictEqual(decoded.data[3], 255);
  });

  it('renders badge for single digit count', () => {
    const basePng = createBlankImage(32, 32);
    const badged = renderTrayBadge(basePng, 5);

    assert.ok(Buffer.isBuffer(badged));
    const decoded = decodePng(badged);
    assert.strictEqual(decoded.width, 32);
    assert.strictEqual(decoded.height, 32);

    // The top-right badge should have modified red pixels (default badgeColor: 240, 54, 54)
    let foundRed = false;
    for (let i = 0; i < decoded.data.length; i += 4) {
      if (decoded.data[i] === 240 && decoded.data[i + 1] === 54 && decoded.data[i + 2] === 54) {
        foundRed = true;
        break;
      }
    }
    assert.ok(foundRed);
  });

  it('renders badge for 99+ count', () => {
    const basePng = createBlankImage(48, 48);
    const badged = renderTrayBadge(basePng, 150);

    assert.ok(Buffer.isBuffer(badged));
    const decoded = decodePng(badged);
    assert.strictEqual(decoded.width, 48);
    assert.strictEqual(decoded.height, 48);
  });

  it('returns unchanged base icon when count <= 0', () => {
    const basePng = createBlankImage(16, 16);
    const result0 = renderTrayBadge(basePng, 0);
    const resultNeg = renderTrayBadge(basePng, -10);

    const decBase = decodePng(basePng);
    const dec0 = decodePng(result0);
    const decNeg = decodePng(resultNeg);

    assert.ok(Buffer.from(dec0.data).equals(Buffer.from(decBase.data)));
    assert.ok(Buffer.from(decNeg.data).equals(Buffer.from(decBase.data)));
  });
});
