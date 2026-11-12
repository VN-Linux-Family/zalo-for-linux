const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { spawnSync } = require('child_process');
const { parseAst, applyEdits } = require('../utils/ast-transform');

const { APP_DIR } = require('../utils/paths');

/**
 * Do not show the main window while quitting.
 *
 * A quit Zalo did not start itself (our close-to-quit without a tray host,
 * the tray "Thoát", a session logout) goes through requestQuitApp(): it asks
 * the renderer to save its state ("before-quit") and then calls
 * mainWindow.show(), meant for quitting from the macOS dock. On Linux that
 * brought the just-closed window back for a moment before the app exited.
 * The renderer handshake is kept; only the show() is skipped on Linux.
 */
const SHOW_RE = /(this\.signalBeforeQuitToRender\(\),[\w$]+\)return void this\.quit\(\);)([\w$]+)\|\|this\.mainWindow\.show\(\)/g;
const PATCHED = '||"linux"===process.platform||this.mainWindow.show()';

function patchQuitContent(content) {
 if (content.includes(PATCHED)) return content;
 try {
  const root = parseAst(content);
  const match = root.find('$COND || this.mainWindow.show()');
  if (match) {
   const cond = match.getMatch('COND').text();
   const edit = match.replace(`${cond} || "linux" === process.platform || this.mainWindow.show()`);
   return applyEdits(root, [edit]);
  }
 } catch (err) {
  logger.warn('AST error on quit-no-show, using regex fallback:', err.message);
 }
 return content.replace(SHOW_RE, `$1$2${PATCHED}`);
}

function patchFile(filePath) {
 const content = fs.readFileSync(filePath, 'utf8');
 if (content.includes(PATCHED)) return false;
 const updated = patchQuitContent(content);
 if (updated === content) return false;
 fs.writeFileSync(filePath, updated, 'utf8');
 return true;
}

function patchFileIsolated(filePath) {
 const runner = 'const changed = require(process.argv[1]).patchFile(process.argv[2]); process.exitCode = changed ? 10 : 0;';
 const result = spawnSync(process.execPath, ['-e', runner, __filename, filePath], {
  stdio: 'inherit',
  env: process.env,
 });
 if (result.error) throw result.error;
 if (result.status !== 0 && result.status !== 10) throw new Error(`Quit-no-show patch failed for ${path.basename(filePath)} (exit code ${result.status})`);
 return result.status === 10;
}

async function main(appDir = APP_DIR) {
 let patched = 0;
 for (const name of ['main.js', 'compact-app.js']) {
  const file = path.join(appDir, 'main-dist', name);
  if (!fs.existsSync(file)) continue;
  if (patchFileIsolated(file)) {
   patched++;
   logger.dim(`Quit without re-showing the window via AST/fallback: ${name}`);
  } else {
   const content = fs.readFileSync(file, 'utf8');
   if (!content.includes(PATCHED) && content.includes('signalBeforeQuitToRender')) {
    logger.warn(`requestQuitApp pattern not found in ${name}; the window may flash on quit`);
   }
  }
 }
 if (patched > 0) logger.success(`Quit flow patched (${patched} files)`);
}

if (require.main === module) {
 main();
}

module.exports = { main, patchQuitContent, patchFile };
