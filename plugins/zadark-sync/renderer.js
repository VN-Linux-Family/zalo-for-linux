/**
 * ZaDark <-> Zalo Two-Way Theme Synchronization
 * Injected into the renderer process when ZaDark is active.
 *
 * Synchronizes theme settings bidirectionally:
 * - ZaDark (Sáng / Tối / Tự động) -> Zalo (Sáng / Tối / Hệ thống)
 * - Zalo (Sáng / Tối / Hệ thống) -> ZaDark (Sáng / Tối / Tự động)
 *
 * Runs entirely in memory via Webpack runtime hooking, without modifying
 * any vendor bundles on disk.
 */
(function() {
  if (window.__zadarkThemeSyncInstalled) return;
  window.__zadarkThemeSyncInstalled = true;

  const ZADARK_KEY = '@ZaDark:THEME';
  const ZALO_KEY = 'za_theme';

  let isSyncing = false;
  let themeManagerInstance = null;
  let themeConstants = null;
  let themeEmitterInstance = null;

  const origSetItem = localStorage.setItem.bind(localStorage);

  function mapZaDarkToZalo(zadarkTheme) {
    switch (zadarkTheme) {
      case 'light': return 0; // THEME_SETTING_OPT.LIGHT
      case 'dark': return 1;  // THEME_SETTING_OPT.DARK
      case 'auto': return 2;  // THEME_SETTING_OPT.SYSTEM
      default: return 1;
    }
  }

  function mapZaloToZaDark(zaloOpt) {
    switch (zaloOpt) {
      case 0: return 'light';
      case 1: return 'dark';
      case 2: return 'auto';
      default: return 'dark';
    }
  }

  function updateZaDarkUI(targetTheme) {
    try {
      const isSystemDark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
      const effectiveTheme = targetTheme === 'auto' ? (isSystemDark ? 'dark' : 'light') : targetTheme;
      document.documentElement.setAttribute('data-zadark-theme', effectiveTheme);

      const radio = document.querySelector('#js-radio-input-theme input[value="' + targetTheme + '"]');
      if (radio && !radio.checked) {
        radio.checked = true;
      }
    } catch (_) {}
  }

  function syncZaDarkToZalo(zadarkTheme) {
    if (isSyncing) return;
    const targetZaloOpt = mapZaDarkToZalo(zadarkTheme);
    isSyncing = true;
    try {
      if (themeManagerInstance && typeof themeManagerInstance.updateThemeSetting === 'function') {
        const cur = themeManagerInstance.getThemeSetting();
        if (!cur || cur.theme_setting !== targetZaloOpt) {
          themeManagerInstance.updateThemeSetting(targetZaloOpt);
        }
      } else {
        const confStr = localStorage.getItem(ZALO_KEY);
        let conf = confStr ? JSON.parse(confStr) : {};
        if (conf.theme_setting !== targetZaloOpt) {
          conf.theme_setting = targetZaloOpt;
          const isDark = window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches;
          conf.theme = targetZaloOpt === 0 ? 'light' : (targetZaloOpt === 1 ? 'dark' : (isDark ? 'dark' : 'light'));
          origSetItem(ZALO_KEY, JSON.stringify(conf));
        }
      }
    } catch (e) {
      console.warn('[ZaDark Theme Sync] Error syncing to Zalo:', e);
    } finally {
      isSyncing = false;
    }
  }

  function syncZaloToZaDark(zaloOpt) {
    if (isSyncing || typeof zaloOpt === 'undefined') return;
    const targetZaDark = mapZaloToZaDark(zaloOpt);
    const curZaDark = localStorage.getItem(ZADARK_KEY);
    if (curZaDark === targetZaDark) return;

    isSyncing = true;
    try {
      origSetItem(ZADARK_KEY, targetZaDark);
      updateZaDarkUI(targetZaDark);
    } catch (e) {
      console.warn('[ZaDark Theme Sync] Error syncing to ZaDark:', e);
    } finally {
      isSyncing = false;
    }
  }

  // 1. Intercept localStorage.setItem to catch any ZaDark or direct Zalo storage theme change
  localStorage.setItem = function(key, value) {
    origSetItem(key, value);
    if (key === ZADARK_KEY && !isSyncing) {
      syncZaDarkToZalo(value);
    } else if (key === ZALO_KEY && !isSyncing) {
      try {
        const conf = JSON.parse(value);
        if (conf && typeof conf.theme_setting === 'number') {
          syncZaloToZaDark(conf.theme_setting);
        }
      } catch (_) {}
    }
  };

  // 2. Listen to change events on radio inputs in ZaDark menu
  document.addEventListener('change', function(e) {
    if (e.target && e.target.name === 'theme' && e.target.closest('#js-radio-input-theme')) {
      if (!isSyncing) syncZaDarkToZalo(e.target.value);
    }
  }, true);

  // 3. Align initial theme
  function alignInitialTheme() {
    const zadarkTheme = localStorage.getItem(ZADARK_KEY);
    const zaloConfStr = localStorage.getItem(ZALO_KEY);
    let zaloSetting = null;
    if (zaloConfStr) {
      try { zaloSetting = JSON.parse(zaloConfStr).theme_setting; } catch (_) {}
    }

    if (zadarkTheme) {
      const targetZaloOpt = mapZaDarkToZalo(zadarkTheme);
      if (themeManagerInstance && typeof themeManagerInstance.updateThemeSetting === 'function') {
        const cur = themeManagerInstance.getThemeSetting();
        if (!cur || cur.theme_setting !== targetZaloOpt) {
          isSyncing = true;
          try {
            themeManagerInstance.updateThemeSetting(targetZaloOpt);
          } finally {
            isSyncing = false;
          }
        }
      }
    } else if (typeof zaloSetting === 'number') {
      const targetZaDark = mapZaloToZaDark(zaloSetting);
      origSetItem(ZADARK_KEY, targetZaDark);
      updateZaDarkUI(targetZaDark);
    }
  }

  // 4. Hook Webpack to get Zalo theme modules dynamically
  function setupThemeManager(req) {
    try {
      themeManagerInstance = req('iwo6').a;
      themeEmitterInstance = req('vtBH').b;
      themeConstants = req('TmDO');

      if (themeEmitterInstance && themeConstants && themeConstants.THEME_LISTENER_EVENTS) {
        themeEmitterInstance.on(themeConstants.THEME_LISTENER_EVENTS.SETTING_UPDATED, function(newConf) {
          if (newConf && typeof newConf.theme_setting !== 'undefined') {
            syncZaloToZaDark(newConf.theme_setting);
          }
        });
      }

      alignInitialTheme();
      return true;
    } catch (_) {
      return false;
    }
  }

  function hookWebpack() {
    if (window.__webpack_require__ && setupThemeManager(window.__webpack_require__)) {
      return;
    }

    let hooked = false;
    const tryChunk = () => {
      if (hooked) return true;
      if (typeof window.webpackJsonp !== 'undefined' && Array.isArray(window.webpackJsonp)) {
        try {
          window.webpackJsonp.push([
            ['__zadark_sync_hook__'],
            {
              '__zadark_sync_hook__': function(module, exports, req) {
                window.__webpack_require__ = req;
                if (setupThemeManager(req)) {
                  hooked = true;
                }
              }
            },
            [['__zadark_sync_hook__']]
          ]);
          return hooked;
        } catch (e) {
          console.warn('[ZaDark Theme Sync] webpackJsonp push error:', e);
        }
      }
      return false;
    };

    if (!tryChunk()) {
      let count = 0;
      const timer = setInterval(() => {
        count++;
        if (tryChunk() || count > 50) {
          clearInterval(timer);
        }
      }, 100);
    }
  }

  hookWebpack();

  // 5. Update ZaDark radio button when ZaDark popup is rendered into DOM
  const observer = new MutationObserver(function() {
    const radioContainer = document.querySelector('#js-radio-input-theme');
    if (radioContainer) {
      const current = localStorage.getItem(ZADARK_KEY);
      if (current) {
        const radio = radioContainer.querySelector('input[value="' + current + '"]');
        if (radio && !radio.checked) {
          radio.checked = true;
        }
      }
    }
  });
  observer.observe(document.body || document.documentElement, { childList: true, subtree: true });

  // 6. Sync system dark/light changes when in auto/system mode
  if (window.matchMedia) {
    window.matchMedia('(prefers-color-scheme: dark)').addListener(function(event) {
      const zadarkTheme = localStorage.getItem(ZADARK_KEY);
      if (zadarkTheme === 'auto') {
        document.documentElement.setAttribute('data-zadark-theme', event.matches ? 'dark' : 'light');
      }
    });
  }

  window.__zadarkThemeSync = {
    syncZaDarkToZalo,
    syncZaloToZaDark,
    mapZaDarkToZalo,
    mapZaloToZaDark,
    updateZaDarkUI
  };
})();
