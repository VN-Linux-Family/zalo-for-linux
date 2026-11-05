/**
 * patch-zcall-callgate.js
 *
 * Fixes the call-button gate that accidentally disables calls on Linux.
 *
 * Zalo's call controller computes a support flag from the OS release version:
 *
 *   let ae=()=>{{
 *     const e=$znode.os.release();
 *     if(e){ if(Number(e.split(".")[0])<17) return ae=()=>!1,!1 }
 *   }
 *   return ae=()=>!0,ae()};
 *   const ie=ae();   // -> isSupport() uses this
 *
 * This was written for macOS (Darwin kernel >= 17), but on Linux
 * os.release() is the Linux kernel version (e.g. "7.0.0-30-generic"),
 * so Number("7") < 17 makes the gate false and the call button stays
 * disabled / makeCall() hits the "[zcall-v2] Call is not supported!" stub.
 *
 * Patch: skip the version check on Linux, keep it intact on other platforms.
 */

const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');

const PC_DIST = path.join(__dirname, '..', '..', 'app', 'pc-dist');

// Hash part of the filenames changes per Zalo version — glob by prefix.
const GLOB_TARGETS = [
 path.join(PC_DIST, 'compact-app-pc.*.js'),
 path.join(PC_DIST, 'search-worker.*.js'),
 path.join(PC_DIST, 'sync-v2-sub-worker.*.js'),
 path.join(PC_DIST, 'lazy', 'default-login-main-startup-shared-worker-znotification.*.js'),
];

const ORIGINAL = 'let ae=()=>{{const e=$znode.os.release();if(e){if(Number(e.split(".")[0])<17)return ae=()=>!1,!1}}return ae=()=>!0,ae()};';
const PATCHED = 'let ae=()=>{{const e=$znode.os.release();if(e){if("linux"!==$znode.os.platform()&&Number(e.split(".")[0])<17)return ae=()=>!1,!1}}return ae=()=>!0,ae()};';

// Static feature defaults have calls disabled (enableCall:!1, enableVideoCall:!1);
// the server may override them via settings.chat.enable_call, but if it does
// not send them, flip the defaults so calls are available out of the box.
const DEFAULTS_ORIGINAL = 'enableCall:!1,enableTag:!0,enableVideoCall:!1';
const DEFAULTS_PATCHED = 'enableCall:!0,enableTag:!0,enableVideoCall:!0';

async function main() {
 const missing = [];
 let patchedCount = 0;

 for (const pattern of GLOB_TARGETS) {
  // resolve the first matching file (hashes change per Zalo version)
  const dir = path.dirname(pattern);
  const base = path.basename(pattern);
  const [prefix, suffix] = base.split('*');
  const file = fs.existsSync(dir)
   ? (fs.readdirSync(dir).find(f => f.startsWith(prefix) && f.endsWith(suffix || '')) || null)
   : null;

  if (!file) {
   missing.push(path.basename(pattern));
   continue;
  }
  const filePath = path.join(dir, file);

  let content = fs.readFileSync(filePath, 'utf8');
  let changed = false;

  if (content.includes('"linux"!==$znode.os.platform()') || content.includes('"linux" !== $znode.os.platform()')) {
   // already patched
  } else {
   let astPatched = false;
   try {
    const root = parseAst(content);
    const edits = [];
    // 1. Kernel gate: Number(e.split(".")[0]) < 17
    const kernelMatch = root.find('Number($E.split(".")[0]) < 17');
    if (kernelMatch) {
     edits.push(kernelMatch.replace(`("linux"!==$znode.os.platform()&&${kernelMatch.text()})`));
    }
    // 2. Default feature toggles
    const callPair = root.findAll('!1').find(n => n.parent().kind() === 'pair' && n.parent().field('key').text() === 'enableCall');
    if (callPair) edits.push(callPair.parent().replace('enableCall:!0'));
    const videoPair = root.findAll('!1').find(n => n.parent().kind() === 'pair' && n.parent().field('key').text() === 'enableVideoCall');
    if (videoPair) edits.push(videoPair.parent().replace('enableVideoCall:!0'));

    if (edits.length > 0) {
     content = applyEdits(root, edits);
     changed = true;
     astPatched = true;
     logger.dim(`Patched callgate via AST in ${file} (${edits.length} edits)`);
    }
   } catch (err) {
    logger.warn(`AST callgate patch skipped on ${file}, falling back:`, err.message);
   }

   if (!astPatched) {
    if (content.includes(ORIGINAL)) {
     content = content.split(ORIGINAL).join(PATCHED);
     changed = true;
    } else {
     missing.push(file + ': kernel gate');
     continue;
    }
    if (content.includes(DEFAULTS_ORIGINAL)) {
     content = content.split(DEFAULTS_ORIGINAL).join(DEFAULTS_PATCHED);
     changed = true;
    }
   }
  }
  if (changed) {
   fs.writeFileSync(filePath, content, 'utf8');
   logger.dim('callgate patched: ' + file);
  } else {
   logger.dim('callgate already patched: ' + file);
  }
  patchedCount++;
 }

 if (missing.length || patchedCount === 0) {
  throw new Error('callgate patch not applied: ' + (missing.join('; ') || 'no targets'));
 }
 logger.success(`zcall callgate patched (${patchedCount} files)`);
}

if (require.main === module) {
 main();
}

module.exports = { main };
