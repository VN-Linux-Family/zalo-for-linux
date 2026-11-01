import { describe, expect, it, afterAll } from 'bun:test';
const fs = require('fs');
const path = require('path');
const os = require('os');
const advancedStore = require(path.join(__dirname, '..', 'plugins', 'advanced-settings', 'store'));
const offlineStore = require(path.join(__dirname, '..', 'plugins', 'offline', 'store'));

describe('Plugin Stores & Persistence', () => {
  const tmpDir = path.join(os.tmpdir(), `zalo-test-${Date.now()}`);

  afterAll(() => {
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
      expect(key).not.toBeNull();
      expect(key.length).toBe(32); // SHA-256 digest
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

      expect(offlineStore.validRecord(record)).toBe(true);

      const writeOk = store.write(key, record);
      expect(writeOk).toBe(true);

      const readBack = store.read(key);
      expect(readBack).not.toBeNull();
      expect(readBack.clientId).toBe(record.clientId);
      expect(readBack.info.UIN).toBe(record.info.UIN);

      // Wrong key fails decryption
      const wrongKey = Buffer.alloc(32, 0x99);
      expect(store.read(wrongKey)).toBeNull();

      store.clear();
      expect(store.read(key)).toBeNull();
    });
  });

  describe('Advanced Settings Store', () => {
    it('reads and initializes settings with empty values', () => {
      const store = advancedStore.createStore(path.join(tmpDir, 'adv'));
      const settings = store.read();
      expect(settings).toBeDefined();
      expect(typeof settings.ringtone).toBe('string');
      expect(typeof settings.ringback).toBe('string');
    });

    it('validates sound kinds properly', () => {
      const store = advancedStore.createStore(path.join(tmpDir, 'adv'));
      expect(() => store.save('invalid_kind', '/dummy/path.mp3')).toThrow();
    });
  });
});
