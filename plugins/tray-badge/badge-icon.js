'use strict';

// Compose an unread-count badge onto a square base icon, using only Node's
// built-in zlib. No canvas, GPU or native dependency, so it runs the same on
// every Linux desktop (including headless/software-rendered sessions) where a
// BrowserWindow-based renderer would be fragile.
//
// Pipeline: decode the base PNG to RGBA, paint a red disc with the count in a
// small embedded bitmap font, then re-encode to PNG. Input must be an 8-bit,
// non-interlaced PNG (RGBA or RGB) — the Zalo app icons shipped in pc-dist are.

const zlib = require('zlib');

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

// --- CRC32 (PNG polynomial) -------------------------------------------------
const CRC_TABLE = (() => {
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  return table;
})();

function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

// --- PNG decode -------------------------------------------------------------
function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  if (pa <= pb && pa <= pc) return a;
  if (pb <= pc) return b;
  return c;
}

function decodePng(buffer) {
  if (!buffer.slice(0, 8).equals(PNG_SIGNATURE)) throw new Error('not a PNG');

  let width = 0;
  let height = 0;
  let bitDepth = 0;
  let colorType = 0;
  let interlace = 0;
  const idat = [];

  let offset = 8;
  while (offset < buffer.length) {
    const length = buffer.readUInt32BE(offset);
    const type = buffer.toString('ascii', offset + 4, offset + 8);
    const dataStart = offset + 8;
    if (type === 'IHDR') {
      width = buffer.readUInt32BE(dataStart);
      height = buffer.readUInt32BE(dataStart + 4);
      bitDepth = buffer[dataStart + 8];
      colorType = buffer[dataStart + 9];
      interlace = buffer[dataStart + 12];
    } else if (type === 'IDAT') {
      idat.push(buffer.slice(dataStart, dataStart + length));
    } else if (type === 'IEND') {
      break;
    }
    offset = dataStart + length + 4; // skip data + CRC
  }

  if (bitDepth !== 8 || interlace !== 0 || (colorType !== 6 && colorType !== 2)) {
    throw new Error(`unsupported PNG (depth ${bitDepth}, color ${colorType}, interlace ${interlace})`);
  }

  const channels = colorType === 6 ? 4 : 3;
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const rgba = new Uint8Array(width * height * 4);
  const prev = new Uint8Array(stride);
  const cur = new Uint8Array(stride);

  let pos = 0;
  for (let y = 0; y < height; y++) {
    const filter = raw[pos++];
    for (let x = 0; x < stride; x++) {
      const rawByte = raw[pos++];
      const a = x >= channels ? cur[x - channels] : 0;
      const b = prev[x];
      const c = x >= channels ? prev[x - channels] : 0;
      let value;
      switch (filter) {
        case 0: value = rawByte; break;
        case 1: value = rawByte + a; break;
        case 2: value = rawByte + b; break;
        case 3: value = rawByte + ((a + b) >> 1); break;
        case 4: value = rawByte + paeth(a, b, c); break;
        default: throw new Error(`bad filter ${filter}`);
      }
      cur[x] = value & 0xff;
    }
    for (let x = 0; x < width; x++) {
      const si = x * channels;
      const di = (y * width + x) * 4;
      rgba[di] = cur[si];
      rgba[di + 1] = cur[si + 1];
      rgba[di + 2] = cur[si + 2];
      rgba[di + 3] = channels === 4 ? cur[si + 3] : 255;
    }
    prev.set(cur);
  }

  return { width, height, data: rgba };
}

// --- PNG encode (8-bit RGBA, filter 0) --------------------------------------
function chunk(type, data) {
  const typeBuf = Buffer.from(type, 'ascii');
  const body = Buffer.concat([typeBuf, data]);
  const out = Buffer.alloc(data.length + 12);
  out.writeUInt32BE(data.length, 0);
  body.copy(out, 4);
  out.writeUInt32BE(crc32(body), out.length - 4);
  return out;
}

function encodePng(width, height, rgba) {
  const stride = width * 4;
  const raw = Buffer.alloc((stride + 1) * height);
  for (let y = 0; y < height; y++) {
    raw[y * (stride + 1)] = 0; // filter: none
    Buffer.from(rgba.buffer, rgba.byteOffset + y * stride, stride).copy(raw, y * (stride + 1) + 1);
  }

  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // color type RGBA
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;

  return Buffer.concat([
    PNG_SIGNATURE,
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// --- 5x7 bitmap font for 0-9 and '+' ---------------------------------------
const GLYPHS = {
  '0': ['01110', '10001', '10011', '10101', '11001', '10001', '01110'],
  '1': ['00100', '01100', '00100', '00100', '00100', '00100', '01110'],
  '2': ['01110', '10001', '00001', '00010', '00100', '01000', '11111'],
  '3': ['11111', '00010', '00100', '00010', '00001', '10001', '01110'],
  '4': ['00010', '00110', '01010', '10010', '11111', '00010', '00010'],
  '5': ['11111', '10000', '11110', '00001', '00001', '10001', '01110'],
  '6': ['00110', '01000', '10000', '11110', '10001', '10001', '01110'],
  '7': ['11111', '00001', '00010', '00100', '01000', '01000', '01000'],
  '8': ['01110', '10001', '10001', '01110', '10001', '10001', '01110'],
  '9': ['01110', '10001', '10001', '01111', '00001', '00010', '01100'],
  '+': ['00000', '00100', '00100', '11111', '00100', '00100', '00000'],
};
const GLYPH_W = 5;
const GLYPH_H = 7;

// --- drawing helpers --------------------------------------------------------
function blend(data, idx, r, g, b, a) {
  if (a >= 255) {
    data[idx] = r;
    data[idx + 1] = g;
    data[idx + 2] = b;
    data[idx + 3] = 255;
    return;
  }
  const alpha = a / 255;
  const inv = 1 - alpha;
  data[idx] = Math.round(r * alpha + data[idx] * inv);
  data[idx + 1] = Math.round(g * alpha + data[idx + 1] * inv);
  data[idx + 2] = Math.round(b * alpha + data[idx + 2] * inv);
  data[idx + 3] = Math.max(data[idx + 3], a);
}

function fillDisc(img, cx, cy, radius, rgb) {
  const { width, height, data } = img;
  const x0 = Math.max(0, Math.floor(cx - radius - 1));
  const x1 = Math.min(width - 1, Math.ceil(cx + radius + 1));
  const y0 = Math.max(0, Math.floor(cy - radius - 1));
  const y1 = Math.min(height - 1, Math.ceil(cy + radius + 1));
  for (let y = y0; y <= y1; y++) {
    for (let x = x0; x <= x1; x++) {
      const dx = x + 0.5 - cx;
      const dy = y + 0.5 - cy;
      const dist = Math.sqrt(dx * dx + dy * dy);
      let a;
      if (dist <= radius - 1) a = 255; // interior
      else if (dist >= radius) a = 0; // outside
      else a = Math.round((radius - dist) * 255); // antialiased edge
      if (a > 0) blend(data, (y * width + x) * 4, rgb[0], rgb[1], rgb[2], a);
    }
  }
}

function drawGlyph(img, glyph, originX, originY, scale, rgb) {
  const { width, height, data } = img;
  for (let gy = 0; gy < GLYPH_H; gy++) {
    const row = glyph[gy];
    for (let gx = 0; gx < GLYPH_W; gx++) {
      if (row[gx] !== '1') continue;
      for (let sy = 0; sy < scale; sy++) {
        for (let sx = 0; sx < scale; sx++) {
          const px = originX + gx * scale + sx;
          const py = originY + gy * scale + sy;
          if (px < 0 || py < 0 || px >= width || py >= height) continue;
          blend(data, (py * width + px) * 4, rgb[0], rgb[1], rgb[2], 255);
        }
      }
    }
  }
}

function drawText(img, text, cx, cy, scale, rgb) {
  const spacing = scale; // gap between glyphs
  const totalW = text.length * GLYPH_W * scale + (text.length - 1) * spacing;
  const totalH = GLYPH_H * scale;
  let x = Math.round(cx - totalW / 2);
  const y = Math.round(cy - totalH / 2);
  for (const ch of text) {
    const glyph = GLYPHS[ch];
    if (glyph) drawGlyph(img, glyph, x, y, scale, rgb);
    x += GLYPH_W * scale + spacing;
  }
}

/**
 * Render a tray icon: the base icon with an unread-count badge in the
 * top-right corner. count <= 0 returns the base icon re-encoded unchanged.
 * Returns a PNG Buffer.
 */
function renderTrayBadge(basePngBuffer, count, options = {}) {
  const img = decodePng(basePngBuffer);
  const n = Math.max(0, Math.floor(count) || 0);
  if (n <= 0) return encodePng(img.width, img.height, img.data);

  const text = n > 99 ? '99+' : String(n);
  const size = Math.min(img.width, img.height);

  const red = options.badgeColor || [240, 54, 54];
  const white = options.textColor || [255, 255, 255];

  // Badge sits in the top-right; larger for longer text so digits stay legible.
  const radius = size * (text.length >= 3 ? 0.30 : text.length === 2 ? 0.27 : 0.24);
  const cx = img.width - radius - size * 0.04;
  const cy = radius + size * 0.04;

  fillDisc(img, cx, cy, radius, red);

  // Scale digits to fit inside the disc on both axes. Text width with
  // per-glyph spacing of one scale unit is scale*(6*len - 1); height is 7*scale.
  const availW = radius * 1.5;
  const availH = radius * 1.4;
  const scaleW = Math.floor(availW / (6 * text.length - 1));
  const scaleH = Math.floor(availH / GLYPH_H);
  const scale = Math.max(1, Math.min(scaleW, scaleH));
  drawText(img, text, cx, cy, scale, white);

  return encodePng(img.width, img.height, img.data);
}

module.exports = { renderTrayBadge, decodePng, encodePng };
