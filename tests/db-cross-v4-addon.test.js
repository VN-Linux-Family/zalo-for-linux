'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');
const crypto = require('crypto');
const { execSync } = require('child_process');
const { ROOT, NATIVE_ADDONS_DIR } = require('../scripts/utils/paths');

const NODE_PATH = [
  path.join(ROOT, 'target', 'release', 'db-cross-v4-native.node'),
  path.join(NATIVE_ADDONS_DIR, 'db-cross-v4', 'target', 'release', 'db-cross-v4-native.node'),
].find(p => fs.existsSync(p)) || path.join(ROOT, 'target', 'release', 'db-cross-v4-native.node');
const hasBinary = fs.existsSync(NODE_PATH);

// Helper to construct little-endian TLV buffers
function writeU32LE(buf, val, offset) {
  buf.writeUInt32LE(val, offset);
}

function buildTlvEntryLE(key, payload) {
  const header = Buffer.alloc(8);
  header.writeUInt32LE(key, 0);
  header.writeUInt32LE(payload.length, 4);
  return Buffer.concat([header, payload]);
}

function buildTlvStringLE(key, str) {
  return buildTlvEntryLE(key, Buffer.from(str, 'utf8'));
}

function buildTlvInt32LE(key, val) {
  const p = Buffer.alloc(4);
  p.writeInt32LE(val, 0);
  return buildTlvEntryLE(key, p);
}

function buildTlvInt64LE(key, val) {
  const p = Buffer.alloc(8);
  p.writeBigInt64LE(BigInt(val), 0);
  return buildTlvEntryLE(key, p);
}

describe('db-cross-v4 Native Rust Addon', () => {
  it('validates patch-db-cross-v4.js exports and structure', () => {
    const patchDb = require(path.join(__dirname, '..', 'scripts', 'patches', 'patch-db-cross-v4.js'));
    assert.strictEqual(typeof patchDb.main, 'function');
  });

  it('binary exists and is a valid Linux ELF shared library', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    assert.ok(fs.existsSync(NODE_PATH), `db-cross-v4-native.node must exist at ${NODE_PATH}`);
    const stat = fs.statSync(NODE_PATH);
    assert.ok(stat.size > 0, 'binary must not be empty');

    const fd = fs.openSync(NODE_PATH, 'r');
    const header = Buffer.alloc(4);
    fs.readSync(fd, header, 0, 4, 0);
    fs.closeSync(fd);

    assert.strictEqual(header[0], 0x7f);
    assert.strictEqual(header[1], 0x45); // 'E'
    assert.strictEqual(header[2], 0x4c); // 'L'
    assert.strictEqual(header[3], 0x46); // 'F'
  });

  it('exports all 3 required functions with expected types', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);
    const expected = [
      'decompressAndDecryptDb',
      'decompressAndDecryptDb_V2',
      'parseBinNet',
    ];

    for (const fn of expected) {
      assert.strictEqual(typeof mod[fn], 'function', `Expected export ${fn} to be a function`);
    }
  });

  it('decompressAndDecryptDb returns V1 unsupported status', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);
    const res = mod.decompressAndDecryptDb();
    assert.strictEqual(res.result, -1);
    assert.strictEqual(res.err_message, 'V1 format not fully implemented in this build');
  });

  it('decompressAndDecryptDb_V2 validates required arguments', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);
    const res = mod.decompressAndDecryptDb_V2();
    assert.strictEqual(res.result, -1);
    assert.strictEqual(res.inner_error, 'Invalid args');
    assert.strictEqual(res.error_message, 'Invalid args');
  });

  it('parseBinNet handles edge cases (empty, non-buffer, wrong args)', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);

    // No args
    const resNoArgs = mod.parseBinNet();
    assert.strictEqual(resNoArgs.result, 1);
    assert.strictEqual(resNoArgs.error_message, 'Wrong arguments');

    // Empty buffer
    const resEmpty = mod.parseBinNet(Buffer.alloc(0));
    assert.strictEqual(resEmpty.result, 2);
    assert.strictEqual(resEmpty.inner_error, 'Empty BinNet');

    // Invalid non-TLV buffer
    const resBad = mod.parseBinNet(Buffer.from([0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00]));
    assert.strictEqual(resBad.result, 2);
  });

  it('parseBinNet correctly decodes complex realistic TLV payloads', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);

    // Build sub-TLV for property (0x05)
    const propPayload = Buffer.concat([
      buildTlvInt32LE(0x1e, 5000),      // size
      buildTlvInt32LE(0x22, 1),         // color
      buildTlvInt32LE(0x20, 2),         // type
      buildTlvInt32LE(0x1f, 3),         // subType
      buildTlvStringLE(0x21, 'png'),    // ext
    ]);

    // Build sub-TLV for attach (0x06)
    const attachPayload = Buffer.concat([
      buildTlvStringLE(0x28, 'photo'),
      buildTlvStringLE(0x2f, 'Cute Dog'),
      buildTlvStringLE(0x30, 'https://zalo.me/photos/dog.png'),
      buildTlvStringLE(0x31, 'https://zalo.me/thumb/dog.png'),
      buildTlvStringLE(0x33, '102400'),
      buildTlvInt32LE(0x29, 10), // catId
      buildTlvInt32LE(0x2a, 99), // id
    ]);

    // Build sub-TLV for quote (0x07)
    const quotePayload = Buffer.concat([
      buildTlvInt32LE(0x50, 42),
      buildTlvInt64LE(0x51, 1001),
      buildTlvInt64LE(0x52, 2002),
      buildTlvStringLE(0x56, 'This is quoted text'),
      buildTlvStringLE(0x58, 'normal'),
    ]);

    // Build sub-TLV for mention (0x08)
    const mentionPayload = Buffer.concat([
      buildTlvInt32LE(0x65, 8888), // uid
      buildTlvInt32LE(0x66, 0),    // pos
      buildTlvInt32LE(0x67, 5),    // len
      buildTlvInt32LE(0x68, 1),    // type
    ]);

    // Assemble top-level BinNet buffer
    const topTlv = Buffer.concat([
      buildTlvEntryLE(0x05, propPayload),
      buildTlvEntryLE(0x06, attachPayload),
      buildTlvEntryLE(0x07, quotePayload),
      buildTlvEntryLE(0x08, mentionPayload),
      buildTlvInt32LE(0x09, 1), // quoteStatus
      buildTlvStringLE(0x0a, 'sync-provider-linux'), // dataSource
      buildTlvStringLE(0x0b, 'Hello from real TLV test'), // data
    ]);

    const parsed = mod.parseBinNet(topTlv);
    assert.strictEqual(parsed.result, 0);
    assert.strictEqual(parsed.error_message, '');

    // Verify debug metadata
    assert.strictEqual(parsed.debug.tlvEndian, 'le');
    assert.ok(parsed.debug.inputLen > 0);
    assert.ok(parsed.debug.attachCount >= 1);

    // Verify data
    const d = parsed.data;
    assert.strictEqual(d.dataSource, 'sync-provider-linux');
    assert.strictEqual(d.data, 'Hello from real TLV test');
    assert.strictEqual(d.quoteStatus, 1);

    // Verify property
    assert.strictEqual(d.property.size, 5000);
    assert.strictEqual(d.property.color, 1);
    assert.strictEqual(d.property.type, 2);
    assert.strictEqual(d.property.subType, 3);
    assert.strictEqual(d.property.ext, 'png');

    // Verify attachment
    assert.ok(Array.isArray(d.attachs));
    assert.strictEqual(d.attachs.length, 1);
    assert.strictEqual(d.attachs[0].type, 'photo');
    assert.strictEqual(d.attachs[0].title, 'Cute Dog');
    assert.strictEqual(d.attachs[0].href, 'https://zalo.me/photos/dog.png');
    assert.strictEqual(d.attachs[0].id, 99);

    // Verify quote
    assert.strictEqual(d.quote.ownerId, 42);
    assert.strictEqual(d.quote.cliMsgId, 1001);
    assert.strictEqual(d.quote.msg, 'This is quoted text');
    assert.strictEqual(d.quote.quoteStatus, 'normal');

    // Verify mention
    assert.ok(Array.isArray(d.mentions));
    assert.strictEqual(d.mentions.length, 1);
    assert.strictEqual(d.mentions[0].uid, 8888);
  });

  it('decompressAndDecryptDb_V2 unpacks realistic ZDB4.0 encrypted backup container', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);
    const testDir = path.join(os.tmpdir(), 'zdb4-test-' + Date.now());
    const outDir = path.join(testDir, 'extracted');
    fs.mkdirSync(testDir, { recursive: true });

    try {
      // 1. Create a genuine SQLite database
      const sqliteHeader = Buffer.from('SQLite format 3\0', 'utf8');
      const sqliteBody = Buffer.alloc(1024 - 16, 0xAA);
      const sampleDb = Buffer.concat([sqliteHeader, sqliteBody]);

      // 2. Compress sampleDb with XZ
      const rawXz = execSync('xz -c -z', { input: sampleDb });

      // 3. Build ZDB4.0 plaintext container
      // Header: "ZDB4.0" (6 bytes) + 8 reserved bytes (14 total) + file_count (4 bytes BE)
      const containerHdr = Buffer.alloc(14);
      containerHdr.write('ZDB4.0', 0, 'utf8');

      const fileCountBuf = Buffer.alloc(4);
      fileCountBuf.writeUInt32BE(1, 0);

      const fileName = 'message.db';
      const fileNameBuf = Buffer.from(fileName, 'utf8');
      const fileNameLen = Buffer.alloc(4);
      fileNameLen.writeUInt32BE(fileNameBuf.length, 0);

      const fileSizeBuf = Buffer.alloc(4);
      fileSizeBuf.writeUInt32BE(sampleDb.length, 0);

      const pt = Buffer.concat([
        containerHdr,
        fileCountBuf,
        fileNameLen,
        fileNameBuf,
        fileSizeBuf,
        rawXz,
      ]);

      // Pad pt to AES 16-byte block size
      const padLen = (16 - (pt.length % 16)) % 16;
      const ptPadded = padLen > 0 ? Buffer.concat([pt, Buffer.alloc(padLen, 0)]) : pt;

      // 4. Derive AES-256 key from privateKey string
      const privateKeyStr = 'MY_ZALO_PRIVATE_KEY_1234567890ABCDEF';
      const keyBytes = Buffer.alloc(32, 0);
      for (let i = 0; i < 32 && i < privateKeyStr.length; i++) {
        keyBytes[i] = privateKeyStr.charCodeAt(i);
      }

      // 5. Encrypt with AES-256-CBC chunked 64KB with IV = 0
      const CHUNK_SIZE = 0x10000;
      const ctChunks = [];
      for (let off = 0; off < ptPadded.length; off += CHUNK_SIZE) {
        const chunk = ptPadded.subarray(off, Math.min(off + CHUNK_SIZE, ptPadded.length));
        const cipher = crypto.createCipheriv('aes-256-cbc', keyBytes, Buffer.alloc(16, 0));
        cipher.setAutoPadding(false);
        const ct = Buffer.concat([cipher.update(chunk), cipher.final()]);
        ctChunks.push(ct);
      }
      const fullCt = Buffer.concat(ctChunks);

      const encryptedPath = path.join(testDir, 'backup.zdb');
      fs.writeFileSync(encryptedPath, fullCt);

      // 6. Execute decompressAndDecryptDb_V2
      let progressCount = 0;
      const progressCb = () => {
        progressCount++;
      };

      const result = mod.decompressAndDecryptDb_V2(
        encryptedPath,
        outDir,
        privateKeyStr,
        progressCb
      );

      assert.strictEqual(result.result, 0, `Expected result 0, got ${result.result} (${result.error_message})`);
      assert.strictEqual(result.error_message, '');
      assert.ok(progressCount >= 1, 'Progress callback must have been invoked');

      // 7. Verify extracted database
      const extractedDbPath = path.join(outDir, fileName);
      assert.ok(fs.existsSync(extractedDbPath), 'message.db must exist in output dir');

      const extractedContent = fs.readFileSync(extractedDbPath);
      assert.strictEqual(extractedContent.length, sampleDb.length);
      assert.deepStrictEqual(extractedContent.subarray(0, 16), sqliteHeader);
    } finally {
      fs.rmSync(testDir, { recursive: true, force: true });
    }
  });

  it('decompressAndDecryptDb_V2 sanitizes path traversal and handles trailing XZ bytes', { skip: !hasBinary ? 'db-cross-v4-native.node not compiled in clean checkout' : false }, () => {
    const mod = require(NODE_PATH);
    const testDir = path.join(os.tmpdir(), 'zdb4-sec-' + Date.now());
    const outDir = path.join(testDir, 'extracted');
    fs.mkdirSync(testDir, { recursive: true });

    try {
      // SQLite db
      const sqliteHeader = Buffer.from('SQLite format 3\0', 'utf8');
      const sqliteBody = Buffer.alloc(512 - 16, 0x55);
      const sampleDb = Buffer.concat([sqliteHeader, sqliteBody]);

      // Compress with XZ and append trailing padding bytes (simulating messy stream)
      const rawXz = execSync('xz -c -z', { input: sampleDb });
      const rawXzWithTrailing = Buffer.concat([rawXz, Buffer.from([0x00, 0x00, 0xfe, 0xff])]);

      // Container with path traversal attempt: "../../escaped.db"
      const containerHdr = Buffer.alloc(14);
      containerHdr.write('ZDB4.0', 0, 'utf8');

      const fileCountBuf = Buffer.alloc(4);
      fileCountBuf.writeUInt32BE(1, 0);

      const traversalFileName = '../../escaped.db';
      const fileNameBuf = Buffer.from(traversalFileName, 'utf8');
      const fileNameLen = Buffer.alloc(4);
      fileNameLen.writeUInt32BE(fileNameBuf.length, 0);

      const fileSizeBuf = Buffer.alloc(4);
      fileSizeBuf.writeUInt32BE(sampleDb.length, 0);

      const pt = Buffer.concat([
        containerHdr,
        fileCountBuf,
        fileNameLen,
        fileNameBuf,
        fileSizeBuf,
        rawXzWithTrailing,
      ]);

      // AES key & chunked encryption
      const padLen = (16 - (pt.length % 16)) % 16;
      const ptPadded = padLen > 0 ? Buffer.concat([pt, Buffer.alloc(padLen, 0)]) : pt;

      // AES key & chunked encryption
      const privateKeyStr = 'abcdef1234567890abcdef1234567890';
      const derivedKey = Buffer.from(privateKeyStr.toUpperCase().substring(0, 32), 'utf8');
      const ivZero = Buffer.alloc(16, 0);

      const cipher = crypto.createCipheriv('aes-256-cbc', derivedKey, ivZero);
      cipher.setAutoPadding(false);
      const fullCt = Buffer.concat([cipher.update(ptPadded), cipher.final()]);

      const encryptedPath = path.join(testDir, 'backup_sec.zdb');
      fs.writeFileSync(encryptedPath, fullCt);
      const result = mod.decompressAndDecryptDb_V2(encryptedPath, outDir, privateKeyStr, () => { });
      assert.strictEqual(result.result, 0, `Expected result 0, got ${result.result} (${result.error_message})`);

      // Verify path traversal was blocked: file must be inside outDir as "escaped.db", NOT in testDir or parent
      const safeExtractedPath = path.join(outDir, 'escaped.db');
      assert.ok(fs.existsSync(safeExtractedPath), 'File must be sanitized into outDir/escaped.db');
      assert.ok(!fs.existsSync(path.join(testDir, 'escaped.db')), 'Path traversal must NOT escape outDir');
    } finally {
      fs.rmSync(testDir, { recursive: true, force: true });
    }
  });
});
