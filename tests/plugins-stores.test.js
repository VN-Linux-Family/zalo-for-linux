'use strict';

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');
const os = require('os');
const advancedStore = require(path.join(__dirname, '..', 'plugins', 'advanced-settings', 'store'));
const offlineStore = require(path.join(__dirname, '..', 'plugins', 'offline', 'store'));

describe('Plugin Stores & Persistence', () => {
  const tmpDir = path.join(os.tmpdir(), `zalo-test-${Date.now()}`);

  after(() => {
    try {
      fs.rmSync(tmpDir, { recursive: true, force: true });
    } catch (_) { }
  });

  describe('Offline Session Store', () => {
    it('creates session key from cookies', () => {
      const dummyCookies = [
        { name: 'zpw_sek', value: 'secret-val-1', domain: 'zalo.me', httpOnly: true, secure: true },
        { name: 'other_cookie', value: 'ignore', domain: 'zalo.me', httpOnly: false, secure: false }
      ];
      const key = offlineStore.sessionKey(dummyCookies);
      assert.ok(key !== null);
      assert.strictEqual(key.length, 32); // SHA-256 digest
    });

    it('encrypts, writes, and reads back offline session record', () => {
      const store = offlineStore.createStore(tmpDir);
      const key = Buffer.alloc(32, 0x07);

      const record = {
        version: 1,
        clientId: 'client-test-id-123',
        info: {
          config: {
            uid: '12345678',
            settings: { features: { enableVoice: true } }
          },
          UIN: '12345678',
          decryptKey: 'dummy-decrypt-key'
        }
      };

      assert.strictEqual(offlineStore.validRecord(record), true);

      const writeOk = store.write(key, record);
      assert.strictEqual(writeOk, true);

      const readBack = store.read(key);
      assert.ok(readBack !== null);
      assert.strictEqual(readBack.clientId, record.clientId);
      assert.strictEqual(readBack.info.UIN, record.info.UIN);

      // Wrong key fails decryption
      const wrongKey = Buffer.alloc(32, 0x99);
      assert.strictEqual(store.read(wrongKey), null);

      store.clear();
      assert.strictEqual(store.read(key), null);
    });
  });

  describe('Advanced Settings Store', () => {
    it('reads and initializes settings with empty values', () => {
      const store = advancedStore.createStore(path.join(tmpDir, 'adv'));
      const settings = store.read();
      assert.ok(settings !== undefined);
      assert.strictEqual(typeof settings.ringtone, 'string');
      assert.strictEqual(typeof settings.ringback, 'string');
    });

    it('validates sound kinds properly', () => {
      const store = advancedStore.createStore(path.join(tmpDir, 'adv'));
      assert.throws(() => store.save('invalid_kind', '/dummy/path.mp3'));
    });
  });
});
