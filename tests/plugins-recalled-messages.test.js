'use strict';

const { describe, it, after } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const vm = require('vm');
const { STORAGE_KEY, MAX_ITEMS, summarize, createRecallLog } = require('../plugins/recalled-messages/log');
const advancedStore = require('../plugins/advanced-settings/store');

function memoryStorage() {
  const data = new Map();
  return {
    getItem: (key) => (data.has(key) ? data.get(key) : null),
    setItem: (key, value) => data.set(key, String(value)),
    removeItem: (key) => data.delete(key)
  };
}

const textMessage = (msgId, message = 'xin chào') => ({
  msgId, message, fromUid: '1', toUid: '2', dName: 'An', sendDttm: '1700000000000'
});

describe('Recalled messages log', () => {
  it('stores a recalled text message once even when Zalo converts it twice', () => {
    const log = createRecallLog(memoryStorage(), () => 42);
    assert.equal(log.remember(textMessage('m1')), true);
    assert.equal(log.remember(textMessage('m1')), false);
    assert.deepEqual(log.load(), [{
      msgId: 'm1', fromUid: '1', toUid: '2', name: 'An', text: 'xin chào', image: '', imageFull: '', video: '',
      sentAt: 1700000000000, recalledAt: 42
    }]);
  });

  it('keeps only the newest entries', () => {
    const log = createRecallLog(memoryStorage());
    for (let i = 0; i < MAX_ITEMS + 5; i++) log.remember(textMessage(`m${i}`));
    const items = log.load();
    assert.equal(items.length, MAX_ITEMS);
    assert.equal(items[0].msgId, 'm5');
  });

  it('summarizes link and photo messages and skips empty ones', () => {
    assert.equal(summarize({ title: 'Tiêu đề', href: 'https://example.com', other: 1 }),
      'Tiêu đề\nhttps://example.com');
    const log = createRecallLog(memoryStorage());
    assert.equal(log.remember(textMessage('m1', {})), false);
    assert.equal(log.remember({ message: 'no id' }), false);
    assert.deepEqual(log.load(), []);
  });

  it('keeps the image link of a recalled photo without a caption', () => {
    const log = createRecallLog(memoryStorage());
    const photo = {
      title: '', description: '', thumbUrl: 'https://cdn.example/t.jpg',
      oriUrl: 'https://cdn.example/o.jpg', hdUrl: undefined, normalUrl: 'https://cdn.example/n.jpg'
    };
    assert.equal(log.remember(textMessage('p1', photo)), true);
    assert.equal(log.load()[0].image, 'https://cdn.example/n.jpg');
    assert.equal(log.load()[0].imageFull, 'https://cdn.example/o.jpg');
    assert.equal(log.load()[0].text, '');
    assert.equal(log.remember(textMessage('p2', { normalUrl: 'javascript:alert(1)' })), false);
  });

  it('keeps a recalled video as its file link with the cover as preview', () => {
    const log = createRecallLog(memoryStorage());
    const video = {
      title: '', description: '', duration: 12000,
      oriUrl: 'https://cdn.example/v.mp4', thumbUrl: 'https://cdn.example/cover.jpg'
    };
    assert.equal(log.remember(textMessage('v1', video)), true);
    const [entry] = log.load();
    assert.equal(entry.video, 'https://cdn.example/v.mp4');
    assert.equal(entry.image, 'https://cdn.example/cover.jpg');
    assert.equal(entry.imageFull, '');
  });

  it('groups entries by the conversation Zalo reports after the recall', () => {
    const log = createRecallLog(memoryStorage());
    log.remember(textMessage('m1'));
    log.remember(textMessage('m2'));
    assert.deepEqual(log.forConversation('c1'), []);
    assert.equal(log.assignConversation('m1', 'c1'), true);
    assert.equal(log.assignConversation('m1', 'c1'), false);
    assert.equal(log.assignConversation('missing', 'c1'), false);
    assert.deepEqual(log.forConversation('c1').map((item) => item.msgId), ['m1']);
    assert.deepEqual(log.forConversation(''), []);
    assert.equal(log.load().length, 2);
  });

  it('clears one conversation without touching the others', () => {
    const log = createRecallLog(memoryStorage());
    for (const id of ['m1', 'm2', 'm3']) log.remember(textMessage(id));
    log.assignConversation('m1', 'c1');
    log.assignConversation('m2', 'c1');
    log.assignConversation('m3', 'c2');
    assert.equal(log.clearConversation('c1'), 2);
    assert.equal(log.clearConversation(''), 0);
    assert.deepEqual(log.load().map((item) => item.msgId), ['m3']);
  });

  it('ignores messages Zalo has already turned into a recall placeholder', () => {
    const storage = memoryStorage();
    const log = createRecallLog(storage);
    assert.equal(log.remember(textMessage('r1', '[Tin nhắn đã được thu hồi]')), false);
    assert.equal(log.remember(textMessage('r2', '[Tin nhắn đã bị xóa]')), false);
    assert.equal(log.remember(textMessage('r3', { catId: 0, id: 0 })), false);
    // Entries captured by an earlier version are dropped on load.
    storage.setItem(STORAGE_KEY, JSON.stringify([{ msgId: 'old', text: '[Tin nhắn đã được thu hồi]' }, { msgId: 'ok', text: 'hi' }]));
    assert.deepEqual(log.load().map((item) => item.msgId), ['ok']);
  });

  it('asks for a flush after every change', () => {
    let flushes = 0;
    const log = createRecallLog(memoryStorage(), Date.now, () => { flushes += 1; });
    log.remember(textMessage('m1'));
    log.remember(textMessage('m1'));
    assert.equal(flushes, 1);
    log.assignConversation('m1', 'c1');
    log.clearConversation('c1');
    log.clear();
    assert.equal(flushes, 4);
  });

  it('recovers from corrupt storage and clears its key', () => {
    const storage = memoryStorage();
    storage.setItem(STORAGE_KEY, '{not json');
    const log = createRecallLog(storage);
    assert.deepEqual(log.load(), []);
    log.remember(textMessage('m1'));
    log.clear();
    assert.equal(storage.getItem(STORAGE_KEY), null);
  });

  it('page scripts parse as plain browser scripts', () => {
    for (const name of ['log.js', 'renderer.js']) {
      const source = fs.readFileSync(path.join(__dirname, '..', 'plugins', 'recalled-messages', name), 'utf8');
      assert.doesNotThrow(() => new vm.Script(source, { filename: name }));
    }
  });
});

describe('Keep recalled messages setting', () => {
  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-recalled-'));
  after(() => fs.rmSync(tmpDir, { recursive: true, force: true }));

  it('is off by default', () => {
    assert.equal(advancedStore.createStore(path.join(tmpDir, 'a')).read().keepRecalled, false);
  });

  it('persists and survives saving a call sound', () => {
    const dir = path.join(tmpDir, 'b');
    const store = advancedStore.createStore(dir);
    assert.equal(store.setKeepRecalled(true).keepRecalled, true);
    store.save('ringtone', '');
    assert.equal(advancedStore.createStore(dir).read().keepRecalled, true);
    assert.equal(store.setKeepRecalled(false).keepRecalled, false);
  });

  it('rejects non-boolean values', () => {
    const store = advancedStore.createStore(path.join(tmpDir, 'c'));
    assert.throws(() => store.setKeepRecalled('yes'));
    assert.equal(store.read().keepRecalled, false);
  });
});
