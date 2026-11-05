const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const ROOT = path.join(__dirname, '..', '..');
const APP_DIR = path.join(ROOT, 'app');
const NATIVELIBS_DIR = path.join(ROOT, 'nativelibs');
const BUILDER_SCRIPT = path.join(NATIVELIBS_DIR, 'builder.js');
const LIB_DIR = path.join(NATIVELIBS_DIR, 'zfile');
const DEST_DIR = path.join(APP_DIR, 'native', 'nativelibs', 'zfile', 'linux');
const INDEX_JS = path.join(APP_DIR, 'native', 'nativelibs', 'zfile', 'index.js');

async function main() {
  if (!fs.existsSync(path.join(LIB_DIR, 'binding.gyp'))) {
    throw new Error('zfile source not found at nativelibs/zfile — cannot build');
  }

  const releaseDir = path.join(LIB_DIR, 'build', 'Release');
  const alreadyBuilt = fs.existsSync(releaseDir) && fs.readdirSync(releaseDir).some(f => f.endsWith('.node'));

  if (!alreadyBuilt || process.env.FORCE_BUILD === 'true') {
    logger.info('Building zfile-native from source...');
    try {
      execSync(`node "${BUILDER_SCRIPT}" "${LIB_DIR}"`, {
        cwd: ROOT,
        stdio: 'pipe'
      });
    } catch (error) {
      logger.error('Failed to build zfile-native', error.message);
      if (error.stdout) logger.dim(error.stdout.toString());
      throw new Error('Failed to build zfile-native');
    }
  } else {
    logger.dim('zfile-native already built, skipping compilation');
  }

  const nodeFiles = fs.readdirSync(releaseDir).filter(f => f.endsWith('.node'));
  if (nodeFiles.length === 0) throw new Error('zfile build produced no .node');

  fs.ensureDirSync(DEST_DIR);
  const destNode = path.join(DEST_DIR, 'zfile-native.node');
  fs.copyFileSync(path.join(releaseDir, nodeFiles[0]), destNode);
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
  const wrapperSrc = path.join(LIB_DIR, 'zfile-linux.js');
  const destWrapper = path.join(DEST_DIR, 'zfile-linux.js');
  if (!fs.existsSync(wrapperSrc)) {
    throw new Error('zfile wrapper source not found at nativelibs/zfile/zfile-linux.js');
  }
  fs.copyFileSync(wrapperSrc, destWrapper);
  logger.dim('Installed Linux zfile-linux.js wrapper');

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
  if (!disks || typeof disks !== 'object' || Object.keys(disks).length === 0) {
    throw new Error('patch-zfile: post-condition failed — zfile.diskInfo() returned no drives');
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