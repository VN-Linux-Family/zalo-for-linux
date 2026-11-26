'use strict';

const fs = require('fs');
const path = require('path');
const { spawnSync } = require('child_process');
const logger = require('./utils/logger');
const { APP_DIR, APP_MAIN_DIST } = require('./utils/paths');
const { runBoundedPool } = require('./utils/patch-pipeline');

const PATCHES = [
  'patch-zalo-language-bridge',
  'patch-zalo-language-transport',
  'patch-titlebar',
  'patch-wayland-titlebar',
  'patch-pasting-img',
  'patch-get-path-for-file',
  'patch-jxl-feature',
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
  'patch-zadark-keep',
  'patch-zocr-pc-dist',
  'patch-zocr-runtime',
  'patch-flatpak-file-transfer',
  'patch-zwalker',
  'patch-linux-guards',
  'patch-zlang',
];

// Disjoint target-file groups: patches sharing a mutable bundle (e.g. main-dist/main.js
// or pc-dist/*) run sequentially inside a single group to prevent lost updates,
// while disjoint groups execute in parallel via a memory-bounded worker pool.
const DISJOINT_GROUPS = [
  {
    type: 'fn',
    name: 'group-main-dist',
    label: 'main-dist bundle patches (single-parse)',
    module: '../utils/patch-groups.js',
    exportName: 'runMainDistGroup',
    patches: [
      'patch-titlebar',
      'patch-wayland-titlebar',
      'patch-pasting-img',
      'patch-get-path-for-file',
      'patch-jxl-feature',
      'patch-db-cross-v4',
      'patch-os-sys-info',
      'patch-zcall-native',
      'patch-auto-launch',
      'patch-quit-no-show',
      'patch-notification-badge',
      'patch-shell-open-linux',
      'patch-offline-startup',
      'patch-auto-theme',
      'patch-zadark-keep',
      'patch-linux-guards',
    ],
  },
  {
    type: 'fn',
    name: 'group-compact-app',
    label: 'main-dist: compact-app.js',
    module: '../utils/patch-groups.js',
    exportName: 'runCompactAppGroup',
    patches: [],
  },
  {
    type: 'fn',
    name: 'pc-dist-compact-app-pc',
    label: 'pc-dist: compact-app-pc (shared AST)',
    module: '../utils/patch-groups.js',
    exportName: 'runPcDistBundleGroup',
    args: ['compact-app-pc'],
    patches: ['patch-zalo-language-bridge', 'patch-zcall-callgate', 'patch-disk-space', 'patch-zocr-pc-dist'],
  },
  {
    type: 'fn',
    name: 'pc-dist-lazy-login',
    label: 'pc-dist: lazy/default-login (shared AST)',
    module: '../utils/patch-groups.js',
    exportName: 'runPcDistBundleGroup',
    args: ['lazy-login'],
    patches: [],
  },
  {
    type: 'fn',
    name: 'pc-dist-search-worker',
    label: 'pc-dist: search-worker',
    module: '../utils/patch-groups.js',
    exportName: 'runPcDistBundleGroup',
    args: ['search-worker'],
    patches: [],
  },
  {
    type: 'fn',
    name: 'pc-dist-sync-v2-sub-worker',
    label: 'pc-dist: sync-v2-sub-worker',
    module: '../utils/patch-groups.js',
    exportName: 'runPcDistBundleGroup',
    args: ['sync-v2-sub-worker'],
    patches: [],
  },
  { type: 'patch', name: 'patch-zalo-language-transport', label: 'patch-zalo-language-transport' },
  { type: 'patch', name: 'patch-sqlite3', label: 'patch-sqlite3' },
  { type: 'patch', name: 'patch-file-utilities', label: 'patch-file-utilities' },
  { type: 'patch', name: 'patch-file-utils', label: 'patch-file-utils' },
  { type: 'patch', name: 'patch-mp4thumb', label: 'patch-mp4thumb' },
  { type: 'patch', name: 'patch-zimage', label: 'patch-zimage' },
  { type: 'patch', name: 'patch-zjxl', label: 'patch-zjxl' },
  { type: 'patch', name: 'patch-zfile', label: 'patch-zfile' },
  { type: 'patch', name: 'patch-v8-profiles', label: 'patch-v8-profiles' },
  { type: 'patch', name: 'patch-zocr-runtime', label: 'patch-zocr-runtime' },
  { type: 'patch', name: 'patch-zwalker', label: 'patch-zwalker' },
  { type: 'patch', name: 'patch-zlang', label: 'patch-zlang' },
  { type: 'task', name: 'extract-zcall-assets', label: 'extract-zcall-assets', task: true },
  { type: 'task', name: 'extract-screenshot-assets', label: 'extract-screenshot-assets', task: true },
];

// Patches that mutate multiple bundles or touch compact-app.js run sequentially after Wave 1.
const CROSS_CUTTING_PATCHES = [
  'patch-call-signal-fix',
  'patch-xdg-user-dirs',
  'patch-network-and-sync-fix',
  'patch-flatpak-file-transfer',
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

  const verbose =
    process.argv.includes('-v') ||
    process.argv.includes('--verbose') ||
    process.env.ZALO_VERBOSE === '1' ||
    process.env.GITHUB_ACTIONS === 'true';
  logger.info(`Applying platform patches to: ${APP_DIR}...`);

  await runBoundedPool(DISJOINT_GROUPS, { verbose });

  const wave2Tasks = CROSS_CUTTING_PATCHES.map(name => ({ type: 'patch', name, label: name }));
  await runBoundedPool(wave2Tasks, { maxWorkers: 1, verbose });

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

module.exports = { main, PATCHES, DISJOINT_GROUPS, CROSS_CUTTING_PATCHES, runPatch };
