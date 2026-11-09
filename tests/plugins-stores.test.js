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
    it('preserves settings owned by other plugins when writing advanced settings', () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-settings-shared-'));
    try {
      fs.writeFileSync(path.join(dir, 'zalo-linux-settings.json'), JSON.stringify({
        devMode: 1,
        externalPluginSetting: 'keep-me'
      }));
      const store = advancedStore.createStore(dir);
      store.saveScreenshotDelay('withZalo', 250);
      const saved = JSON.parse(fs.readFileSync(path.join(dir, 'zalo-linux-settings.json'), 'utf8'));
      assert.equal(saved.devMode, 1);
      assert.equal(saved.externalPluginSetting, 'keep-me');
      assert.equal(saved.screenshotDelayWithZalo, 250);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('reads and initializes settings with empty values and default screenshot delays', () => {
      const store = advancedStore.createStore(path.join(tmpDir, 'adv'));
      const settings = store.read();
      assert.ok(settings !== undefined);
      assert.strictEqual(typeof settings.ringtone, 'string');
      assert.strictEqual(typeof settings.ringback, 'string');
      assert.strictEqual(settings.screenshotDelayWithZalo, 100);
      assert.strictEqual(settings.screenshotDelayWithoutZalo, 500);
    });
    it('validates sound kinds properly', () => {
      const store = advancedStore.createStore(path.join(tmpDir, 'adv'));
      assert.throws(() => store.save('invalid_kind', '/dummy/path.mp3'));
    });

    it('saves and reads custom screenshot delays for withZalo and withoutZalo', () => {
      const advDir = path.join(tmpDir, 'adv-delay');
      const store = advancedStore.createStore(advDir);

      const savedWith = store.saveScreenshotDelay('withZalo', 250);
      assert.strictEqual(savedWith.screenshotDelayWithZalo, 250);

      const savedWithout = store.saveScreenshotDelay('withoutZalo', 750);
      assert.strictEqual(savedWithout.screenshotDelayWithoutZalo, 750);

      const readBack = store.read();
      assert.strictEqual(readBack.screenshotDelayWithZalo, 250);
      assert.strictEqual(readBack.screenshotDelayWithoutZalo, 750);
    });

    it('validates screenshot delay input values and kinds', () => {
      const store = advancedStore.createStore(path.join(tmpDir, 'adv-val'));
      assert.throws(() => store.saveScreenshotDelay('unknownKind', 100));
      assert.throws(() => store.saveScreenshotDelay('withZalo', -50));
      assert.throws(() => store.saveScreenshotDelay('withoutZalo', 15000));
      assert.throws(() => store.saveScreenshotDelay('withZalo', 'not_a_number'));

      // Empty string resets to default
      const resetWith = store.saveScreenshotDelay('withZalo', '');
      assert.strictEqual(resetWith.screenshotDelayWithZalo, 100);
      const resetWithout = store.saveScreenshotDelay('withoutZalo', '');
      assert.strictEqual(resetWithout.screenshotDelayWithoutZalo, 500);
    });
  });
});
