(function installZaloLanguageCore(global) {
  'use strict';
  if (!global || !global.document) return;
  if (Object.prototype.hasOwnProperty.call(global, 'ZaloLanguage')) {
    console.warn('[ZaloLanguage] A global named ZaloLanguage already exists; bridge not installed.');
    return;
  }
  var LOCALES = ['vi', 'en'];
  var PLUGIN_ID_PATTERN = /^[a-z0-9][a-z0-9-]*$/;
  var registry = new Map();
  var currentLocale = null;
  var sourceMode = 'unavailable';
  var bridgeKind = 'unavailable';
  var sourceObject = null;
  var sourceCallback = null;
  var bridgeListener = null;
  var latestBridgeTimestamp = 0;
  var requestSequence = 0;
  var lastRequestId = null;
  var dispatchErrors = 0;
  var dispatchCount = 0;
  var destroyed = false;

  function isLocale(value) { return LOCALES.indexOf(value) !== -1; }
  function normalizeSource(candidate) {
    if (!candidate || (typeof candidate !== 'object' && typeof candidate !== 'function')) return null;
    var value = candidate.default && typeof candidate.default === 'object' ? candidate.default : candidate;
    return typeof value.getCurrentLanguageName === 'function' &&
      typeof value.currentLanguage === 'function' &&
      typeof value.callUpdate === 'function' &&
      typeof value.removeUpdate === 'function' ? value : null;
  }
  // Zalo's getter maps every unknown index (including its startup sentinel -1)
  // to Vietnamese. Validate the underlying enum before trusting that getter.
  // In the source bundle LANG_INDEX_VI=0 and LANG_INDEX_EN=1.
  function readSourceLocale(source) {
    var index = source.currentLanguage();
    if (index !== 0 && index !== 1) return null;
    var locale = source.getCurrentLanguageName();
    return isLocale(locale) ? locale : null;
  }
  // Read-only discovery: inspect only Webpack caches already exposed by Zalo.
  // Never push chunks, invoke module loaders, patch functions, or write Zalo state.
  function discoverLanguageSource() {
    var direct = normalizeSource(global.ZaloLanguageManager) ||
      normalizeSource(global.zaloLanguageManager);
    if (direct) return direct;
    var runtimes = [global.__webpack_require__, global.webpackRequire];
    var matches = [];
    for (var r = 0; r < runtimes.length; r++) {
      var runtime = runtimes[r];
      if (!runtime || !runtime.c || typeof runtime.c !== 'object') continue;
      var ids = Object.keys(runtime.c);
      for (var i = 0; i < ids.length; i++) {
        var record = runtime.c[ids[i]];
        var exports = record && record.exports;
        var source = normalizeSource(exports) || normalizeSource(exports && exports.default);
        if (source && matches.indexOf(source) === -1) matches.push(source);
      }
    }
    return matches.length === 1 ? matches[0] : null;
  }
  function normalizeBridgeState(value) {
    if (!value || value.version !== 1 || !isLocale(value.locale) ||
        !Number.isFinite(value.timestamp) || value.timestamp < latestBridgeTimestamp) return null;
    latestBridgeTimestamp = value.timestamp;
    return value;
  }
  function createRequest(locale, previousLocale, source) {
    requestSequence += 1;
    var now = Date.now();
    return Object.freeze({
      type: 'LANGUAGE_UPDATE_REQUEST',
      requestId: 'zalo-language-' + now + '-' + requestSequence,
      locale: locale,
      previousLocale: previousLocale,
      source: source,
      timestamp: now
    });
  }
  function dispatchToPlugin(plugin, request) {
    if (!plugin || registry.get(plugin.id) !== plugin ||
        plugin.lastRequestId === request.requestId || destroyed) return;
    plugin.lastRequestId = request.requestId;
    dispatchCount += 1;
    try {
      var result = plugin.handler(request);
      if (result && typeof result.then === 'function') {
        Promise.resolve(result).catch(function (error) {
          dispatchErrors += 1;
          console.error('[ZaloLanguage] Plugin "' + plugin.id + '" rejected language request:', error);
        });
      }
    } catch (error) {
      dispatchErrors += 1;
      console.error('[ZaloLanguage] Plugin "' + plugin.id + '" failed language request:', error);
    }
  }
  function broadcast(locale, source, forceInitial) {
    if (!isLocale(locale) || destroyed ||
        (!forceInitial && locale === currentLocale)) return false;
    var previousLocale = currentLocale;
    currentLocale = locale;
    sourceMode = source;
    // The preload exposes a one-way, locale-only IPC capability. The main
    // process independently authenticates the sender and validates vi/en.
    try {
      var transport = global.__zaloLanguageTransport;
      if (transport && typeof transport.publishLocale === 'function') {
        transport.publishLocale(locale);
      }
    } catch (error) {
      console.warn('[ZaloLanguage] Main-process locale transport unavailable:', error);
    }
    var request = createRequest(locale, previousLocale, source);
    lastRequestId = request.requestId;
    // A handler may mutate the registry. Snapshot to keep this dispatch finite
    // and ensure plugins added mid-broadcast wait for their own initial request.
    var recipients = Array.from(registry.values());
    recipients.forEach(function (plugin) {
      if (registry.get(plugin.id) === plugin) dispatchToPlugin(plugin, request);
    });
    return true;
  }
  function onPatchedBridgeChange(event) {
    var snapshot = global.__ZALO_LANGUAGE_BRIDGE__;
    // Zalo Locale Bridge publishes the same object as both snapshot and event.detail.
    // Ignore detached/fabricated event payloads; the page snapshot is authoritative.
    if (event && event.detail && event.detail !== snapshot) return;
    var state = normalizeBridgeState(snapshot);
    if (!state) return;
    bridgeKind = 'zalo-locale-bridge';
    broadcast(state.locale, 'zalo', false);
  }
  function attachPatchedBridge() {
    if (typeof global.addEventListener === 'function') {
      bridgeListener = onPatchedBridgeChange;
      global.addEventListener('zalo-language:changed', bridgeListener);
    }
    var state = normalizeBridgeState(global.__ZALO_LANGUAGE_BRIDGE__);
    if (!state) return false;
    bridgeKind = 'zalo-locale-bridge';
    broadcast(state.locale, 'zalo', true);
    return true;
  }
  function onSourceUpdate() {
    if (!sourceObject || destroyed) return;
    try {
      var locale = readSourceLocale(sourceObject);
      if (locale) broadcast(locale, 'zalo', false);
    } catch (error) {
      console.warn('[ZaloLanguage] Cannot read Zalo locale:', error);
    }
  }
  function attachSource(source) {
    if (!source || source === sourceObject || destroyed) return false;
    sourceObject = source;
    sourceCallback = onSourceUpdate;
    try {
      sourceObject.callUpdate(sourceCallback);
      bridgeKind = 'readonly-singleton';
      var locale = readSourceLocale(sourceObject);
      if (locale) broadcast(locale, 'zalo', currentLocale === null);
      return true;
    } catch (error) {
      try {
        if (sourceObject && sourceCallback) sourceObject.removeUpdate(sourceCallback);
      } catch (_) {}
      sourceObject = null;
      sourceCallback = null;
      bridgeKind = 'unavailable';
      sourceMode = 'unavailable';
      console.warn('[ZaloLanguage] Read-only adapter unavailable:', error);
      return false;
    }
  }
  function registerPlugin(options) {
    if (destroyed) throw new Error('Language Core is shutting down');
    if (!options || typeof options !== 'object') {
      throw new TypeError('Plugin registration options are required');
    }
    var id = options.id;
    if (typeof id !== 'string' || !PLUGIN_ID_PATTERN.test(id)) {
      throw new Error('Plugin id must use lowercase letters, digits, and hyphens');
    }
    if (registry.has(id)) throw new Error('Plugin "' + id + '" is already registered');
    if (typeof options.onLanguageUpdateRequest !== 'function') {
      throw new TypeError('onLanguageUpdateRequest must be a function');
    }
    var plugin = {
      id: id,
      handler: options.onLanguageUpdateRequest,
      lastRequestId: null
    };
    registry.set(id, plugin);
    if (currentLocale) {
      dispatchToPlugin(plugin, createRequest(currentLocale, null, 'initial'));
    }
    var active = true;
    return function unregisterPlugin() {
      if (!active) return false;
      active = false;
      if (registry.get(id) !== plugin) return false;
      return registry.delete(id);
    };
  }
  function getStatus() {
    return Object.freeze({
      ready: currentLocale !== null,
      locale: currentLocale,
      source: sourceMode,
      bridge: bridgeKind,
      registeredPlugins: registry.size,
      lastRequestId: lastRequestId,
      dispatchedRequests: dispatchCount,
      dispatchErrors: dispatchErrors
    });
  }
  var api = Object.freeze({
    getLocale: function () { return currentLocale; },
    getStatus: getStatus,
    registerPlugin: registerPlugin
  });
  Object.defineProperty(global, 'ZaloLanguage', {
    configurable: false,
    enumerable: true,
    writable: false,
    value: api
  });
  function cleanup() {
    if (destroyed) return;
    destroyed = true;
    if (sourceObject && sourceCallback) {
      try { sourceObject.removeUpdate(sourceCallback); } catch (_) {}
    }
    if (bridgeListener && global.removeEventListener) {
      global.removeEventListener('zalo-language:changed', bridgeListener);
    }
    registry.clear();
  }
  var hasPatchedBridge = attachPatchedBridge();
  if (!hasPatchedBridge) {
    var source = discoverLanguageSource();
    if (source) attachSource(source);
  }
  // Let same-renderer plugins register after the Core API and initial locale
  // snapshot are available. This is a readiness signal, not a locale source.
  if (typeof global.dispatchEvent === 'function' && typeof global.Event === 'function') {
    global.dispatchEvent(new global.Event('zalo-language:ready'));
  }
  // Never infer locale from HTML lang: it is not authoritative in Zalo.
  if (global.addEventListener) global.addEventListener('pagehide', cleanup, { once: true });
})(window);
