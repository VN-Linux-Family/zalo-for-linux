'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('./utils/logger');
const { APP_DIR, APP_MAIN_DIST } = require('./utils/paths');

async function main() {
  if (!fs.existsSync(APP_DIR)) {
    throw new Error(`Target app directory not found: ${APP_DIR}. Please run "npm run extract" first.`);
  }

  logger.info(`Applying platform patches to: ${APP_DIR}...`);

  const { main: patchZaloLanguageBridge } = require('./patches/patch-zalo-language-bridge');
  await patchZaloLanguageBridge();

  const { main: patchZaloLanguageTransport } = require('./patches/patch-zalo-language-transport');
  await patchZaloLanguageTransport();

  const { main: patchTitlebar } = require('./patches/patch-titlebar');
  await patchTitlebar();

  const { main: patchWaylandTitlebar } = require('./patches/patch-wayland-titlebar');
  await patchWaylandTitlebar();

  const { main: patchPastingImg } = require('./patches/patch-pasting-img');
  await patchPastingImg();

  const { main: patchSqlite3 } = require('./patches/patch-sqlite3');
  await patchSqlite3();

  const { main: patchDbCrossV4 } = require('./patches/patch-db-cross-v4');
  await patchDbCrossV4();

  const { main: patchFileUtilities } = require('./patches/patch-file-utilities');
  await patchFileUtilities();

  const { main: patchFileUtils } = require('./patches/patch-file-utils');
  await patchFileUtils();

  const { main: patchDiskSpace } = require('./patches/patch-disk-space');
  await patchDiskSpace();

  const { main: patchMp4thumb } = require('./patches/patch-mp4thumb');
  await patchMp4thumb();

  const { main: patchOsSysInfo } = require('./patches/patch-os-sys-info');
  await patchOsSysInfo();

  const { main: patchZimage } = require('./patches/patch-zimage');
  await patchZimage();

  const { main: patchZjxl } = require('./patches/patch-zjxl');
  await patchZjxl();

  const { main: patchZfile } = require('./patches/patch-zfile');
  await patchZfile();

  const { main: patchV8Profiles } = require('./patches/patch-v8-profiles');
  await patchV8Profiles();

  const { main: patchZcallCallgate } = require('./patches/patch-zcall-callgate');
  await patchZcallCallgate();

  // Calls run on the native engine (zcall/), no Wine.
  const { main: patchZcallNative } = require('./patches/patch-zcall-native');
  await patchZcallNative();

  const { main: patchAutoLaunch } = require('./patches/patch-auto-launch');
  await patchAutoLaunch();

  const { main: patchQuitNoShow } = require('./patches/patch-quit-no-show');
  await patchQuitNoShow();

  const { main: patchNotificationBadge } = require('./patches/patch-notification-badge');
  await patchNotificationBadge();

  const { main: patchXdgUserDirs } = require('./patches/patch-xdg-user-dirs');
  await patchXdgUserDirs();

  const { main: patchShellOpenLinux } = require('./patches/patch-shell-open-linux');
  await patchShellOpenLinux();

  const { main: patchCallSignalFix } = require('./patches/patch-call-signal-fix');
  await patchCallSignalFix();

  const { main: patchNetworkAndSyncFix } = require('./patches/patch-network-and-sync-fix');
  await patchNetworkAndSyncFix();

  await require('./patches/patch-offline-startup').main();

  const { main: patchAutoTheme } = require('./patches/patch-auto-theme');
  await patchAutoTheme();

  const { main: patchZocrPcDist } = require('./patches/patch-zocr-pc-dist');
  await patchZocrPcDist();

  const { main: patchZocrRuntime } = require('./patches/patch-zocr-runtime');
  await patchZocrRuntime();

  const { main: patchflatpakFileTransfer } = require('./patches/patch-flatpak-file-transfer');
  await patchflatpakFileTransfer();

  const { main: patchZwalker } = require('./patches/patch-zwalker');
  await patchZwalker();

  const { main: patchZlang } = require('./patches/patch-zlang');
  await patchZlang();

  // The call window's icons, sounds and fonts, from the macOS app.
  await require('./extract-zcall-assets').main();

  // The screenshot tool's icon font, from pc-dist.
  await require('./extract-screenshot-assets').main();

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

module.exports = { main };
