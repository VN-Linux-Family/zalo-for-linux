'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { APP_PC_DIST } = require('../utils/paths');

const ORIGINAL_RULE = '.titlebar{-webkit-user-select:none;height:32px;min-height:32px;background:var(--title-bar);font-size:.8125rem;font-weight:500;line-height:1.5;z-index:2;width:calc(100% - 86px);left:70px;padding-left:16px;color:var(--text-primary)}';
const LOGIN_TITLEBAR_RULE = '.login-title-bar__setting-button:has(> div[icon="icon-outline-menu"].z--btn--v2.btn-tertiary-neutral.large.--rounded.icon-only){left:0!important;right:auto!important}';

function patchContent(content) {
  if (content.includes(LOGIN_TITLEBAR_RULE)) return { content, status: 'already' };
  if (!content.includes(ORIGINAL_RULE)) {
    throw new Error('Linux titlebar CSS patch: expected original Zalo titlebar rule not found');
  }
  return {
    content: content.replace(ORIGINAL_RULE, ORIGINAL_RULE + LOGIN_TITLEBAR_RULE),
    status: 'patched',
  };
}

function collectFiles(directory, extension) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const file = path.join(directory, entry.name);
    return entry.isDirectory() ? collectFiles(file, extension) : entry.isFile() && entry.name.endsWith(extension) ? [file] : [];
  });
}

async function main() {
  const cssFiles = collectFiles(APP_PC_DIST, '.css');
  let titlebarTargets = 0;

  for (const file of cssFiles) {
    const original = fs.readFileSync(file, 'utf8');
    if (!original.includes(ORIGINAL_RULE)) continue;

    titlebarTargets++;
    const result = patchContent(original);
    if (result.status === 'patched') {
      fs.writeFileSync(file, result.content, 'utf8');
      logger.dim('Linux login titlebar CSS patched: ' + path.relative(APP_PC_DIST, file));
    }
  }

  if (!titlebarTargets) throw new Error('Linux titlebar CSS patch: original Zalo titlebar stylesheets not found');
}

if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
module.exports = { main, patchContent, collectFiles, ORIGINAL_RULE, LOGIN_TITLEBAR_RULE };
