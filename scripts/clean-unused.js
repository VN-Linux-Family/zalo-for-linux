const fs = require('fs-extra');
const path = require('path');
const logger = require('./utils/logger');

const { APP_DIR, APP_NATIVE_LIBS } = require('./utils/paths');

const CLEAN_NATIVELiBS = [
  "db-cross-v4/prebuilt/darwin",
  "file-utilities/darwin",
  "file-utilities/darwin-arm",
  "file-utils/darwin",
  "file-utils/darwin-arm",
  "file-utils/ia32",
  "file-utils/x64",
  "mp4thumb/darwin-arm64",
  "mp4thumb/darwin-x64",
  "os-sys-info/darwin_arm64",
  "os-sys-info/darwin_x64",
  "sqlite3/binding/napi-v6-darwin-arm64",
  "sqlite3/binding/napi-v6-darwin-x64",
  "v8-profiles/profiler_electron1.8_mac.node",
  "zcall/zcall_mac.node",
  "zfile/win32",
  "zfile/win64",
  "zimage/darwin_arm64",
  "zimage/darwin_x64",
  "zjxl/build/darwin_arm64",
  "zjxl/build/darwin_x64",
  "zocr/darwin-arm64",
  "zocr/darwin-x64",
  "zwalker/darwin-arm64",
  "zwalker/darwin-x64",
]

// Linux-native addons always sit under a path containing "linux".
const PLATFORM_BINARY = /\.(node|dylib|dll|exe)$/i;
const BUILD_SOURCE = /(^Cargo\.(toml|lock)$)|(^build\.rs$)|(^rust-toolchain\.toml$)|\.rs$|\.pyc$/;
const BUILD_SOURCE_DIR = new Set(['__pycache__', '.cargo']);

async function sweep(dir, { platformBinaries }) {
  if (!(await fs.pathExists(dir))) return 0;

  let removed = 0;
  for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);

    if (entry.isDirectory()) {
      if (BUILD_SOURCE_DIR.has(entry.name)) {
        await fs.remove(full);
        removed++;
      } else {
        removed += await sweep(full, { platformBinaries });
      }
      continue;
    }

    const dropSource = BUILD_SOURCE.test(entry.name);
    const dropBinary = platformBinaries
      && PLATFORM_BINARY.test(entry.name)
      && !path.relative(APP_NATIVE_LIBS, full).includes('linux');

    if (dropSource || dropBinary) {
      await fs.remove(full);
      removed++;
    }
  }
  return removed;
}

async function main() {
  logger.info('Cleaning unused nativelibs...');

  for (const dir of CLEAN_NATIVELiBS) {
    const dirPath = path.join(APP_NATIVE_LIBS, dir);
    try {
      await fs.access(dirPath);
      await fs.remove(dirPath);
      logger.dim('Cleaned', dirPath);
    } catch (e) {
      logger.warn('Could not find', dirPath);
    }
  }

  const cleanedBinaries = await sweep(APP_NATIVE_LIBS, { platformBinaries: true });
  const cleanedSources = await sweep(APP_DIR, { platformBinaries: false });

  if (cleanedBinaries) logger.dim(`Cleaned ${cleanedBinaries} non-Linux binar(ies)`);
  if (cleanedSources) logger.dim(`Cleaned ${cleanedSources} build-only file(s)/dir(s)`);
}

if (require.main === module) {
  main();
}

module.exports = { main };