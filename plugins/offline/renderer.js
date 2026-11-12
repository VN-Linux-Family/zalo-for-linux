'use strict';

// Kept independent of webpack module names so the state machine can be tested
// without starting Electron or accessing a real account.
function createOfflineController(env) {
  const { storage, setTimeout, clearTimeout } = env;
  const reportActive = typeof env.reportActive === 'function' ? env.reportActive : () => {};
  function setReportedActive(active) {
    try { reportActive(active); } catch (error) {
      console.warn('[offline] Failed to report Offline state:', error);
    }
  }
  let offline = false;
  let stopped = false;
  let ready = false;
  let recovered = false;
  let refreshing = false;
  let generation = 0;
  let retryTimer;
  let request;
  let inFlight;
  let options;

  function networkError(error) {
    // An authentication/server response must never be replaced by old credentials.
    if (error && (error.data && error.data.error_code != null || error.error_code != null)) return false;
    const code = error && error.code || error;
    return ['ERR_NO_NETWORK', 'ERR_INTERNET_DISCONNECTED', 'ERR_NAME_NOT_RESOLVED',
      'ERR_CONNECTION_TIMED_OUT', 'ERR_CONNECTION_TIMED_OUT_SENT', 'ERR_CONNECTION_RESET',
      'ERR_CONNECTION_REFUSED', 'ERR_NETWORK_CHANGED', 'OFFLINE_STARTUP_TIMEOUT'].includes(code) ||
      !!(error && error.status === 0);
  }

  function sessionRejected(error) {
    const code = error && (error.data && error.data.error_code || error.error_code || error.code);
    // Zalo's ERR_SESSION_TIMEOUT, ERR_KICK_OUT and invalid-account responses.
    return [102, 600, 210, 212].includes(code);
  }

  function schedule() {
    clearTimeout(retryTimer);
    if (offline && !stopped && !recovered) retryTimer = setTimeout(retry, 10000);
  }

  function finishRecovery() {
    if (recovered && ready && !stopped && !refreshing) {
      refreshing = true;
      clearTimeout(retryTimer);
      env.showStatus('reconnecting');
      Promise.resolve().then(() => env.reload()).then(started => {
        if (started === false) throw new Error('Reload did not start');
      }).catch(() => {
        if (stopped) return;
        recovered = false;
        env.showStatus('retrying');
        schedule();
      }).finally(() => { refreshing = false; });
    }
  }

  async function onlineRequest() {
    if (inFlight) return inFlight;
    const current = generation;
    let deadline;
    // Some upstream requests wait in the network queue indefinitely. Bound the
    // attempt as well as startup so reconnect polling can make progress.
    inFlight = Promise.race([Promise.resolve().then(request), new Promise((_, reject) => {
      deadline = setTimeout(() => reject({ code: 'OFFLINE_STARTUP_TIMEOUT' }), 15000);
    })]).then(async info => {
      if (current !== generation || stopped) throw { code: 'OFFLINE_CANCELLED' };
      if (!info || !info.config || !info.config.uid) throw new Error('Invalid login information');
      await storage.write({ version: 1, clientId: options.clientId, info }).catch(() => false);
      if (current !== generation || stopped) throw { code: 'OFFLINE_CANCELLED' };
      if (offline) {
        recovered = true;
        finishRecovery();
      }
      return info;
    }).catch(async error => {
      if (current === generation && !stopped && sessionRejected(error)) {
        await invalidate();
        if (offline) env.reload(); // Let the normal login flow handle a rejected session.
      }
      throw error;
    }).finally(() => { clearTimeout(deadline); inFlight = null; });
    return inFlight;
  }

  async function retry() {
    if (!offline || stopped || recovered) return;
    try { await onlineRequest(); } catch (_) { /* Keep local history available. */ }
    schedule();
  }

  async function invalidate() {
    stopped = true;
    generation++;
    clearTimeout(retryTimer);
    setReportedActive(false);
    await storage.clear().catch(() => {});
  }

  async function fetchLoginInfo(makeRequest, settings) {
    request = makeRequest;
    options = settings;
    offline = false;
    setReportedActive(false);
    const current = generation;
    let cached = await storage.read().catch(() => null);
    if (!cached || cached.version !== 1 || cached.clientId !== settings.clientId ||
        !settings.userId || !cached.info || !cached.info.config ||
        String(cached.info.config.uid) !== String(settings.userId)) cached = null;
    if (current !== generation || stopped) throw { code: 'OFFLINE_CANCELLED' };
    if (!cached) return onlineRequest();

    if (env.isOnline()) {
      let timeout;
      try {
        return await Promise.race([onlineRequest(), new Promise((_, reject) => {
          timeout = setTimeout(() => reject({ code: 'OFFLINE_STARTUP_TIMEOUT' }), 8000);
        })]);
      } catch (error) {
        if (!networkError(error) || stopped) throw error;
      } finally { clearTimeout(timeout); }
    }
    offline = true;
    setReportedActive(true);
    settings.onOffline();
    env.showStatus('offline');
    schedule();
    // Keep the server clock offset, rather than setting the clock back to the
    // time of the previous login. The original object remains untouched on disk.
    const info = JSON.parse(JSON.stringify(cached.info));
    const now = env.now();
    info.config.current_time += (now - info.requestStartTime) / 1000;
    info.requestStartTime = now;
    return info;
  }

  env.onOnline(() => { clearTimeout(retryTimer); retry(); });
  env.onUnload(() => {
    stopped = true;
    generation++;
    clearTimeout(retryTimer);
    setReportedActive(false);
  });
  return {
    fetchLoginInfo, invalidate,
    get active() { return offline; },
    markReady() { ready = true; finishRecovery(); }
  };
}

if (typeof module !== 'undefined' && module.exports) module.exports = { createOfflineController };
if (typeof window !== 'undefined' && window.__zaloOfflineBridge &&
    typeof window.__zaloOfflineBridge.request === 'function') {
  const localeMessages = window.__zaloLinuxOfflineMessages || {};
  let currentLocale = null;
  let currentStatus = null;
  let statusBadge = null;
  let unregisterLanguage = null;

  function renderStatus() {
    if (!currentStatus || !currentLocale) return;
    const entry = localeMessages['offline.status.' + currentStatus];
    const message = entry && entry[currentLocale];
    if (typeof message !== 'string') return;
    if (!statusBadge) {
      statusBadge = document.createElement('div');
      statusBadge.id = 'linux-offline-status';
      statusBadge.setAttribute('role', 'status');
      statusBadge.style.cssText = 'position:fixed;bottom:12px;left:80px;z-index:99999;padding:8px 12px;' +
        'border-radius:6px;background:#263445;color:white;font:13px sans-serif;pointer-events:none';
      document.body.appendChild(statusBadge);
    }
    statusBadge.textContent = message;
  }

  function onLanguageUpdateRequest(request) {
    if (!request || request.type !== 'LANGUAGE_UPDATE_REQUEST' ||
        (request.locale !== 'vi' && request.locale !== 'en')) return;
    currentLocale = request.locale;
    renderStatus();
  }

  function startLanguageSupport() {
    const api = window.ZaloLanguage;
    if (!api || typeof api.registerPlugin !== 'function') return false;
    unregisterLanguage = api.registerPlugin({
      id: 'offline',
      onLanguageUpdateRequest
    });
    return true;
  }

  function onLanguageReady() {
    if (startLanguageSupport()) window.removeEventListener('zalo-language:ready', onLanguageReady);
  }

  if (!startLanguageSupport()) window.addEventListener('zalo-language:ready', onLanguageReady);
  window.addEventListener('pagehide', () => {
    window.removeEventListener('zalo-language:ready', onLanguageReady);
    if (typeof unregisterLanguage === 'function') {
      unregisterLanguage();
      unregisterLanguage = null;
    }
  }, { once: true });

  const request = window.__zaloOfflineBridge.request;
  const storage = Object.freeze({
    read: () => request('read'),
    write: record => request('write', record),
    clear: () => request('clear'),
    refresh: () => request('refresh')
  });

  window.__zaloOffline = createOfflineController({
    storage,
    reportActive: active => {
      if (typeof window.__zaloOfflineBridge.reportActive === 'function') {
        window.__zaloOfflineBridge.reportActive(active);
      }
    },
    setTimeout: window.setTimeout.bind(window), clearTimeout: window.clearTimeout.bind(window),
    now: Date.now, isOnline: () => navigator.onLine,
    reload: () => storage.refresh(),
    onOnline: callback => window.addEventListener('online', callback),
    onUnload: callback => window.addEventListener('beforeunload', callback),
    showStatus(status) {
      currentStatus = status;
      renderStatus();
    }
  });
}
