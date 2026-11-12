'use strict';

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const logger = require('./utils/logger');
const { APP_DIR, APP_MAIN_DIST } = require('./utils/paths');

const PATCHES = [
  'patch-zalo-language-bridge',
  'patch-zalo-language-transport',
  'patch-titlebar',
  'patch-wayland-titlebar',
  'patch-pasting-img',
  'patch-sqlite3',
  'patch-db-cross-v4',
  'patch-file-utilities',
  'patch-file-utils',
  'patch-disk-space',
  'patch-mp4thumb',
  'patch-os-sys-info',
  'patch-zimage',
  'patch-zjxl',
  'patch-zfile',
  'patch-v8-profiles',
  'patch-zcall-callgate',
  'patch-zcall-native',
  'patch-auto-launch',
  'patch-quit-no-show',
  'patch-notification-badge',
  'patch-xdg-user-dirs',
  'patch-shell-open-linux',
  'patch-call-signal-fix',
  'patch-network-and-sync-fix',
  'patch-offline-startup',
  'patch-auto-theme',
  'patch-zocr-pc-dist',
  'patch-zocr-runtime',
  'patch-flatpak-file-transfer',
  'patch-zwalker',
  'patch-linux-guards',
  'patch-zlang',
];

function runPatch(name, args = [], task = false) {
  const target = path.join(__dirname, task ? `${name}.js` : `patches/${name}.js`);
  const runner = [
    'const target = process.argv[1];',
    'const args = process.argv.slice(2);',
    'Promise.resolve(require(target).main(...args)).catch((error) => { console.error(error); process.exitCode = 1; });',
  ].join(' ');
  const result = spawnSync(process.execPath, ['-e', runner, target, ...args], {
    stdio: 'inherit',
    env: process.env,
    cwd: process.cwd(),
  });

  if (result.error) throw result.error;
  if (result.status !== 0) {
    const reason = result.signal ? `signal ${result.signal}` : `exit code ${result.status}`;
    throw new Error(`${name} failed (${reason})`);
  }
}

async function main() {
  if (!fs.existsSync(APP_DIR)) {
    throw new Error(`Target app directory not found: ${APP_DIR}. Please run "npm run extract" first.`);
  }

  logger.info(`Applying platform patches to: ${APP_DIR}...`);

  for (const patch of PATCHES) {
    logger.info(`Running ${patch}...`);
    runPatch(patch);
  }

  runPatch('extract-zcall-assets', [], true);
  runPatch('extract-screenshot-assets', [], true);

  // Without it Zalo would look for ZaloCall.exe / ZaloHelper.app: no calls at all.
  const mainJs = path.join(APP_MAIN_DIST, 'main.js');
  if (!fs.existsSync(mainJs) || !fs.readFileSync(mainJs, 'utf8').includes('ZCALL_ENGINE_JS')) {
    throw new Error('native call patch missing from app/main-dist/main.js');
  }

  logger.success('All platform patches applied successfully');
}

if (require.main === module) {
  main().catch((err) => {
    logger.error('Patching failed:', err.message);
    process.exit(1);
  });
}

module.exports = { main, PATCHES, runPatch };
