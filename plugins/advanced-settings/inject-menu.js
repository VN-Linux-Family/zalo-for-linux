(function () {
  if (window.__zaloAdvancedMenuInstalled) return;
  window.__zaloAdvancedMenuInstalled = true;

  function openSettings() {
    const previousTitle = document.title;
    document.title = 'LINUX_ADVANCED_SETTINGS_TRIGGER';
    setTimeout(function () { document.title = previousTitle; }, 100);
  }

  let currentLocale = 'vi';
  let unregisterLanguagePlugin = null;
  const dictionary = window.__zaloAdvancedSettingsMessages || {};
  const translate = (key) => { const entry = dictionary['advanced-settings.' + key]; return entry ? entry[currentLocale] : key; };

  function addMenuItem() {
    const existing = document.getElementById('zalo-advanced-setting-item');
    if (existing) {
      const label = existing.querySelector('.setting-menu__name');
      if (label) label.textContent = translate('menu.open');
      existing.title = translate('menu.title');
      return;
    }
    const containers = document.querySelectorAll('#setting .setting-menu');
    if (!containers.length) return;
    const container = containers[containers.length - 1];
    const reference = container.querySelector('.setting-menu__item');
    const item = document.createElement('div');
    item.id = 'zalo-advanced-setting-item';
    item.className = reference ? reference.className : 'setting-menu__item';
    item.setAttribute('role', 'button');
    item.setAttribute('tabindex', '0');
    item.title = translate('menu.title');
    item.innerHTML = '<div class="setting-menu__wrapper-content truncate">'
      + '<div class="setting-menu__icon" style="display:flex;align-items:center;justify-content:center">'
      + '<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">'
      + '<path d="M4 7h16M4 17h16"/><circle cx="9" cy="7" r="3"/><circle cx="15" cy="17" r="3"/>'
      + '</svg></div><p class="setting-menu__name truncate">' + translate('menu.open') + '</p></div>';
    item.addEventListener('click', openSettings);
    item.addEventListener('keydown', function (event) {
      if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); openSettings(); }
    });
    container.appendChild(item);
  }

  function connectLanguageCore() {
    const core = window.ZaloLanguage;
    if (!core || typeof core.registerPlugin !== 'function' || unregisterLanguagePlugin) return;
    try {
      unregisterLanguagePlugin = core.registerPlugin({
        id: 'advanced-settings',
        onLanguageUpdateRequest(request) {
          if (!request || request.type !== 'LANGUAGE_UPDATE_REQUEST' ||
              (request.locale !== 'vi' && request.locale !== 'en')) return;
          currentLocale = request.locale;
          addMenuItem();
        }
      });
    } catch (error) {
      console.warn('[advanced-settings] Language Core registration failed:', error);
    }
  }

  addMenuItem();
  connectLanguageCore();
  window.addEventListener('zalo-language:ready', connectLanguageCore);
  let updateQueued = false;
  function scheduleUpdate() {
    if (updateQueued) return;
    updateQueued = true;
    requestAnimationFrame(function () {
      updateQueued = false;
      addMenuItem();
    });
  }
  const observer = new MutationObserver(function (mutations) {
    for (const mutation of mutations) {
      const target = mutation.target.nodeType === 1 ? mutation.target : mutation.target.parentElement;
      if (target && target.closest && target.closest('#setting .setting-menu')) {
        scheduleUpdate();
        return;
      }
      for (const node of mutation.addedNodes) {
        if (node.nodeType !== 1) continue;
        if ((node.matches && node.matches('#setting, #setting .setting-menu')) ||
            (node.querySelector && node.querySelector('#setting .setting-menu'))) {
          scheduleUpdate();
          return;
        }
      }
    }
  });
  observer.observe(document.documentElement, { childList: true, subtree: true });
  window.addEventListener('pagehide', function cleanup() {
    observer.disconnect();
    window.removeEventListener('zalo-language:ready', connectLanguageCore);
    if (unregisterLanguagePlugin) unregisterLanguagePlugin();
  }, { once: true });
})();
