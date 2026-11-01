import { describe, expect, it } from 'bun:test';
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

    expect(Buffer.isBuffer(pngBuffer)).toBe(true);
    // PNG signature: 89 50 4E 47 0D 0A 1A 0A
    expect(pngBuffer[0]).toBe(0x89);
    expect(pngBuffer[1]).toBe(0x50);
    expect(pngBuffer[2]).toBe(0x4e);
    expect(pngBuffer[3]).toBe(0x47);

    const decoded = decodePng(pngBuffer);
    expect(decoded.width).toBe(width);
    expect(decoded.height).toBe(height);
    expect(decoded.data.length).toBe(width * height * 4);

    // Verify pixel values
    expect(decoded.data[0]).toBe(20);
    expect(decoded.data[1]).toBe(120);
    expect(decoded.data[2]).toBe(220);
    expect(decoded.data[3]).toBe(255);
  });

  it('renders badge for single digit count', () => {
    const basePng = createBlankImage(32, 32);
    const badged = renderTrayBadge(basePng, 5);

    expect(Buffer.isBuffer(badged)).toBe(true);
    const decoded = decodePng(badged);
    expect(decoded.width).toBe(32);
    expect(decoded.height).toBe(32);

    // The top-right badge should have modified red pixels (default badgeColor: 240, 54, 54)
    let foundRed = false;
    for (let i = 0; i < decoded.data.length; i += 4) {
      if (decoded.data[i] === 240 && decoded.data[i + 1] === 54 && decoded.data[i + 2] === 54) {
        foundRed = true;
        break;
      }
    }
    expect(foundRed).toBe(true);
  });

  it('renders badge for 99+ count', () => {
    const basePng = createBlankImage(48, 48);
    const badged = renderTrayBadge(basePng, 150);

    expect(Buffer.isBuffer(badged)).toBe(true);
    const decoded = decodePng(badged);
    expect(decoded.width).toBe(48);
    expect(decoded.height).toBe(48);
  });

  it('returns unchanged base icon when count <= 0', () => {
    const basePng = createBlankImage(16, 16);
    const result0 = renderTrayBadge(basePng, 0);
    const resultNeg = renderTrayBadge(basePng, -10);

    const decBase = decodePng(basePng);
    const dec0 = decodePng(result0);
    const decNeg = decodePng(resultNeg);

    expect(Buffer.from(dec0.data).equals(Buffer.from(decBase.data))).toBe(true);
    expect(Buffer.from(decNeg.data).equals(Buffer.from(decBase.data))).toBe(true);
  });
});
