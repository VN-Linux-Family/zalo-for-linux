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

// Build-only files/directories that ship inside an addon folder but are never
// read at runtime. Kept explicit per addon: pruning by generic name (src/,
// scripts/) could delete a loader a future addon actually uses.
const ADDON_BUILD_ONLY = {
  zlang: [
    'index.ts',
    'tsconfig.json',
    'scripts',
    'src',
    'build.rs',
    'Cargo.toml',
    'Cargo.lock',
    'rust-toolchain.toml',
    '.cargo',
  ],
};

// Linux-native addons always sit under a path containing "linux".
const PLATFORM_BINARY = /\.(node|dylib|dll|exe)$/i;
const BUILD_SOURCE = /(^Cargo\.(toml|lock)$)|(^build\.rs$)|(^rust-toolchain\.toml$)|\.rs$|\.pyc$/;
const BUILD_SOURCE_DIR = new Set(['__pycache__', '.cargo']);

// Platform directories that only carry non-Linux binaries once their .node /
// .dylib files are swept (darwin-arm64, win32-x64, ...). Their build-info.json
// describes a binary that no longer ships, so the whole directory goes.
const NON_LINUX_PLATFORM_DIR = /^(?:darwin|win32|win64|windows|mac|macos|apple)(?:[-_]|$)/i;

async function sweep(dir, { platformBinaries, root = APP_NATIVE_LIBS }) {
  if (!(await fs.pathExists(dir))) return 0;

  let removed = 0;
  for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);

    if (entry.isDirectory()) {
      if (BUILD_SOURCE_DIR.has(entry.name)) {
        await fs.remove(full);
        removed++;
      } else {
        removed += await sweep(full, { platformBinaries, root });
      }
      continue;
    }

    const dropSource = BUILD_SOURCE.test(entry.name);
    const dropBinary = platformBinaries
      && PLATFORM_BINARY.test(entry.name)
      && !path.relative(root, full).includes('linux');

    if (dropSource || dropBinary) {
      await fs.remove(full);
      removed++;
    }
  }
  return removed;
}

async function pruneAddonBuildFiles(rootDir = APP_NATIVE_LIBS) {
  let removed = 0;
  for (const [addon, entries] of Object.entries(ADDON_BUILD_ONLY)) {
    for (const entry of entries) {
      const full = path.join(rootDir, addon, entry);
      if (await fs.pathExists(full)) {
        await fs.remove(full);
        removed++;
      }
    }
  }
  return removed;
}

async function pruneNonLinuxPlatformDirs(dir) {
  let removed = 0;
  for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const full = path.join(dir, entry.name);
    if (NON_LINUX_PLATFORM_DIR.test(entry.name)) {
      await fs.remove(full);
      removed++;
    } else {
      removed += await pruneNonLinuxPlatformDirs(full);
    }
  }
  return removed;
}

// Bottom-up: a directory emptied by the binary sweep has nothing left to load.
async function removeEmptyDirs(dir) {
  let removed = 0;
  for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const full = path.join(dir, entry.name);
    removed += await removeEmptyDirs(full);
    if ((await fs.readdir(full)).length === 0) {
      await fs.remove(full);
      removed++;
    }
  }
  return removed;
}

async function findNonLinuxBinaries(dir, out = [], root = dir) {
  for (const entry of await fs.readdir(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      await findNonLinuxBinaries(full, out, root);
    } else if (
      PLATFORM_BINARY.test(entry.name)
      && !path.relative(root, full).includes('linux')
    ) {
      out.push(path.relative(root, full));
    }
  }
  return out;
}

// Removes every non-Linux binary/source under `dir` and then prunes the
// directories they leave behind. Returns a summary so callers (and tests) can
// assert on the result; leftovers are unexpected, so they are warned about.
async function cleanNativelibs(dir = APP_NATIVE_LIBS) {
  const binaries = await sweep(dir, { platformBinaries: true, root: dir });
  const addonFiles = await pruneAddonBuildFiles(dir);
  const platformDirs = await pruneNonLinuxPlatformDirs(dir);
  const emptyDirs = await removeEmptyDirs(dir);
  const leftovers = await findNonLinuxBinaries(dir);

  if (leftovers.length) {
    logger.warn(
      `Non-Linux binaries still present under nativelibs: ${leftovers.join(', ')}`
    );
  }

  return { binaries, addonFiles, platformDirs, emptyDirs, leftovers };
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

  const cleanedNativelibs = await cleanNativelibs();
  const cleanedSources = await sweep(APP_DIR, { platformBinaries: false });

  if (cleanedNativelibs.binaries) {
    logger.dim(`Cleaned ${cleanedNativelibs.binaries} non-Linux binar(ies)`);
  }

  const pruned = cleanedNativelibs.addonFiles
    + cleanedNativelibs.platformDirs
    + cleanedNativelibs.emptyDirs;
  if (pruned) logger.dim(`Cleaned ${pruned} build-only addon path(s)`);
  if (cleanedSources) logger.dim(`Cleaned ${cleanedSources} build-only file(s)/dir(s)`);
}

if (require.main === module) {
  main();
}

module.exports = {
  main,
  cleanNativelibs,
  pruneAddonBuildFiles,
  pruneNonLinuxPlatformDirs,
  removeEmptyDirs,
  findNonLinuxBinaries,
  sweep,
};
