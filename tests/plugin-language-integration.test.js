'use strict';

(function testGroup_plugin_language_integration_test_js() {
const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const root = path.join(__dirname, '..');
const read = (file) => fs.readFileSync(path.join(root, file), 'utf8');

describe('Language Core integration for plugin-owned dictionaries', () => {
  it('keeps Language Core as locale source and request broadcaster only', () => {
    const core = read('plugins/language-core/renderer.js');
    assert.match(core, /function registerPlugin\(options\)/);
    assert.match(core, /LANGUAGE_UPDATE_REQUEST/);
    assert.doesNotMatch(core, /function translate\(/);
    assert.doesNotMatch(core, /validateMessages/);
    assert.doesNotMatch(core, /messages: options/);
    assert.match(core, /registerPlugin: registerPlugin/);
  });

  it('keeps Advanced Settings dictionary local and connects its Zalo menu to Core', () => {
    const main = read('plugins/advanced-settings/index.js');
    const preload = read('plugins/advanced-settings/preload.js');
    const renderer = read('plugins/advanced-settings/settings.js');
    const menu = read('plugins/advanced-settings/inject-menu.js');
    const dictionary = JSON.parse(read('plugins/advanced-settings/locale.json'));
    assert.equal(dictionary.plugin, 'advanced-settings');
    assert.equal(dictionary.messages['advanced-settings.menu.open'].en, 'Advanced Settings...');
    assert.equal(dictionary.messages['advanced-settings.devModeTitle'].en, 'Developer Mode');
    assert.equal(dictionary.messages['advanced-settings.devModeForced'].vi, 'Được bật bắt buộc bởi biến môi trường ZALO_DEV_MODE.');
    assert.match(renderer, /renderDevModeStatus\(\)/);
    assert.match(renderer, /'devModeEnabled'/);
    assert.match(renderer, /'devModeDisabled'/);
    assert.match(read('plugins/advanced-settings/index.html'), /id="dev-mode-title" data-i18n="devModeTitle"/);
    assert.match(renderer, /devModeStatusKind = 'state'/);
    assert.match(main, /require\('\.\/locale\.json'\)/);
    assert.match(main, /__zaloAdvancedSettingsMessages/);
    assert.match(preload, /localeMessages/);
    assert.match(renderer, /localeMessages\['advanced-settings\.' \+ key\]/);
    assert.match(renderer, /onLanguageUpdate\(applyLocale\)/);
    assert.match(menu, /id: 'advanced-settings'/);
    assert.match(menu, /zalo-language:ready/);
    assert.doesNotMatch(menu, /messages:/);
  });

  it('keeps the embedded Userscripts Manager on its own locale dictionary', () => {
    const html = read('plugins/advanced-settings/index.html');
    const settings = read('plugins/advanced-settings/settings.js');
    const advanced = JSON.parse(read('plugins/advanced-settings/locale.json'));
    const preload = read('plugins/userscripts/preload.js');
    const manager = read('plugins/userscripts/manager.js');
    const userscriptsMain = read('plugins/userscripts/index.js');
    const userscripts = JSON.parse(read('plugins/userscripts/locale.json'));
    assert.equal(userscripts.plugin, 'userscripts');
    assert.match(html, /id="userscripts-view"[^>]*data-i18n-plugin="userscripts"/);
    assert.match(settings, /window\.userscripts\.translate\(key, currentLocale\)/);
    const advancedHtml = html.slice(0, html.indexOf('<section id=\"userscripts-view\"'));
    for (const match of advancedHtml.matchAll(/data-i18n(?:-placeholder|-aria-label)?=\"([^\"]+)\"/g)) {
      assert.ok(advanced.messages['advanced-settings.' + match[1]], 'missing Advanced Settings key: ' + match[1]);
    }
    assert.match(preload, /require\('\.\/locale\.json'\)/);
    assert.match(manager, /tr\('managerSaved'\)/);
    assert.match(manager, /linuxSettings\.get\(\)/, 'manager should hydrate when opened after a locale change');
    assert.match(manager, /onLanguageUpdate\(applyLocale\)/);
    assert.match(manager, /localeRevision !== requestRevision/);
    assert.match(userscriptsMain, /advancedSettings\.getLocale\(\)/);
    for (const key of Object.keys(userscripts.messages)) {
      assert.ok(key.startsWith('userscripts.'));
      assert.deepEqual(Object.keys(userscripts.messages[key]).sort(), ['en', 'vi']);
    }
    const managerHtml = html.split('<section id="userscripts-view"')[1].split('</section>')[0];
    for (const match of managerHtml.matchAll(/data-i18n(?:-placeholder|-aria-label)?="([^"]+)"/g)) {
      assert.ok(userscripts.messages['userscripts.' + match[1]], 'missing Userscripts key: ' + match[1]);
    }
    assert.ok(advanced.messages['advanced-settings.title']);
  });

  it('keeps Dev Mode on its own dictionary and fans Language Core updates to it', () => {
    const orchestrator = read('plugins/index.js');
    const main = read('main.js');
    const devMode = read('plugins/dev-mode/index.js');
    const dictionary = JSON.parse(read('plugins/dev-mode/locale.json'));
    assert.equal(dictionary.plugin, 'dev-mode');
    assert.equal(dictionary.messages['dev-mode.toggleDevTools'].vi, 'Bật/tắt DevTools');
    assert.equal(dictionary.messages['dev-mode.toggleDevTools'].en, 'Toggle DevTools');
    assert.match(devMode, /onLanguageUpdateRequest\(request\)/);
    assert.match(devMode, /translate\('toggleDevTools'\)/);
    assert.match(orchestrator, /activeDevMode = devMode/);
    assert.match(orchestrator, /\['dev-mode', activeDevMode\]/);
    assert.match(main, /plugins\.updateLanguage\(locale\)[\s\S]{0,100}updateTrayVisibilityMenu\(\)/);
  });

  it('relays locale updates to both ZCall windows without re-rendering the call UI', () => {
    const main = read('main.js');
    const zcall = read('plugins/zcall/index.js');
    const window = read('plugins/zcall/window.js');
    const incoming = read('plugins/zcall/incoming.js');
    const callUi = read('plugins/zcall/ui/call.js');
    const incomingUi = read('plugins/zcall/ui/incoming.js');
    const callHtml = read('plugins/zcall/ui/call.html');
    const incomingHtml = read('plugins/zcall/ui/incoming.html');
    const dictionary = JSON.parse(read('plugins/zcall/ui/locale.json'));
    assert.equal(dictionary.plugin, 'zcall');
    assert.ok(dictionary.messages['zcall.mute']);
    assert.match(main, /zcall\.onLanguageUpdateRequest/);
    assert.match(zcall, /onLanguageUpdateRequest/);
    assert.match(window, /zcall-ui-language/);
    assert.match(incoming, /zcall-ui-language/);
    assert.match(callUi, /window\.zcallI18n\.apply\(locale\)/);
    assert.match(callUi, /function render\(\)/);
    assert.match(callUi, /const tr = \(key, params\) => window\.zcallI18n\.t\(key, params\)/);
    assert.doesNotMatch(callUi, /updateLocaleLabels|localizedStatus|localeState|data-locale-key/);
    assert.doesNotMatch(callUi, /zcall-ui-language.{0,160}render\(\)/s);
    assert.match(incomingUi, /window\.zcallI18n\.apply\(locale\)/);
    assert.match(incomingUi, /function updateIncomingSubtitle\(s\)/);
    assert.match(incomingUi, /currentIncomingState = s/);
    assert.doesNotMatch(incomingUi, /updateIncomingLocaleLabels|renderIncoming/);
    assert.doesNotMatch(incomingUi, /zcall-ui-language.{0,160}renderIncoming\(/s);
    assert.match(callHtml, /i18n\.js/);
    assert.match(incomingHtml, /i18n\.js/);
    for (const source of [callUi, incomingUi]) {
      for (const match of source.matchAll(/tr\('([^']+)'\)/g)) {
        assert.ok(dictionary.messages['zcall.' + match[1]], 'missing ZCall key: ' + match[1]);
      }
    }
    for (const html of [callHtml, incomingHtml]) {
      for (const match of html.matchAll(/data-i18n-key=\"([^\"]+)\"/g)) {
        assert.ok(dictionary.messages['zcall.' + match[1]], 'missing ZCall HTML key: ' + match[1]);
      }
    }
  });

  it('relays plugin-owned Screenshot dictionary without requiring JSON in sandboxed preload', () => {
    const main = read('plugins/index.js');
    const plugin = read('plugins/screenshot/index.js');
    const preload = read('plugins/screenshot/ui/preload.js');
    const renderer = read('plugins/screenshot/ui/screencap.js');
    const html = read('plugins/screenshot/ui/index.html');
    const dictionary = JSON.parse(read('plugins/screenshot/ui/locale.json'));
    assert.equal(dictionary.plugin, 'screenshot');
    assert.ok(dictionary.messages['screenshot.hint']);
    assert.match(main, /activeScreenshot = screenshotPlugin\.register/);
    assert.match(plugin, /onLanguageUpdateRequest\(request\)/);
    assert.match(plugin, /screencap:language/);
    assert.doesNotMatch(preload, /require\(['"]\.\/locale\.json['"]\)/);
    assert.match(plugin, /screenshotLocaleMessages/);
    assert.match(plugin, /messages: screenshotLocaleMessages/);
    assert.match(preload, /onLanguageUpdate/);
    assert.match(renderer, /screenshotMessages/);
    assert.match(renderer, /onLanguageUpdate\(applyScreenshotLocale\)/);
    assert.match(html, /data-i18n-key="hint"/);
    assert.match(renderer, /const baseCanvas = document\.getElementById\('base-canvas'\)/);
  });

  it('keeps seven independent, schema-compatible locale JSON dictionaries', () => {
    for (const [file, plugin] of [
      ['plugins/tray-menu/locale.json', 'tray-menu'],
      ['plugins/dev-mode/locale.json', 'dev-mode'],
      ['plugins/advanced-settings/locale.json', 'advanced-settings'],
      ['plugins/userscripts/locale.json', 'userscripts'],
      ['plugins/zcall/ui/locale.json', 'zcall'],
      ['plugins/screenshot/ui/locale.json', 'screenshot'],
      ['plugins/recalled-messages/locale.json', 'recalled-messages']
    ]) {
      const dictionary = JSON.parse(read(file));
      assert.equal(dictionary.schemaVersion, 1);
      assert.equal(dictionary.plugin, plugin);
      assert.ok(Object.keys(dictionary.messages).length > 0);
      for (const [key, pair] of Object.entries(dictionary.messages)) {
        assert.ok(key.startsWith(plugin + '.'));
        assert.deepEqual(Object.keys(pair).sort(), ['en', 'vi']);
        assert.equal(typeof pair.vi, 'string');
        assert.equal(typeof pair.en, 'string');
      }
    }
  });
});

})();

(function testGroup_tray_menu_language_test_js() {
const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const { buildTrayMenuTemplate } = require('../plugins/tray-menu');

describe('Tray Menu language integration', () => {
  const onToggleVisibility = () => 'toggle';
  const onQuit = () => 'quit';

  it('translates only tray-owned labels for Vietnamese and English', () => {
    const vi = buildTrayMenuTemplate({ locale: 'vi', visible: true, onToggleVisibility, onQuit });
    const en = buildTrayMenuTemplate({ locale: 'en', visible: false, onToggleVisibility, onQuit });
    assert.deepEqual(vi.map(item => item.label), ['Ẩn Zalo', 'Thoát']);
    assert.deepEqual(en.map(item => item.label), ['Open Zalo', 'Quit']);
  });

  it('preserves the original Vietnamese labels until Zalo publishes a locale', () => {
    const menu = buildTrayMenuTemplate({ locale: null, visible: false, onToggleVisibility, onQuit });
    assert.deepEqual(menu.map(item => item.label), ['Mở Zalo', 'Thoát']);
  });

  it('passes Dev Mode menu items through unchanged and in the same position', () => {
    const devModeItem = { label: 'Toggle DevTools', click: () => 'devtools', type: 'normal' };
    const menu = buildTrayMenuTemplate({
      locale: 'en',
      visible: true,
      devModeItems: [devModeItem],
      onToggleVisibility,
      onQuit
    });
    assert.strictEqual(menu[1], devModeItem);
    assert.equal(menu[1].label, 'Toggle DevTools');
    assert.strictEqual(menu[1].click, devModeItem.click);
    assert.deepEqual(menu.map(item => item.label), ['Hide Zalo', 'Toggle DevTools', 'Quit']);
  });

  it('preserves visibility and quit callbacks', () => {
    const menu = buildTrayMenuTemplate({ locale: 'vi', visible: false, onToggleVisibility, onQuit });
    assert.strictEqual(menu[0].click, onToggleVisibility);
    assert.strictEqual(menu[1].click, onQuit);
  });

  it('does not add Dev Mode item when Dev Mode supplies none', () => {
    const menu = buildTrayMenuTemplate({ locale: 'en', visible: true, devModeItems: [], onToggleVisibility, onQuit });
    assert.deepEqual(menu.map(item => item.label), ['Hide Zalo', 'Quit']);
  });
});

})();

(function testGroup_zcall_ui_i18n_test_js() {
const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const uiDir = path.join(__dirname, '..', 'plugins', 'zcall', 'ui');
const source = fs.readFileSync(path.join(uiDir, 'i18n.js'), 'utf8');

function loadUiI18n(page) {
  const html = { lang: '' };
  const window = { location: { href: new URL(page, 'file://' + uiDir + '/').href } };
  vm.runInNewContext(source, {
    window,
    URL,
    require,
    document: { documentElement: html },
  });
  return { api: window.zcallI18n, html };
}

describe('ZCall renderer translations', () => {
  it('loads the plugin dictionary beside the call page, independent of process cwd', () => {
    const { api, html } = loadUiI18n('call.html');
    assert.equal(api.getLocale(), 'vi');
    assert.equal(api.t('end'), 'Kết thúc');
    assert.equal(api.apply('en'), true);
    assert.equal(api.getLocale(), 'en');
    assert.equal(api.t('end'), 'End call');
    assert.equal(html.lang, 'en');
  });

  it('loads the same plugin dictionary beside the incoming-call page', () => {
    const { api } = loadUiI18n('incoming.html');
    assert.equal(api.t('answer'), 'Trả lời');
    assert.equal(api.apply('en'), true);
    assert.equal(api.t('answer'), 'Answer');
  });

  it('keeps invalid locale updates from changing the active dictionary', () => {
    const { api } = loadUiI18n('call.html');
    assert.equal(api.apply('fr'), false);
    assert.equal(api.getLocale(), 'vi');
    assert.equal(api.t('end'), 'Kết thúc');
  });
});

})();
