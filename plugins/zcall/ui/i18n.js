'use strict';

(function installZCallTranslations(global) {
  // Renderer process cwd is changed by main.js to app/, so a bare
  // require('./locale.json') resolves from the wrong directory and aborts
  // both call UIs before they can register their state handlers. Resolve the
  // plugin dictionary beside this HTML page instead.
  const fs = require('fs');
  const { fileURLToPath } = require('url');
  const dictionaryPath = fileURLToPath(new URL('./locale.json', global.location.href));
  const dictionary = JSON.parse(fs.readFileSync(dictionaryPath, 'utf8')).messages;
  let locale = 'vi';
  function t(key, params = {}) {
    const entry = dictionary['zcall.' + key];
    const template = entry ? entry[locale] : key;
    return String(template).replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (_match, name) => {
      const value = params && params[name];
      if (Array.isArray(value)) {
        return value.map((item) => item === undefined || item === null || item === '' ? t('member') : String(item)).join(', ');
      }
      return value === undefined || value === null ? '' : String(value);
    });
  }
  function apply(next) {
    if (next !== 'vi' && next !== 'en') return false;
    locale = next;
    document.documentElement.lang = next;
    return true;
  }
  global.zcallI18n = Object.freeze({ t, apply, getLocale: () => locale });
})(window);
