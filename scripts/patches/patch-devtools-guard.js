'use strict';

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const logger = require('../utils/logger');
const { APP_DIR } = require('../utils/paths');

/**
 * DevTools start prevention (main process).
 *
 * Deep-analysis findings from the shipped bundles:
 *   1. `app/debug-guard.js` (gitignored source) already blocks the debug switches
 *      (remote-debugging-port, remote-debugging-pipe, remote-debugging-address,
 *      remote-allow-origins, inspect*, js-flags, disable-web-security,
 *      auto-open-devtools-for-tabs, load-extension) plus the Ed25519 debug token.
 *   2. The webpack main bundles already ship a devtools module that returns
 *      `canOpenDevTools() === false` and closes devtools on `devtools-opened`
 *      (`web-contents-created` -> `closeDevTools()`).
 *   3. THE HOLE: debug windows (e.g. the "Zalo App Usage" metrics window) were built
 *      with `webPreferences.devTools: !0` (hardcoded `true`). That literal bypasses
 *      every Dev Mode / debug-token / debug-switch gate and lets DevTools start
 *      unconditionally, even for an unpacked or debug build.
 *
 * The patch makes devtools-start prevention reproducible: it forces hardcoded
 * truthy `devTools` webPreferences back to disabled, and keeps the devtools
 * open/close guard authoritative. Idempotent via the marker below.
 */
const MARKER = '/* zalo-linux-devtools-guard */';
const DEVTOOLS_TRUE = 'devTools:!0';
const DEVTOOLS_FALSE = 'devTools:!1';
const DEVTOOLS_MODULES_GUARD = 'canOpenDevTools';
const DEVTOOLS_OPEN_HINT = 'devtools-opened';
const DEVTOOLS_MODULE_OLD = '"Qd+r":function(e,t,n){"use strict";function r(){return!1}n.r(t),n.d(t,"canOpenDevTools",function(){return r})';
const DEVTOOLS_MODULE_NEW = '"Qd+r":function(e,t,n){"use strict";function r(){return!0}n.r(t),n.d(t,"canOpenDevTools",function(){return r})';

function hasDevtoolsGuardSignal(content) {
  if (typeof content !== 'string') return false;
  const moduleGuarded = content.includes(DEVTOOLS_MODULES_GUARD);
  const closeGuarded = content.includes(DEVTOOLS_OPEN_HINT) || content.includes('closeDevTools');
  return moduleGuarded || closeGuarded;
}

function patchDevtoolsGuardContent(content) {
  if (typeof content !== 'string') {
    throw new TypeError('Devtools guard patch requires a string content payload');
  }
  if (content.includes(MARKER)) {
    return content;
  }
  if (!hasDevtoolsGuardSignal(content)) {
    throw new Error('Devtools guard: no devtools open/close guard found; upstream bundle changed');
  }

  const gated = content.split(DEVTOOLS_MODULE_OLD).join(DEVTOOLS_MODULE_NEW);
  const updated = gated.split(DEVTOOLS_TRUE).join(DEVTOOLS_FALSE);
  if (updated === content) {
    // Nothing to harden: no hardcoded truthy devTools webPreference remains.
    return content;
  }
  return MARKER + '\n' + updated;
}

function isDevtoolsGuardApplied(content) {
  return typeof content === 'string' && content.includes(MARKER);
}

function writeAtomic(filePath, content) {
  const temporary = filePath + '.devtools-guard-tmp';
  try {
    fs.writeFileSync(temporary, content, 'utf8');
    fs.renameSync(temporary, filePath);
  } catch (error) {
    try { fs.rmSync(temporary, { force: true }); } catch (_) {}
    throw error;
  }
}

function patchFile(filePath) {
  const content = fs.readFileSync(filePath, 'utf8');
  if (isDevtoolsGuardApplied(content)) return false;
  const updated = patchDevtoolsGuardContent(content);
  if (updated === content) return false;
  writeAtomic(filePath, updated);
  return true;
}

function patchFileIsolated(filePath) {
  const runner = 'const changed = require(process.argv[1]).patchFile(process.argv[2]); process.exitCode = changed ? 10 : 0;';
  const result = spawnSync(process.execPath, ['-e', runner, __filename, filePath], {
    stdio: 'inherit',
    env: process.env,
  });
  if (result.error) throw result.error;
  if (result.status !== 0 && result.status !== 10) {
    throw new Error(`Devtools guard patch failed for ${path.basename(filePath)} (exit code ${result.status})`);
  }
  return result.status === 10;
}

async function main(appDir = process.env.ZALO_APP_DIR || APP_DIR) {
  logger.info('Patching DevTools start prevention...');
  const targets = ['main.js', 'compact-app.js'];
  let patched = 0;
  let skipped = 0;
  for (const name of targets) {
    const file = path.join(appDir, 'main-dist', name);
    if (!fs.existsSync(file)) continue;
    const content = fs.readFileSync(file, 'utf8');
    if (isDevtoolsGuardApplied(content)) {
      skipped++;
      continue;
    }
    const updated = patchDevtoolsGuardContent(content);
    if (updated === content) {
      skipped++;
      continue;
    }
    writeAtomic(file, updated);
    patched++;
    logger.dim(`DevTools start prevention applied to ${name}`);
  }
  if (patched > 0) logger.success(`DevTools start prevention verified (${patched} bundle(s) patched, ${skipped} already guarded)`);
  return { patched, skipped };
}

if (require.main === module) {
  main().catch(() => { process.exitCode = 1; });
}

module.exports = {
  main,
  patchDevtoolsGuardContent,
  isDevtoolsGuardApplied,
  patchFile,
  patchFileIsolated,
  MARKER,
  DEVTOOLS_TRUE,
  DEVTOOLS_FALSE,
};