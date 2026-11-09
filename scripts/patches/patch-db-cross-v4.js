const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { arch } = process;

const { APP_MAIN_DIST, APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const DB_CROSS_V4_DIR = path.join(NATIVELIBS_DIR, 'db-cross-v4');

async function main() {
  logger.info('Installing db-cross-v4 native addon...');

  const releaseDir = path.join(DB_CROSS_V4_DIR, 'build', 'Release');
  const fallbackNode = fs.existsSync(releaseDir) && fs.readdirSync(releaseDir).some(f => f.endsWith('.node'))
    ? path.join(releaseDir, fs.readdirSync(releaseDir).find(f => f.endsWith('.node')))
    : path.join(releaseDir, 'db-cross-v4-native.node');
  const builtNode = resolvePrebuiltBinary('db-cross-v4-native.node', fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for db-cross-v4 at: ${builtNode}. Please run "npm run build:native" first.`);
  }
  const destDir = path.join(APP_NATIVE_LIBS, 'db-cross-v4', 'prebuilt', 'linux', 'electron', `${arch}`);
  fs.ensureDirSync(destDir);

  fs.copyFileSync(builtNode, path.join(destDir, 'db-cross-v4-native.node'));

  const bindingJsPath = path.join(APP_NATIVE_LIBS, 'db-cross-v4', 'dist', 'binding.js');
  if (fs.existsSync(bindingJsPath)) {
    let content = fs.readFileSync(bindingJsPath, 'utf8');

    if (!content.includes("process.platform === 'linux'")) {
      content = content.replace(
        /else \{\s*if \(process\.arch === 'x64'\)/,
        `else if (process.platform === 'linux') {\n    addon = require('../prebuilt/linux/electron/${arch}/db-cross-v4-native.node');\n}\nelse {\n    if (process.arch === 'x64')`
      );
      fs.writeFileSync(bindingJsPath, content, 'utf8');
      logger.dim('Patched binding.js for Linux support');
    }
  }

  const mainJsPath = path.join(APP_MAIN_DIST, 'main.js');
  if (fs.existsSync(mainJsPath)) {
    let content = fs.readFileSync(mainJsPath, 'utf8');

    // PLATFORM ID PATCH
    // ---------------------------------------------------------------
    // Switching the platform ID from 25 to another ID is required to
    // make the patch's features work. The only observed difference
    // is how Zalo's tray icon behaves:
    //
    //   25 → 23 (macOS)   ← currently used
    //       Zalo's tray icon does not appear, so only our own icon
    //       is shown.
    //
    //   25 → 24 (Windows)
    //       Zalo's tray icon also appears alongside ours, but the
    //       actual Zalo logo is missing — it shows a default/blank
    //       icon. Result: two tray icons, with the Zalo one looking
    //       wrong.
    // ---------------------------------------------------------------
    if (content.includes('case"LINUX":return 25;')) {
      content = content.replace(/case"LINUX":return 25;/g, 'case"LINUX":return 23;');
      fs.writeFileSync(mainJsPath, content, 'utf8');
      logger.dim('Patched platform ID (25 -> 23, macOS)');
    }
  }

  logger.success('db-cross-v4 built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };