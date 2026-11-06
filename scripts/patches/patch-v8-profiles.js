// Original copyright
// This file is derived from: https://github.com/thotam/zalo-linux
// Copyright (c) 2026 thotam <thanhtamtqno1@gmail.com>
// Licensed under the MIT License.
// ---------------------------------------------------------------------------
// Intergation copyright
// Copyright (c) 2026 doandat943 and Việt Nam Linux Family
// Licensed under the BSD 3-Clause License.

// The v8-profiles addon is raw-V8 (NODE_MODULE, NOT N-API), so it loads only under
// the exact Electron ABI it was built against. We splice a linux branch into the
// binding require chain BEFORE the mac fallback; the linux-guards try/catch that
// wraps this expression stays as a defensive stub if the .node ever fails to load.

const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_DIR, APP_NATIVE_LIBS, NATIVELIBS_DIR, BUILDER_SCRIPT } = require('../utils/paths');
const V8_PROFILES_DIR = path.join(NATIVELIBS_DIR, 'v8-profiles');
const DEST_NODE = path.join(APP_NATIVE_LIBS, 'v8-profiles', 'profiler_electron_linux_x64.node');
const INDEX_JS = path.join(APP_NATIVE_LIBS, 'v8-profiles', 'index.js');

const MAC_REQUIRE = "require('./profiler_electron1.8_mac.node')";
const LINUX_REQUIRE =
  "process.platform === 'linux' ? require('./profiler_electron_linux_x64.node') : require('./profiler_electron1.8_mac.node')";

async function main() {
  logger.info('Building v8-profiles from source...');

  if (!fs.existsSync(path.join(V8_PROFILES_DIR, 'binding.gyp'))) {
    throw new Error('v8-profiles source not found at nativelibs/v8-profiles — cannot build');
  }

  const releaseDir = path.join(V8_PROFILES_DIR, 'build', 'Release');
  const alreadyBuilt = fs.existsSync(releaseDir) && fs.readdirSync(releaseDir).some(f => f.endsWith('.node'));

  if (!alreadyBuilt || process.env.FORCE_BUILD === 'true') {
    try {
      execSync(`node "${BUILDER_SCRIPT}" "${V8_PROFILES_DIR}"`, {
        cwd: ROOT,
        stdio: 'pipe'
      });
    } catch (error) {
      logger.error('Failed to build v8-profiles', error.message);
      if (error.stdout) logger.dim(error.stdout.toString());
      throw new Error('Failed to build v8-profiles');
    }
  } else {
    logger.dim('v8-profiles already built, skipping compilation');
  }

  const nodeFiles = fs.readdirSync(releaseDir).filter(f => f.endsWith('.node'));
  if (nodeFiles.length === 0) throw new Error('v8-profiles build produced no .node');

  fs.ensureDirSync(path.dirname(DEST_NODE));
  fs.copyFileSync(path.join(releaseDir, nodeFiles[0]), DEST_NODE);
  logger.dim('Installed Linux profiler_electron_linux_x64.node');

  if (!fs.existsSync(INDEX_JS)) {
    throw new Error('v8-profiles/index.js not found — did extraction overlay app.asar.unpacked?');
  }
  let c = fs.readFileSync(INDEX_JS, 'utf8');
  if (c.includes("require('./profiler_electron_linux_x64.node')")) {
    logger.dim('v8-profiles/index.js already has linux branch');
  } else {
    if (!c.includes(MAC_REQUIRE)) {
      throw new Error('patch-v8-profiles: anchor ' + MAC_REQUIRE + ' not found in index.js — bundle format changed, update the patch');
    }
    c = c.replace(MAC_REQUIRE, LINUX_REQUIRE);
    fs.writeFileSync(INDEX_JS, c, 'utf8');
    logger.dim('Patched v8-profiles/index.js with linux branch');
  }

  // Post-conditions (fail loud). The .node is Electron-ABI (raw V8), so we do NOT
  // require() it here under plain Node — that would be a false ABI-mismatch failure;
  // the real load test runs under Electron (smoke boot).
  if (!fs.existsSync(DEST_NODE) || fs.statSync(DEST_NODE).size === 0) {
    throw new Error(`patch-v8-profiles: post-condition failed — ${DEST_NODE} missing/empty`);
  }
  const after = fs.readFileSync(INDEX_JS, 'utf8');
  if (!after.includes("require('./profiler_electron_linux_x64.node')")) {
    throw new Error('patch-v8-profiles: post-condition failed — linux require not present in index.js');
  }

  logger.success('v8-profiles built and installed');
}

if (require.main === module) {
  main().catch(e => { logger.error(e.message); process.exit(1); });
}

module.exports = { main };