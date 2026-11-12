'use strict';

const fs = require('fs');
const path = require('path');
const vm = require('vm');
const { spawnSync } = require('child_process');
const logger = require('../utils/logger');
const { parseAst } = require('../utils/ast-transform');
const { APP_MAIN_DIST } = require('../utils/paths');

const MARKER = 'zalo-language-main-transport';
const TRANSPORT = `
;/* ${MARKER} */
(() => {
  const { contextBridge, ipcRenderer } = require('electron');
  const api = Object.freeze({
    publishLocale(locale) {
      if (locale !== 'vi' && locale !== 'en') return false;
      ipcRenderer.send('zalo-language:publish-locale', locale);
      return true;
    }
  });
  // Zalo has windows with different contextIsolation settings. Match the
  // existing preload bridge convention instead of throwing on non-isolated
  // renderers (an uncaught preload error can break startup/session UI).
  if (process.contextIsolated) {
    contextBridge.exposeInMainWorld('__zaloLanguageTransport', api);
  } else if (typeof window !== 'undefined') {
    window.__zaloLanguageTransport = api;
  }
})();
`;

function patchContent(content) {
  if (typeof content !== 'string') throw new TypeError('Preload content must be a string');
  if (content.includes(MARKER)) {
    try { parseAst(content); new vm.Script(content); return { content, status: 'already' }; }
    catch (error) { return { content, status: 'parse-error', error }; }
  }
  try {
    parseAst(content);
    new vm.Script(content);
    const updated = content + '\n' + TRANSPORT + '\n';
    parseAst(updated);
    new vm.Script(updated);
    return { content: updated, status: 'patched' };
  } catch (error) {
    return { content, status: 'parse-error', error };
  }
}

function patchFile(filePath) {
  if (!fs.existsSync(filePath)) throw new Error('Zalo renderer preload not found: ' + filePath);
  const original = fs.readFileSync(filePath, 'utf8');
  const result = patchContent(original);
  if (result.status === 'patched') fs.writeFileSync(filePath, result.content, 'utf8');
  if (result.status === 'parse-error') throw result.error;
  return result.status;
}

async function main(appMainDist = APP_MAIN_DIST) {
  logger.info('Patching Zalo language transport...');
  try {
    // Zalo's primary windows use preload-render.js, while Compact App (where
    // the Language Manager lives) uses its own compact-app-preload.js.
    // Both renderers need the same narrow locale-only IPC capability.
    const files = ['preload-render.js', 'compact-app-preload.js'];
    const results = [];
    for (const name of files) {
      const filePath = path.join(appMainDist, name);
      const runner = 'const status = require(process.argv[1]).patchFile(process.argv[2]); process.exitCode = status === "patched" ? 10 : 0;';
      const result = spawnSync(process.execPath, ['-e', runner, __filename, filePath], {
        stdio: 'inherit',
        env: process.env,
      });
      if (result.error) throw result.error;
      if (result.status !== 0 && result.status !== 10) throw new Error(`Language transport failed for ${name} (exit code ${result.status})`);
      results.push({ filePath, name, status: result.status === 10 ? 'patched' : 'already' });
    }
    for (const result of results) {
      if (result.status === 'patched') {
        logger.dim('Patched locale-only IPC transport in ' + result.name);
      } else {
        logger.info('Zalo language transport already present in ' + result.name + '; no changes needed');
      }
    }
    logger.success('Zalo language transport verified for all Zalo renderers');
    return results;
  } catch (error) {
    logger.error('Zalo language transport patch failed:', error.message);
    throw error;
  }
}

if (require.main === module) {
  main().catch(() => process.exit(1));
}

module.exports = { main, patchContent, patchFile, MARKER, TRANSPORT };
