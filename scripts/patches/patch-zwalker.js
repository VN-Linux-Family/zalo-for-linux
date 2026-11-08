// Original copyright
// This file is derived from: https://github.com/thotam/zalo-linux
// Copyright (c) 2026 thotam <thanhtamtqno1@gmail.com>
// Licensed under the MIT License.
// ---------------------------------------------------------------------------
// Intergation copyright
// Copyright (c) 2026 doandat943 and Việt Nam Linux Family
// Licensed under the BSD 3-Clause License.

// ---------------------------------------------------------------------------
// Linux port of the `zwalker` NAPI-RS Rust addon (storage garbage collector for the
// media download directory). Reconstructed from scratch (nativelibs/zwalker) from the
// mac binary's leaked crate layout + struct fields + the JS facade/orchestrator that
// drives it (see nativelibs/zwalker/RE-PARAMS.md).
//
//   1. cargo build --release -> target/release/libzwalker.so (N-API, ABI-stable),
//   2. deploy it as zwalker.linux-x64-gnu.node — the exact slot the addon's own
//      auto-generated napi loader (index.js) already probes on linux/glibc. No index.js
//      splice is needed: the loader require()s ./zwalker.linux-x64-gnu.node when present,
//      so simply dropping the real binary in place lights up all 5 functions and the
//      `if (!nativeBinding)` fallthrough is never reached.
//   3. gate: ELF + ldd shows only base system libs (Rust statically links everything
//      else) + the module actually loads and exports the 5 expected functions.
//
// NB: patch-linux-guards no longer touches zwalker — with the real binding present, the
// stub it used to inject would be dead code. This is the "real addon owns its own load"
// split (same as mp4thumb), leaving linux-guards to only short-circuit codesign.
// ---------------------------------------------------------------------------


const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const ZWALKER_DIR = path.join(NATIVELIBS_DIR, 'zwalker');

const EXPECTED_EXPORTS = [
  'scanDirectory',
  'updateReferenceMessageId',
  'statUnmarkedFiles',
  'deleteHomelessFiles',
  'deleteEmptyFolders',
];

async function main() {
  logger.info('Installing zwalker native addon...');

  const fallbackSo = path.join(ZWALKER_DIR, 'target', 'release', 'zwalker.node');
  const builtSo = resolvePrebuiltBinary('zwalker.node', fallbackSo);

  if (!fs.existsSync(builtSo)) {
    throw new Error(`Missing pre-built binary for zwalker at: ${builtSo}. Please run "npm run build:native" first.`);
  }

  const destDir = path.join(APP_NATIVE_LIBS, 'zwalker');
  fs.ensureDirSync(destDir);
  const destNode = path.join(destDir, 'zwalker.linux-x64-gnu.node');

  fs.copyFileSync(builtSo, destNode);
  logger.dim('Installed zwalker.linux-x64-gnu.node');

  // Post-conditions & validations
  if (!fs.existsSync(destNode) || fs.statSync(destNode).size === 0) {
    throw new Error('patch-zwalker: .node missing/empty');
  }
  const magic = Buffer.alloc(4);
  const fd = fs.openSync(destNode, 'r');
  fs.readSync(fd, magic, 0, 4, 0);
  fs.closeSync(fd);
  if (!(magic[0] === 0x7f && magic[1] === 0x45 && magic[2] === 0x4c && magic[3] === 0x46)) {
    throw new Error('patch-zwalker: .node is not an ELF file');
  }

  const ldd = execSync(`ldd "${destNode}"`).toString();
  const bad = ldd.split('\n').filter((l) => /=>/.test(l) && !/linux-vdso/.test(l) &&
    !/\/(lib|lib64|usr\/lib)\S*\/(libc|libgcc_s|libm|libpthread|libdl|librt|ld-linux)/.test(l));
  if (bad.length) {
    throw new Error(`patch-zwalker: unexpected non-system deps:\n${bad.join('\n')}`);
  }

  const probe = path.join(require('os').tmpdir(), `zwalker-probe-${process.pid}.node`);
  fs.copyFileSync(destNode, probe);
  try {
    const mod = require(probe);
    for (const fn of EXPECTED_EXPORTS) {
      if (typeof mod[fn] !== 'function') {
        throw new Error(`patch-zwalker: addon does not export ${fn}()`);
      }
    }
  } finally {
    fs.removeSync(probe);
  }

  logger.success('zwalker built and installed');
}

if (require.main === module) {
  main().catch((e) => {
    logger.error(e.message);
    process.exit(1);
  });
}

module.exports = { main };