// Pure storage logic for recalled messages. Injected into the Zalo page before
// renderer.js, and required directly by the tests.
(function (root) {
  'use strict';

  const STORAGE_KEY = 'zalo-linux:recalled-messages';
  const MAX_ITEMS = 500;
  const MAX_TEXT = 4000;

  // What convertToRecalled leaves behind. Zalo calls it again for messages
  // that were already recalled (e.g. when reloading a conversation), and
  // those must not be stored as if they were the original content.
  const PLACEHOLDERS = ['[Tin nhắn đã được thu hồi]', '[Tin nhắn đã bị xóa]'];
  function alreadyRecalled(message) {
    if (typeof message === 'string') return PLACEHOLDERS.includes(message);
    return !!message && typeof message === 'object' && message.catId === 0 && message.id === 0;
  }

  // Text messages carry a string; links, photos and files carry an object.
  function summarize(message) {
    if (typeof message === 'string') return message.slice(0, MAX_TEXT);
    if (!message || typeof message !== 'object') return '';
    return ['title', 'description', 'href', 'thumb']
      .map((key) => (typeof message[key] === 'string' ? message[key] : ''))
      .filter(Boolean)
      .join('\n')
      .slice(0, MAX_TEXT);
  }

  // Photos keep only a link to Zalo's CDN, not the image itself, so storage
  // stays small. Prefer the mid-size rendition over the original.
  function imageUrl(message, keys = ['normalUrl', 'hdUrl', 'oriUrl', 'thumbUrl']) {
    if (!message || typeof message !== 'object') return '';
    for (const key of keys) {
      const value = message[key];
      if (typeof value === 'string' && /^https:\/\//.test(value)) return value;
    }
    return '';
  }

  // Videos carry a duration; their oriUrl is the video file and thumbUrl the
  // cover. Photos have no duration and use oriUrl for the full-size image.
  function media(message) {
    if (message && typeof message === 'object' && typeof message.duration === 'number') {
      return { image: imageUrl(message, ['thumbUrl']), imageFull: '', video: imageUrl(message, ['oriUrl']) };
    }
    const image = imageUrl(message);
    return {
      image,
      imageFull: image ? imageUrl(message, ['oriUrl', 'hdUrl', 'normalUrl']) || image : '',
      video: ''
    };
  }

  // Options:
  // - onWrite runs after every change so the caller can flush it to disk;
  //   Chromium otherwise commits localStorage a few seconds later and a
  //   SIGTERM in between (logout, shutdown) silently undoes a deletion.
  // - owner returns the logged-in account id. Several accounts can share one
  //   Zalo profile, so entries are tagged and only the owner's are visible.
  function createRecallLog(storage, { now = Date.now, onWrite = () => {}, owner = () => '' } = {}) {
    function loadAll() {
      try {
        const items = JSON.parse(storage.getItem(STORAGE_KEY) || '[]');
        // Older entries may hold a placeholder captured before it was filtered.
        return Array.isArray(items) ? items.filter((item) => item && !PLACEHOLDERS.includes(item.text)) : [];
      } catch (_) {
        return [];
      }
    }

    // Entries from before accounts were tagged have no owner.
    function mine(item, me) {
      return item.owner ? item.owner === me : true;
    }

    function load() {
      const me = String(owner() || '');
      return loadAll().filter((item) => mine(item, me));
    }

    function save(items) {
      let kept = items.slice(-MAX_ITEMS);
      // localStorage is shared with Zalo itself; when it is full, drop the
      // oldest entries rather than losing the newest one.
      for (;;) {
        try {
          if (kept.length) storage.setItem(STORAGE_KEY, JSON.stringify(kept));
          else storage.removeItem(STORAGE_KEY);
          break;
        } catch (error) {
          if (kept.length <= 1) throw error;
          kept = kept.slice(Math.ceil(kept.length / 2));
        }
      }
      onWrite();
    }

    // Returns true when a new entry was stored. Zalo converts the same
    // message more than once per recall, so entries are keyed by msgId.
    function remember(msg) {
      if (!msg || !msg.msgId || alreadyRecalled(msg.message)) return false;
      const text = summarize(msg.message);
      const { image, imageFull, video } = media(msg.message);
      if (!text && !image && !video) return false;
      const items = loadAll();
      const msgId = String(msg.msgId);
      if (items.some((item) => item.msgId === msgId)) return false;
      items.push({
        msgId,
        owner: String(owner() || ''),
        fromUid: String(msg.fromUid || ''),
        toUid: String(msg.toUid || msg.idTo || ''),
        name: String(msg.dName || ''),
        text,
        image,
        imageFull,
        video,
        sentAt: Number(msg.sendDttm) || null,
        recalledAt: now()
      });
      save(items);
      return true;
    }

    // Zalo reports the conversation separately, right after converting the
    // message; the sender/receiver pair alone cannot tell groups apart.
    function assignConversation(msgId, conversationId) {
      if (!msgId || !conversationId) return false;
      const items = loadAll();
      const item = items.find((entry) => entry.msgId === String(msgId));
      if (!item || item.conversationId === String(conversationId)) return false;
      item.conversationId = String(conversationId);
      save(items);
      return true;
    }

    function forConversation(conversationId) {
      if (!conversationId) return [];
      return load().filter((item) => item.conversationId === String(conversationId));
    }

    // Removes the current account's entries that match; returns how many.
    function removeMine(test) {
      const me = String(owner() || '');
      const items = loadAll();
      const kept = items.filter((item) => !(mine(item, me) && test(item)));
      if (kept.length !== items.length) save(kept);
      return items.length - kept.length;
    }

    function clearConversation(conversationId) {
      if (!conversationId) return 0;
      return removeMine((item) => item.conversationId === String(conversationId));
    }

    // "Xoá hết" in the panel: only the logged-in account's entries.
    function clear() {
      return removeMine(() => true);
    }

    // Turning the feature off: every account's entries.
    function clearAll() {
      storage.removeItem(STORAGE_KEY);
      onWrite();
    }

    return { load, remember, assignConversation, forConversation, clearConversation, clear, clearAll };
  }

  const api = { STORAGE_KEY, MAX_ITEMS, MAX_TEXT, summarize, imageUrl, createRecallLog };
  // In the page only the global is used; never touch a `module` the page
  // itself may expose.
  if (typeof window === 'object') root.__zaloLinuxRecallLog = api;
  else if (typeof module === 'object' && module.exports) module.exports = api;
})(typeof window === 'object' ? window : globalThis);
