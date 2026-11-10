'use strict';
// Applies ZaDark to a Zalo root (`<root>/app`). `main()` also writes the files
// electron-builder would normally own, for packagers that skip it (e.g. AUR).
const fs = require('fs');
const path = require('path');
const logger = require('./utils/logger');
const { buildCommit } = require('./utils/build-commit');
const { ZADARK_VENDOR_DIR } = require('./utils/paths');

const REPO_ROOT = path.resolve(__dirname, '..');

function zaloVersion(zaloRoot) {
  if (process.env.ZALO_VERSION) return process.env.ZALO_VERSION;
  // extract.js keeps the extracted app's manifest as app/package.json.bak.
  for (const file of ['package.json.bak', 'package.json']) {
    const full = path.join(zaloRoot, 'app', file);
    if (fs.existsSync(full)) {
      return JSON.parse(fs.readFileSync(full, 'utf8')).version;
    }
  }
  return null;
}

async function integrate(zaloRoot) {
  const appDir = path.join(zaloRoot, 'app');
  const modulePath = path.join(ZADARK_VENDOR_DIR, 'build', 'pc', 'zadark-pc.js');
  if (!fs.existsSync(modulePath)) {
    throw new Error('ZaDark PC module not found - run "npm run prepare-zadark" first');
  }

  const zadarkPC = require(modulePath);
  zadarkPC.copyZaDarkAssets(zaloRoot);
  zadarkPC.writeIndexFile(zaloRoot);
  zadarkPC.writeBootstrapFile(zaloRoot);
  zadarkPC.writePopupViewerFile(zaloRoot);

  // api.zadark.com is dead: route translate requests to Google Translate.
  await require('./patches/patch-zadark-translate').main(appDir);
  // Zalo's dark theme strips ZaDark's CSS/scripts from the page (#66).
  await require('./patches/patch-zadark-keep').main(appDir);
}

function writeBuildInfo(zaloRoot) {
  const pcDist = path.join(zaloRoot, 'app', 'pc-dist');
  if (!fs.existsSync(pcDist)) {
    logger.warn('pc-dist not found, skipping build-info.json');
    return false;
  }

  const { version } = JSON.parse(
    fs.readFileSync(path.join(ZADARK_VENDOR_DIR, 'package.json'), 'utf8'),
  );
  fs.writeFileSync(
    path.join(pcDist, 'build-info.json'),
    JSON.stringify({
      version: zaloVersion(zaloRoot),
      zadarkVersion: version,
      commit: buildCommit(),
      buildDate: new Date().toISOString(),
    }, null, 2),
    'utf8',
  );
  return true;
}

function writePackageVersion(zaloRoot) {
  const version = zaloVersion(zaloRoot);
  const file = path.join(zaloRoot, 'package.json');
  if (!version || !fs.existsSync(file)) return false;

  const pkg = JSON.parse(fs.readFileSync(file, 'utf8'));
  pkg.version = version;
  fs.writeFileSync(file, JSON.stringify(pkg, null, 2) + '\n', 'utf8');
  return true;
}

async function main(zaloRoot = REPO_ROOT) {
  await integrate(zaloRoot);
  writeBuildInfo(zaloRoot);
  if (!writePackageVersion(zaloRoot)) {
    logger.dim('package.json version left unchanged (no version resolved)');
  }
  logger.success(`ZaDark applied to ${path.relative(process.cwd(), zaloRoot) || '.'}`);
}

if (require.main === module) {
  main(process.argv[2]).catch((error) => {
    logger.error('ZaDark integration failed:', error.message);
    process.exit(1);
  });
}

module.exports = { main, integrate };
