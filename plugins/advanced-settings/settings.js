(() => {
  'use strict';

  let currentLocale = 'vi';
  let preview = null;
  let previewButton = null;
  let previewRequest = 0;
  let devModeState = null;
  let devModeStatusKind = null;

  function renderDevModeStatus() {
    const status = document.getElementById('dev-mode-status');
    if (!status || !devModeState || !devModeStatusKind) return;
    const key = devModeStatusKind === 'error'
      ? 'errorDevModeValue'
      : (devModeState.devModeForced ? 'devModeForced' :
        (devModeState.devModeEnabled ? 'devModeEnabled' : 'devModeDisabled'));
    status.textContent = tr(key);
  }
  const tr = (key) => { const entry = window.linuxSettings.localeMessages['advanced-settings.' + key]; return entry ? entry[currentLocale] : key; };
  function applyLocale(locale) {
    if (locale !== 'vi' && locale !== 'en') return;
    currentLocale = locale;
    const set = (selector, key) => {
      const el = document.querySelector(selector);
      if (el) el.textContent = tr(key);
    };
    document.documentElement.lang = locale;
    document.title = tr('title');
    set('#advanced-title', 'title');
    set('.settings-heading > p', 'tagline');
    set('#sound-title', 'soundTitle');
    set('#sound-title + .section-description', 'soundDescription');
    set('label[for="ringtone-path"]', 'ringtone');
    set('#ringtone-description', 'ringtoneDescription');
    set('label[for="ringback-path"]', 'ringback');
    set('#ringback-description', 'ringbackDescription');
    document.querySelectorAll('[data-action="browse"]').forEach((el) => { el.textContent = tr('browse'); });
    document.querySelectorAll('[data-action="preview"]').forEach((el) => { el.textContent = tr('preview'); });
    if (previewButton) previewButton.textContent = tr('stopPreview');
    document.querySelectorAll('[data-action="reset"]').forEach((el) => { el.textContent = tr('reset'); });
    document.querySelectorAll('button[type="submit"]').forEach((el) => { el.textContent = tr('save'); });
    set('.sound-hint', 'soundHint');
    set('#screenshot-title', 'screenshotTitle');
    const descriptions = document.querySelectorAll('#screenshot-title ~ .section-description');
    if (descriptions[0]) descriptions[0].textContent = tr('screenshotDescription');
    set('label[for="delay-with-zalo"]', 'withZalo');
    set('#delay-with-zalo-desc', 'withZaloDescription');
    set('label[for="delay-without-zalo"]', 'withoutZalo');
    set('#delay-without-zalo-desc', 'withoutZaloDescription');
    const hints = document.querySelectorAll('.sound-hint');
    if (hints[1]) hints[1].textContent = tr('delayHint');
    set('#recalled-title', 'recalledTitle');
    set('#recalled-description', 'recalledDescription');
    set('#recalled-hint', 'recalledHint');
    set('.userscripts-description p', 'userscriptsDescription');
    const openManager = document.querySelector('#open-userscripts');
    if (openManager && openManager.firstChild) openManager.firstChild.textContent = tr('openManager') + ' ';
    else if (openManager) openManager.textContent = tr('openManager');
    document.querySelectorAll('[data-i18n]').forEach((el) => {
      const key = el.getAttribute('data-i18n');
      const owner = el.closest('[data-i18n-plugin]')?.getAttribute('data-i18n-plugin');
      el.textContent = owner === 'userscripts' ? window.userscripts.translate(key, currentLocale) : tr(key);
    });
    document.querySelectorAll('[data-i18n-placeholder]').forEach((el) => {
      const owner = el.closest('[data-i18n-plugin]')?.getAttribute('data-i18n-plugin');
      const key = el.getAttribute('data-i18n-placeholder');
      el.placeholder = owner === 'userscripts' ? window.userscripts.translate(key, currentLocale) : tr(key);
    });
    renderDevModeStatus();
    document.querySelectorAll('[data-i18n-aria-label]').forEach((el) => {
      const owner = el.closest('[data-i18n-plugin]')?.getAttribute('data-i18n-plugin');
      const key = el.getAttribute('data-i18n-aria-label');
      el.setAttribute('aria-label', owner === 'userscripts' ? window.userscripts.translate(key, currentLocale) : tr(key));
    });
  }

  const advanced = document.getElementById('advanced-view');
  if (window.linuxSettings && typeof window.linuxSettings.onLanguageUpdate === 'function') {
    window.linuxSettings.onLanguageUpdate(applyLocale);
  }
  applyLocale('vi');
  const manager = document.getElementById('userscripts-view');
  const openButton = document.getElementById('open-userscripts');
  const backButton = document.getElementById('back-to-advanced');
  const forms = [...document.querySelectorAll('[data-sound]')];
  const delayForms = [...document.querySelectorAll('[data-screenshot-delay]')];
  function message(form, text, error = false) {
    const status = form.querySelector('.sound-status, .screenshot-status');
    if (!status) return;
    status.textContent = text;
    status.classList.toggle('error', error);
  }

  function errorMessage(error) {
    const message = String(error.message || error).replace(/^Error invoking remote method '[^']+': Error:\s*/, '');
    const code = error.code || message;
    const keys = {
      ADVANCED_SETTINGS_INVALID_SOUND_KIND: 'errorSoundKind',
      ADVANCED_SETTINGS_INVALID_DELAY: 'errorDelayInvalid',
      ADVANCED_SETTINGS_NEGATIVE_DELAY: 'errorDelayNegative',
      ADVANCED_SETTINGS_MAX_DELAY_EXCEEDED: 'errorDelayMax',
      ADVANCED_SETTINGS_INVALID_PATH: 'errorPathInvalid',
      ADVANCED_SETTINGS_ABSOLUTE_PATH_REQUIRED: 'errorPathAbsolute',
      ADVANCED_SETTINGS_UNSUPPORTED_AUDIO_FORMAT: 'errorAudioFormat',
      ADVANCED_SETTINGS_AUDIO_FILE_UNREADABLE: 'errorAudioRead',
      ADVANCED_SETTINGS_INVALID_VALUE: 'errorValueInvalid',
      ADVANCED_SETTINGS_INVALID_SCREENSHOT_KIND: 'errorScreenshotKind',
      ADVANCED_SETTINGS_INVALID_DEV_MODE_VALUE: 'errorDevModeValue'
    };
    return keys[code] ? tr(keys[code]) : message;
  }

  function stopPreview() {
    previewRequest++;
    if (preview) { preview.pause(); preview = null; }
    if (previewButton) {
      previewButton.textContent = tr('preview');
      previewButton.setAttribute('aria-pressed', 'false');
      previewButton = null;
    }
  }

  function showManager(show) {
    stopPreview();
    document.body.classList.toggle('show-userscripts', show);
    advanced.inert = show;
    manager.inert = !show;
    advanced.setAttribute('aria-hidden', String(show));
    manager.setAttribute('aria-hidden', String(!show));
    (show ? backButton : openButton).focus();
    document.title = show ? tr('managerPageTitle') : tr('title');
  }
  openButton.addEventListener('click', () => showManager(true));
  backButton.addEventListener('click', () => showManager(false));

  for (const form of forms) {
    const kind = form.dataset.sound;
    const input = form.elements.path;
    const browse = form.querySelector('[data-action="browse"]');
    const reset = form.querySelector('[data-action="reset"]');
    const listen = form.querySelector('[data-action="preview"]');
    listen.setAttribute('aria-pressed', 'false');

    async function save(value) {
      stopPreview();
      const saved = await window.linuxSettings.saveSound(kind, value);
      input.value = saved[kind];
      message(form, value ? tr('saved') : tr('resetSound'));
    }

    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      try { await save(input.value); }
      catch (error) { message(form, errorMessage(error), true); }
    });
    input.addEventListener('input', () => { stopPreview(); message(form, tr('unsaved')); });
    browse.addEventListener('click', async () => {
      browse.disabled = true;
      try {
        const selected = await window.linuxSettings.pickSound(kind);
        if (selected) {
          stopPreview();
          input.value = selected;
          message(form, tr('picked'));
        }
      } catch (error) { message(form, errorMessage(error), true); }
      finally { browse.disabled = false; }
    });
    reset.addEventListener('click', async () => {
      try { await save(''); }
      catch (error) { message(form, errorMessage(error), true); }
    });
    listen.addEventListener('click', async () => {
      const wasPlaying = previewButton === listen;
      stopPreview();
      if (wasPlaying) return;
      const request = previewRequest;
      try {
        const customUrl = await window.linuxSettings.previewSound(kind, input.value);
        if (request !== previewRequest) return;
        const sound = new Audio(customUrl);
        preview = sound;
        previewButton = listen;
        listen.textContent = tr('stopPreview');
        listen.setAttribute('aria-pressed', 'true');
        sound.addEventListener('ended', () => { if (preview === sound) stopPreview(); });
        sound.addEventListener('error', () => {
          if (preview !== sound) return;
          stopPreview();
          message(form, tr('previewError'), true);
        });
        await sound.play();
      } catch (error) {
        if (request !== previewRequest) return;
        stopPreview();
        message(form, errorMessage(error), true);
      }
    });
  }

  for (const form of delayForms) {
    const kind = form.dataset.screenshotDelay;
    const input = form.elements.delay;
    const reset = form.querySelector('[data-action="reset"]');
    const defaultVal = kind === 'withZalo' ? 100 : 500;

    async function save(value) {
      const saved = await window.linuxSettings.saveScreenshotDelay(kind, value);
      const key = kind === 'withZalo' ? 'screenshotDelayWithZalo' : 'screenshotDelayWithoutZalo';
      input.value = saved[key] !== undefined ? saved[key] : defaultVal;
      message(form, value !== '' ? tr('delaySaved') : tr('delayReset'));
    }

    form.addEventListener('submit', async (event) => {
      event.preventDefault();
      try {
        await save(input.value);
      } catch (error) {
        message(form, errorMessage(error), true);
      }
    });

    input.addEventListener('input', () => {
      message(form, tr('unsaved'));
    });

    reset.addEventListener('click', async () => {
      try {
        await save('');
      } catch (error) {
        message(form, errorMessage(error), true);
      }
    });
  }

  const keepRecalled = document.getElementById('keep-recalled');
  const recalledStatus = document.getElementById('recalled-status');
  function recalledMessage(text, error = false) {
    recalledStatus.textContent = text;
    recalledStatus.classList.toggle('error', error);
  }
  keepRecalled.disabled = true;
  keepRecalled.addEventListener('change', async () => {
    const enabled = keepRecalled.checked;
    if (!enabled && !window.confirm(tr('recalledConfirm'))) {
      keepRecalled.checked = true;
      return;
    }
    keepRecalled.disabled = true;
    try {
      const saved = await window.linuxSettings.setKeepRecalled(enabled);
      keepRecalled.checked = saved.keepRecalled;
      recalledMessage(saved.keepRecalled ? tr('recalledOn') : tr('recalledOff'));
    } catch (error) {
      keepRecalled.checked = !enabled;
      recalledMessage(errorMessage(error), true);
    } finally {
      keepRecalled.disabled = false;
    }
  });
  const devMode = document.getElementById('dev-mode');
  const devModeStatus = document.getElementById('dev-mode-status');
  devMode.disabled = true;
  devMode.addEventListener('change', async () => {
    const enabled = devMode.checked;
    devMode.disabled = true;
    try {
      const saved = await window.linuxSettings.setDevMode(enabled);
      devModeState = saved;
      devModeStatusKind = 'updated';
      devMode.checked = saved.devModeEnabled;
      devMode.disabled = saved.devModeForced === true;
      renderDevModeStatus();
      devModeStatus.classList.remove('error');
    } catch (error) {
      devMode.checked = !enabled;
      devMode.disabled = false;
      devModeStatusKind = 'error';
      renderDevModeStatus();
      devModeStatus.classList.add('error');
    }
  });

  window.addEventListener('pagehide', stopPreview);
  window.linuxSettings.get().then((settings) => {
    applyLocale(settings.locale || 'vi');
    for (const form of forms) form.elements.path.value = settings[form.dataset.sound] || '';
    for (const form of delayForms) {
      const kind = form.dataset.screenshotDelay;
      const key = kind === 'withZalo' ? 'screenshotDelayWithZalo' : 'screenshotDelayWithoutZalo';
      const fallback = kind === 'withZalo' ? 100 : 500;
      form.elements.delay.value = typeof settings[key] === 'number' ? settings[key] : fallback;
    }
    keepRecalled.checked = settings.keepRecalled === true;
    keepRecalled.disabled = false;
    devModeState = settings;
    devModeStatusKind = 'state';
    devMode.checked = settings.devModeEnabled === true;
    devMode.disabled = settings.devModeForced === true;
    renderDevModeStatus();
  }).catch((error) => {
    for (const form of forms) message(form, errorMessage(error), true);
    for (const form of delayForms) message(form, errorMessage(error), true);
    recalledMessage(errorMessage(error), true);
  });
})();
