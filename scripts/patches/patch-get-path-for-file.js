/**
 * patch-get-path-for-file.js
 *
 * Zalo 26.11.20 is built for Electron 43 and its preloads expose
 *   getPathForFile: e => r.webUtils.getPathForFile(e)
 * to find the disk path of a File the user picked, dropped or pasted.
 * `webUtils` only exists from Electron 29. On the Electron 22 runtime this
 * repo ships it is undefined, so the call throws inside the send promise and
 * sending a picked or dropped photo/file fails.
 *
 * Fall back to Electron 22's `File.path` when `webUtils` is missing; newer
 * runtimes keep using webUtils.
 */

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { APP_MAIN_DIST } = require('../utils/paths');

const CALL = /getPathForFile:([\w$]+)=>([\w$]+)\.webUtils\.getPathForFile\(\1\)/g;
const DONE = /getPathForFile:([\w$]+)=>([\w$]+)\.webUtils\?\2\.webUtils\.getPathForFile\(\1\):\1&&\1\.path/;

// Returns { content, status }: 'patched', 'already', 'absent' or 'missing'.
function patchContent(content) {
  if (!content.includes('webUtils')) return { content, status: 'absent' };
  const patched = content.replace(CALL, 'getPathForFile:$1=>$2.webUtils?$2.webUtils.getPathForFile($1):$1&&$1.path');
  if (patched !== content) return { content: patched, status: 'patched' };
  return { content, status: DONE.test(content) ? 'already' : 'missing' };
}

async function main(mainDist = APP_MAIN_DIST) {
  const missing = [];
  let count = 0;
  for (const name of fs.readdirSync(mainDist).filter(f => f.endsWith('.js'))) {
    const file = path.join(mainDist, name);
    const { content, status } = patchContent(fs.readFileSync(file, 'utf8'));
    if (status === 'missing') missing.push(name);
    if (status === 'patched') {
      fs.writeFileSync(file, content, 'utf8');
      logger.dim(`Patched getPathForFile fallback in ${name}`);
    }
    if (status === 'patched' || status === 'already') count++;
  }
  if (missing.length) {
    throw new Error('getPathForFile patch not applied, webUtils usage changed in: ' + missing.join(', '));
  }
  logger.success(count ? `getPathForFile falls back to File.path (${count} files)` : 'No webUtils usage in this Zalo version');
}

if (require.main === module) {
  main().catch((error) => {
    logger.error('getPathForFile patch failed:', error.message);
    process.exit(1);
  });
}

module.exports = { main, patchContent };
