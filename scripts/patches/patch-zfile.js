const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const LIB_DIR = path.join(NATIVELIBS_DIR, 'zfile');
const DEST_DIR = path.join(APP_NATIVE_LIBS, 'zfile', 'linux');
const INDEX_JS = path.join(APP_NATIVE_LIBS, 'zfile', 'index.js');

async function main() {
  logger.info('Installing zfile native addon...');

  const rootFallback = path.join(ROOT, 'target', 'release', 'zfile-native.node');
  const localFallback = path.join(LIB_DIR, 'target', 'release', 'zfile-native.node');
  const fallbackNode = fs.existsSync(rootFallback) ? rootFallback : localFallback;
  const builtNode = resolvePrebuiltBinary('zfile-native.node', fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for zfile at: ${builtNode}. Please run "npm run build:native" first.`);
  }
  fs.ensureDirSync(DEST_DIR);
  const destNode = path.join(DEST_DIR, 'zfile-native.node');
  fs.copyFileSync(builtNode, destNode);
  logger.dim('Installed Linux zfile-native.node');

  // Install the JS wrapper that resolves getDiskInfo() lookups by absolute path.
  // zfile IS used by the renderer's Data-Management screen, which looks up disk
  // info BY PATH (e.g. the user-data dir) via its own `formatDrivePath`. That
  // function is Windows-only (returns the path unchanged on Linux), and our native
  // getDiskInfo() is keyed by MOUNT POINT — so a bare `diskInfo()["/home/user/..."]`
  // returns undefined -> `undefined.label` throws and the screen hangs (verified).
  // The wrapper returns a Proxy whose get-trap resolves any absolute path to its
  // longest-prefix mount entry (verified correct for data on "/" and on other
  // mounts like "/mnt/data"). Enumeration (Object.keys/entries) still yields the
  // real mount points, so the drive-list bar keeps working.
  const wrapperFallback = path.join(LIB_DIR, 'zfile-linux.js');
  const wrapperSrc = resolvePrebuiltBinary('zfile-linux.js', wrapperFallback);
  const destWrapper = path.join(DEST_DIR, 'zfile-linux.js');

  if (!fs.existsSync(wrapperSrc)) {
    throw new Error(
      `Missing zfile-linux.js in prebuilt and fallback paths. Checked: ${wrapperSrc}`
    );
  }

  fs.copyFileSync(wrapperSrc, destWrapper);
  logger.dim(`Installed Linux zfile-linux.js wrapper from: ${wrapperSrc}`);

  // Splice a linux branch into index.js that requires the WRAPPER (not the raw .node).
  if (!fs.existsSync(INDEX_JS)) {
    throw new Error('zfile/index.js not found — did extraction overlay app.asar.unpacked?');
  }
  let c = fs.readFileSync(INDEX_JS, 'utf8');
  if (!c.includes("process.platform === 'linux'")) {
    const before = c;
    c = c.replace(
      /\}\s*else\s*\{\s*return\s*\{\s*\n?\s*stat:/,
      "}else if(process.platform === 'linux'){\n        addon = require('./linux/zfile-linux.js');\n    }else{\n        return {\n            stat:"
    );
    if (c === before) {
      throw new Error("patch-zfile: could not insert linux branch — zfile/index.js format changed, update the regex");
    }
    fs.writeFileSync(INDEX_JS, c, 'utf8');
    logger.dim('Patched zfile/index.js with linux branch (requires zfile-linux.js wrapper)');
  } else {
    logger.dim('zfile/index.js already has linux branch');
  }

  // Post-conditions: native lib, wrapper, and branch must all be present.
  if (!fs.existsSync(destNode) || fs.statSync(destNode).size === 0) {
    throw new Error(`patch-zfile: post-condition failed — ${destNode} missing/empty`);
  }
  if (!fs.existsSync(destWrapper) || fs.statSync(destWrapper).size === 0) {
    throw new Error(`patch-zfile: post-condition failed — ${destWrapper} missing/empty`);
  }
  const after = fs.readFileSync(INDEX_JS, 'utf8');
  if (!after.includes("process.platform === 'linux'")) {
    throw new Error('patch-zfile: post-condition failed — linux branch not present in index.js');
  }
  // Load index.js the way the app does and assert diskInfo() works.
  delete require.cache[require.resolve(INDEX_JS)];
  const zfile = require(INDEX_JS);
  const disks = await zfile.diskInfo();
  if (!disks || typeof disks !== 'object' || Array.isArray(disks)) {
    throw new Error('patch-zfile: post-condition failed — zfile.diskInfo() returned invalid data');
  }
  const availableMounts = Object.keys(disks);
  if (availableMounts.length > 0) {
    // Get a mount point that found, then probe a deep nonexistent path under it. The wrapper should resolve that path to the mount point.
    const baseMount = availableMounts[0];
    const testPath = path.join(baseMount, 'deep/nonexistent/path/for/postcondition');

    const probe = disks[testPath];
    if (!probe || typeof probe.totalSpace !== 'number' || probe.totalSpace <= 0) {
      throw new Error('patch-zfile: post-condition failed — diskInfo() Proxy did not resolve an absolute path to a mount entry');
    }
  }
  logger.success('zfile built and installed');
}

if (require.main === module) main().catch(e => { logger.error(e.message); process.exit(1); });
module.exports = { main };