#!/usr/bin/env node
"use strict";

const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_DIR, APP_NATIVE_LIBS, NATIVELIBS_DIR, BUILDER_RUST_SCRIPT: BUILDER_SCRIPT } = require('../utils/paths');
const ZLANG_DIR = path.join(NATIVELIBS_DIR, 'zlang');

const EXPECTED_EXPORTS = [
  'version',
  'backend',
  'available',
  'scores',
  'detect',
];

async function main() {
  logger.info('Building zlang from source...');

  if (!fs.existsSync(path.join(ZLANG_DIR, 'Cargo.toml'))) {
    logger.warn('zlang not found, skipping');
    return;
  }

  const releaseDir = path.join(ZLANG_DIR, 'target', 'release');
  const builtNode = path.join(releaseDir, 'zlang.node');
  const alreadyBuilt = fs.existsSync(builtNode);

  if (!alreadyBuilt || process.env.FORCE_BUILD === 'true') {
    try {
      execSync(`node "${BUILDER_SCRIPT}" "${ZLANG_DIR}"`, {
        cwd: ROOT,
        stdio: 'pipe'
      });
    } catch (error) {
      logger.error('Failed to build zlang', error.message);
      if (error.stdout) logger.dim(error.stdout.toString());
      throw new Error(`Failed to build zlang`);
    }
  } else {
    logger.dim('zlang already built, skipping compilation');
  }

  if (!fs.existsSync(builtNode)) {
    throw new Error(`zlang build produced no ${builtNode}`);
  }

  const archDir = process.arch === 'arm64' ? 'linux-arm64' : 'linux-x64';
  const destDir = path.join(APP_NATIVE_LIBS, 'zlang', archDir);
  fs.ensureDirSync(destDir);
  const destNode = path.join(destDir, `zlang.${archDir}.node`);

  fs.copyFileSync(builtNode, destNode);
  logger.dim(`Installed zlang.${archDir}.node`);

  // Verify ELF binary
  if (!fs.existsSync(destNode) || fs.statSync(destNode).size === 0) {
    throw new Error('patch-zlang: .node missing or empty');
  }
  const magic = Buffer.alloc(4);
  const fd = fs.openSync(destNode, 'r');
  fs.readSync(fd, magic, 0, 4, 0);
  fs.closeSync(fd);
  if (!(magic[0] === 0x7f && magic[1] === 0x45 && magic[2] === 0x4c && magic[3] === 0x46)) {
    throw new Error('patch-zlang: .node is not an ELF file');
  }

  // Verify exports via temporary probe
  const probe = path.join(require('os').tmpdir(), `zlang-probe-${process.pid}.node`);
  fs.copyFileSync(destNode, probe);
  try {
    const mod = require(probe);
    for (const fn of EXPECTED_EXPORTS) {
      if (typeof mod[fn] !== 'function') {
        throw new Error(`patch-zlang: addon does not export ${fn}()`);
      }
    }
  } finally {
    fs.removeSync(probe);
  }

  // Patch index.js in zlang to enable Linux loading and bypass platform check
  const indexJsPath = path.join(APP_DIR, 'native', 'nativelibs', 'zlang', 'index.js');
  if (fs.existsSync(indexJsPath)) {
    let content = fs.readFileSync(indexJsPath, 'utf8');

    // 1. Add linux platforms to switch(platform)
    if (!content.includes("'linux-x64'")) {
      content = content.replace(
        /case SupportedPlatform\.WIN32_X64:\s*load\(platform\);/m,
        `case SupportedPlatform.WIN32_X64:\n    case 'linux-x64':\n    case 'linux-arm64':\n        load(platform);`
      );
    }

    // 2. Allow linux in getUnavailableReason
    if (!content.includes("process.platform !== 'linux'")) {
      content = content.replace(
        "if (process.platform !== 'darwin' && process.platform !== 'win32')",
        "if (process.platform !== 'darwin' && process.platform !== 'win32' && process.platform !== 'linux')"
      );
    }

    fs.writeFileSync(indexJsPath, content, 'utf8');
    logger.dim('Patched zlang/index.js for Linux support');
  }

  logger.success('zlang built and installed');
}

if (require.main === module) {
  main().catch((err) => {
    logger.error('patch-zlang failed:', err.message);
    process.exit(1);
  });
}

module.exports = { main };
