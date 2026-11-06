// Injected into the main Zalo page after log.js when "Keep recalled messages"
// is enabled. window.__zaloLinuxRecalled.stop() undoes everything.
(function () {
  'use strict';

  if (window.__zaloLinuxRecalled) return;
  const { createRecallLog } = window.__zaloLinuxRecallLog;
  const log = createRecallLog(window.localStorage);

  let unseen = 0;
  let open = false;
  const hooked = [];
  let view = 'conversation';
  let lastConversation = '';
  let stopped = false;
  const timers = [];

  // Zalo replaces its local copy when a recall arrives, so the content has to
  // be copied when Zalo converts the message. Webpack module ids change between
  // builds, so the module is looked up by its function name.
  function getRequire() {
    const chunks = window.webpackJsonp;
    if (!chunks) return null;
    let req = null;
    const id = 'zaloLinuxRecalled' + Date.now();
    chunks.push([[id], { [id]: (_module, _exports, r) => { req = r; } }, [[id]]]);
    return req;
  }

  function findModule(req, test) {
    for (const mod of Object.values(req.c || {})) {
      try {
        const holder = mod && mod.exports && mod.exports.default;
        if (holder && test(holder)) return holder;
      } catch (_) { /* Some exports throw on access. */ }
    }
    return null;
  }

  function wrap(target, name, before) {
    const original = target[name];
    const wrapper = function (...args) {
      try {
        if (!stopped) before(...args);
      } catch (error) {
        console.error('[recalled-messages]', error);
      }
      return original.apply(this, args);
    };
    target[name] = wrapper;
    hooked.push({ target, name, original, wrapper });
  }

  let conversations = null;
  function activeConversationId() {
    try {
      const value = conversations && conversations.activeConversationId;
      return typeof value === 'string' ? value : '';
    } catch (_) {
      return '';
    }
  }

  function hook() {
    const req = getRequire();
    const recall = req && findModule(req, (h) => typeof h.convertToRecalled === 'function');
    if (!recall) return false;
    wrap(recall, 'convertToRecalled', (msg) => {
      if (log.remember(msg)) { unseen += 1; render(); }
    });
    // Optional: without these, entries still show under "Tất cả".
    const recallState = findModule(req, (h) => typeof h.setRecallMessage === 'function');
    if (recallState) {
      wrap(recallState, 'setRecallMessage', (info) => {
        if (info && log.assignConversation(info.msgId, info.conversationId)) render();
      });
    }
    conversations = findModule(req, (h) => 'activeConversationId' in h);
    return true;
  }

  const style = document.createElement('style');
  style.textContent = `
    #zalo-linux-recalled-btn { position: fixed; left: 50%; top: 8px; visibility: hidden; transform: translateX(-50%); z-index: 99999;
      height: 24px; padding: 0 12px; border-radius: 12px; border: 1px solid #b8d4ff; cursor: pointer;
      background: #e8f1ff; color: #0058d6; font: 600 12px system-ui, sans-serif;
      box-shadow: 0 1px 3px rgba(0, 0, 0, .12); }
    #zalo-linux-recalled-btn:hover { background: #d6e7ff; }
    #zalo-linux-recalled-btn.has-new { background: #0068ff; border-color: #0068ff; color: #fff; }
    #zalo-linux-recalled-btn[hidden], #zalo-linux-recalled-panel[hidden] { display: none; }
    #zalo-linux-recalled-panel { position: fixed; left: 0; top: 0; z-index: 99999;
      width: 380px; max-height: 70vh; display: flex; flex-direction: column;
      background: #fff; color: #111; border-radius: 10px; overflow: hidden;
      box-shadow: 0 6px 24px rgba(0,0,0,.3); font: 13px system-ui, sans-serif; }
    #zalo-linux-recalled-panel header { display: flex; align-items: center; gap: 8px;
      padding: 10px 12px; border-bottom: 1px solid #e5e5e5; font-weight: 600; }
    #zalo-linux-recalled-panel header span { flex: 1; }
    #zalo-linux-recalled-panel header button { border: 0; background: none; color: #0068ff; cursor: pointer; font: inherit; }
    #zalo-linux-recalled-panel .zlr-tabs { display: flex; gap: 6px; padding: 8px 12px 0; }
    #zalo-linux-recalled-panel .zlr-tabs[hidden] { display: none; }
    #zalo-linux-recalled-panel .zlr-tabs button { border: 1px solid #d6dbe1; border-radius: 12px; padding: 3px 10px;
      background: #fff; color: #333; cursor: pointer; font: 12px system-ui, sans-serif; }
    #zalo-linux-recalled-panel .zlr-tabs button[aria-pressed="true"] { border-color: #b8d4ff; background: #e8f1ff; color: #0058d6; font-weight: 600; }
    #zalo-linux-recalled-panel .zlr-list { overflow-y: auto; padding: 4px 0; }
    #zalo-linux-recalled-panel .zlr-item { display: block; padding: 8px 12px; border-bottom: 1px solid #f0f0f0; }
    #zalo-linux-recalled-panel .zlr-meta { color: #666; font-size: 12px; margin-bottom: 3px; }
    #zalo-linux-recalled-panel .zlr-text { white-space: pre-wrap; word-break: break-word; }
    #zalo-linux-recalled-panel .zlr-empty { padding: 16px 12px; color: #666; }
    #zalo-linux-recalled-panel .zlr-item .zlr-empty { padding: 4px 0; }
    #zalo-linux-recalled-panel .zlr-image { display: block; max-width: 120px; max-height: 120px; margin: 4px 0;
      border-radius: 8px; cursor: zoom-in; background: #f0f2f5; }
    #zalo-linux-recalled-viewer { position: fixed; inset: 0; z-index: 100000; display: flex;
      align-items: center; justify-content: center; background: rgba(0, 0, 0, .82); cursor: zoom-out; }
    #zalo-linux-recalled-viewer[hidden] { display: none; }
    #zalo-linux-recalled-viewer img { max-width: 94vw; max-height: 94vh; object-fit: contain;
      border-radius: 4px; box-shadow: 0 8px 40px rgba(0, 0, 0, .5); }
  `;

  const button = document.createElement('button');
  button.id = 'zalo-linux-recalled-btn';
  button.type = 'button';
  button.hidden = true;

  const panel = document.createElement('div');
  panel.id = 'zalo-linux-recalled-panel';
  panel.hidden = true;
  const header = document.createElement('header');
  const title = document.createElement('span');
  title.textContent = 'Tin nhắn đã thu hồi';
  const clearButton = document.createElement('button');
  clearButton.type = 'button';
  clearButton.textContent = 'Xoá hết';
  const closeButton = document.createElement('button');
  closeButton.type = 'button';
  closeButton.textContent = 'Đóng';
  header.append(title, clearButton, closeButton);
  const tabs = document.createElement('div');
  tabs.className = 'zlr-tabs';
  const tabButtons = {};
  for (const [key, label] of [['conversation', 'Cuộc trò chuyện này'], ['all', 'Tất cả']]) {
    const tab = document.createElement('button');
    tab.type = 'button';
    tab.dataset.label = label;
    tab.addEventListener('click', () => { view = key; render(); });
    tabButtons[key] = tab;
    tabs.append(tab);
  }
  const list = document.createElement('div');
  list.className = 'zlr-list';
  panel.append(header, tabs, list);

  // Full-size view of a recalled photo; closes on click or Escape.
  const viewer = document.createElement('div');
  viewer.id = 'zalo-linux-recalled-viewer';
  viewer.hidden = true;
  viewer.setAttribute('role', 'dialog');
  viewer.setAttribute('aria-label', 'Ảnh đã thu hồi');
  const viewerImage = document.createElement('img');
  viewerImage.alt = 'Ảnh đã thu hồi';
  viewerImage.referrerPolicy = 'no-referrer';
  viewer.append(viewerImage);
  function showImage(url) {
    viewerImage.src = url;
    viewer.hidden = false;
  }
  function hideImage() {
    viewer.hidden = true;
    viewerImage.removeAttribute('src');
  }
  viewer.addEventListener('click', hideImage);

  function formatTime(ms) {
    if (!ms) return '?';
    return new Date(ms).toLocaleString('vi-VN', { hour: '2-digit', minute: '2-digit', day: '2-digit', month: '2-digit' });
  }

  function render() {
    const all = log.load();
    const conversationId = activeConversationId();
    const here = log.forConversation(conversationId);
    button.hidden = all.length === 0;
    const count = conversationId ? here.length : all.length;
    button.textContent = unseen > 0 ? `Thu hồi (${unseen} mới)` : `Thu hồi (${count})`;
    button.classList.toggle('has-new', unseen > 0);
    placeButton();
    if (!open) return;
    if (!conversationId) view = 'all';
    tabs.hidden = !conversationId;
    tabButtons.conversation.textContent = `${tabButtons.conversation.dataset.label} (${here.length})`;
    tabButtons.all.textContent = `${tabButtons.all.dataset.label} (${all.length})`;
    for (const [key, tab] of Object.entries(tabButtons)) tab.setAttribute('aria-pressed', String(view === key));
    const items = view === 'conversation' ? here : all;
    list.replaceChildren();
    if (items.length === 0) {
      const empty = document.createElement('div');
      empty.className = 'zlr-empty';
      empty.textContent = view === 'conversation'
        ? 'Cuộc trò chuyện này chưa có tin nào bị thu hồi.'
        : 'Chưa có tin nào bị thu hồi.';
      list.append(empty);
      return;
    }
    for (const item of items.slice().reverse()) {
      const row = document.createElement('div');
      row.className = 'zlr-item';
      const meta = document.createElement('div');
      meta.className = 'zlr-meta';
      meta.textContent = `${item.name || 'Không rõ tên'} · gửi ${formatTime(item.sentAt)} · thu hồi ${formatTime(item.recalledAt)}`;
      row.append(meta);
      if (item.image) {
        const image = document.createElement('img');
        image.className = 'zlr-image';
        image.alt = 'Ảnh đã thu hồi';
        image.loading = 'lazy';
        image.referrerPolicy = 'no-referrer';
        image.src = item.image;
        image.title = 'Bấm để xem ảnh lớn';
        image.addEventListener('click', () => showImage(item.imageFull || item.image));
        image.addEventListener('error', () => {
          const gone = document.createElement('div');
          gone.className = 'zlr-empty';
          gone.textContent = 'Ảnh không còn tải được từ máy chủ Zalo.';
          image.replaceWith(gone);
        });
        row.append(image);
      }
      if (item.text) {
        const text = document.createElement('div');
        text.className = 'zlr-text';
        text.textContent = item.text;
        row.append(text);
      }
      list.append(row);
    }
  }

  // Sit like Zalo's date pill: centred over the message list, just above the
  // composer toolbar.
  function placeButton() {
    if (button.hidden) return;
    const visible = (selector) => [...document.querySelectorAll(selector)]
      .map((el) => el.getBoundingClientRect())
      .filter((rect) => rect.width > 0 && rect.height > 0);
    // Without an open conversation there is no good spot, and a fallback
    // position makes the pill jump once the chat pane appears.
    const listRect = visible('#messageViewContainer')[0];
    button.style.visibility = listRect ? 'visible' : 'hidden';
    if (!listRect) return;
    const tops = visible('.chat-box-input__tools__btn, #chatInput').map((rect) => rect.top);
    const bottom = Math.min(listRect.bottom, ...tops);
    button.style.left = listRect.left + listRect.width / 2 + 'px';
    button.style.top = bottom - button.offsetHeight - 10 + 'px';
  }

  // Open below the button, or above it when there is more room there.
  function placePanel() {
    const rect = button.getBoundingClientRect();
    const width = Math.min(380, window.innerWidth - 16);
    const left = Math.max(8, Math.min(rect.left + rect.width / 2 - width / 2, window.innerWidth - width - 8));
    const below = window.innerHeight - rect.bottom;
    const downward = below >= rect.top;
    panel.style.width = width + 'px';
    panel.style.left = left + 'px';
    panel.style.maxHeight = Math.max(200, Math.max(below, rect.top) - 24) + 'px';
    panel.style.top = downward ? rect.bottom + 8 + 'px' : 'auto';
    panel.style.bottom = downward ? 'auto' : window.innerHeight - rect.top + 8 + 'px';
  }

  function setOpen(value) {
    open = value;
    panel.hidden = !open;
    if (open) { unseen = 0; placePanel(); }
    render();
  }

  button.addEventListener('click', () => setOpen(!open));
  closeButton.addEventListener('click', () => setOpen(false));
  clearButton.addEventListener('click', () => {
    if (!window.confirm('Xoá toàn bộ tin đã lưu?')) return;
    log.clear();
    unseen = 0;
    render();
  });

  // Close like a popover: a click outside the panel or Escape dismisses it.
  function onPointerDown(event) {
    if (open && !panel.contains(event.target) && !button.contains(event.target) &&
        !viewer.contains(event.target)) setOpen(false);
  }
  function onKeyDown(event) {
    if (event.key !== 'Escape') return;
    if (!viewer.hidden) hideImage();
    else if (open) setOpen(false);
  }
  document.addEventListener('pointerdown', onPointerDown, true);
  document.addEventListener('keydown', onKeyDown, true);

  document.head.append(style);
  document.body.append(button, panel, viewer);
  render();

  // The chat pane moves when the window is resized, the conversation list is
  // collapsed or a reply preview opens.
  timers.push(setInterval(() => {
    const conversationId = activeConversationId();
    if (conversationId !== lastConversation) {
      lastConversation = conversationId;
      view = 'conversation';
      render();
    }
    placeButton();
    if (open) placePanel();
  }, 300));

  // Chat modules load lazily after login.
  let tries = 0;
  const hookTimer = setInterval(() => {
    tries += 1;
    if (hook() || tries > 300) clearInterval(hookTimer);
  }, 1000);
  timers.push(hookTimer);

  window.__zaloLinuxRecalled = {
    stop() {
      stopped = true;
      for (const timer of timers) clearInterval(timer);
      document.removeEventListener('pointerdown', onPointerDown, true);
      document.removeEventListener('keydown', onKeyDown, true);
      // Leave a function alone if a userscript wrapped it after us.
      for (const { target, name, original, wrapper } of hooked) {
        if (target[name] === wrapper) target[name] = original;
      }
      style.remove();
      button.remove();
      panel.remove();
      viewer.remove();
      log.clear();
      delete window.__zaloLinuxRecalled;
    }
  };
})();
