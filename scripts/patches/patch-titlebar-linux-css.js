'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { APP_PC_DIST } = require('../utils/paths');

const ORIGINAL_RULE = '.titlebar{-webkit-user-select:none;height:32px;min-height:32px;background:var(--title-bar);font-size:.8125rem;font-weight:500;line-height:1.5;z-index:2;width:calc(100% - 86px);left:70px;padding-left:16px;color:var(--text-primary)}';
const TITLEBAR_RULES = '.titlebar{left:0!important;width:100%!important;box-sizing:border-box!important;padding-left:0!important}.titlebar .title-name.macos,.titlebar.locked .title-name.macos{left:0!important;width:100%!important;padding-left:8px!important;box-sizing:border-box!important}.login-title-bar__setting-button:has(> div[icon="icon-outline-menu"].z--btn--v2.btn-tertiary-neutral.large.--rounded.icon-only){left:0!important;right:auto!important}';
const NAV_RULE = '#sidebarNav:before{content:"";display:block;flex:0 0 70px;width:70px;min-width:70px}#main-tab:not(.WEB){position:fixed!important;left:0!important;top:32px!important;bottom:0!important;width:70px!important;height:auto!important;margin:0!important;align-self:auto!important;z-index:11!important}';
const BANNER_RULE = '';
const MACOS_NAV_GAP_RULES = [
  ['.nav__tabs__header{display:flex;flex-direction:column;gap:6px;margin-top:32px}', '.nav__tabs__header{display:flex;flex-direction:column;gap:6px}'],
  ['.nav__tabs.WEB .nav__tabs__header{margin-top:4px}', ''],
];
const RULE = TITLEBAR_RULES + NAV_RULE + BANNER_RULE;

function patchContent(content) {
  if (content.includes(RULE)) {
    throw new Error('Linux titlebar CSS patch: expected original Zalo stylesheet, found patched CSS');
  }
  if (!content.includes(ORIGINAL_RULE)) {
    throw new Error('Linux titlebar CSS patch: expected original Zalo titlebar rule not found');
  }
  return { content: content.replace(ORIGINAL_RULE, ORIGINAL_RULE + RULE), status: 'patched' };
}

function collectFiles(directory, extension) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const file = path.join(directory, entry.name);
    return entry.isDirectory() ? collectFiles(file, extension) : entry.isFile() && entry.name.endsWith(extension) ? [file] : [];
  });
}

function patchLayoutContent(content) {
  const obsoleteNavRules = [
    '#main-tab{position:relative!important;top:32px!important;height:calc(100vh - 32px)!important;margin-top:-32px!important;padding-top:0!important;align-self:flex-start!important}',
    'body:has(.system-banner__container.--main-window:not(.--web)) #main-tab:not(.WEB){margin-top:-64px!important}',
  ];
  if (content.includes(NAV_RULE) || content.includes(TITLEBAR_RULES)) {
    throw new Error('Linux titlebar CSS patch: expected pristine Zalo layout, found patched CSS');
  }
  if (obsoleteNavRules.some(rule => content.includes(rule))) {
    throw new Error('Linux titlebar CSS patch: expected pristine Zalo layout, found obsolete titlebar compensation');
  }

  let output = content;
  let changed = false;
  for (const [reserved, replacement] of MACOS_NAV_GAP_RULES) {
    if (output.includes(reserved)) {
      output = output.replace(reserved, replacement);
      changed = true;
    }
  }
  if (output.includes(ORIGINAL_RULE) && !output.includes(TITLEBAR_RULES)) {
    output = output.replace(ORIGINAL_RULE, ORIGINAL_RULE + TITLEBAR_RULES);
    changed = true;
  }
  if (output.includes('#main-tab{width:70px')) {
    output += NAV_RULE;
    changed = true;
  }
  return { content: output, changed };
}

async function main() {
  const cssFiles = collectFiles(APP_PC_DIST, '.css');
  let titlebarTargets = 0;
  let navTargets = 0;
  for (const file of cssFiles) {
    const original = fs.readFileSync(file, 'utf8');
    if (original.includes(ORIGINAL_RULE)) titlebarTargets++;
    if (original.includes('#main-tab{width:70px')) navTargets++;
    const result = patchLayoutContent(original);
    if (result.changed) {
      fs.writeFileSync(file, result.content, 'utf8');
      logger.dim('Linux titlebar/nav layout patched: ' + path.relative(APP_PC_DIST, file));
    }
  }
  if (!titlebarTargets) throw new Error('Linux titlebar CSS patch: original Zalo titlebar stylesheets not found');
  if (!navTargets) throw new Error('Linux titlebar CSS patch: Zalo main navigation stylesheet not found');


}

if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
module.exports = { main, patchContent, patchLayoutContent, collectFiles, ORIGINAL_RULE, TITLEBAR_RULES, NAV_RULE, BANNER_RULE, MACOS_NAV_GAP_RULES, RULE };
