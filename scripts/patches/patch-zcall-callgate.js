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
const { spawnSync } = require('child_process');
const logger = require('../utils/logger');
const { parseAst, applyEdits, extractEnclosingBlock } = require('../utils/ast-transform');

const { APP_PC_DIST: PC_DIST, resolveLazyLoginBundles } = require('../utils/paths');

// Hash part of the filenames changes per Zalo version — glob by prefix.
const GLOB_TARGETS = [
 path.join(PC_DIST, 'compact-app-pc.*.js'),
 path.join(PC_DIST, 'search-worker.*.js'),
 path.join(PC_DIST, 'sync-v2-sub-worker.*.js'),
 ...(resolveLazyLoginBundles(PC_DIST).length > 0
  ? resolveLazyLoginBundles(PC_DIST)
  : [path.join(PC_DIST, 'lazy', 'default-login-main-startup-shared-worker-znotification.*.js')]),
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
 const workerFile = process.env.ZALO_CALLGATE_PATCH_FILE;

 for (const pattern of workerFile ? [workerFile] : GLOB_TARGETS) {
  // resolve the first matching file (hashes change per Zalo version)
  const dir = path.dirname(pattern);
  const base = path.basename(pattern);
  const [prefix, suffix] = base.split('*');
  const file = workerFile
   ? (fs.existsSync(pattern) ? base : null)
   : (fs.existsSync(dir)
    ? (fs.readdirSync(dir).find(f => f.startsWith(prefix) && f.endsWith(suffix || '')) || null)
    : null);

  if (!file) {
   missing.push(path.basename(pattern));
   continue;
  }
  const filePath = path.join(dir, file);

  if (!workerFile) {
   const result = spawnSync(process.execPath, [__filename], {
    stdio: 'inherit',
    env: { ...process.env, ZALO_CALLGATE_PATCH_FILE: filePath },
   });
   if (result.error) throw result.error;
   if (result.status !== 0) {
    const reason = result.signal ? `signal ${result.signal}` : `exit code ${result.status}`;
    throw new Error(`callgate patch failed for ${file} (${reason})`);
   }
   patchedCount++;
   continue;
  }

  let content = fs.readFileSync(filePath, 'utf8');
  let changed = false;

  if (content.includes('"linux"!==$znode.os.platform()') || content.includes('"linux" !== $znode.os.platform()')) {
   // already patched
  } else {
   let astPatched = false;
   try {
    const snippetRes = patchCallgateViaSnippetAst(content);
    if (snippetRes.editCount > 0) {
     content = snippetRes.content;
     changed = true;
     astPatched = true;
     logger.dim(`Patched callgate via Snippet AST in ${file} (${snippetRes.editCount} edits)`);
    } else {
     const root = parseAst(content);
     const edits = [];
     // 1. Kernel gate: Number(e.split(".")[0]) < 17
     const kernelMatch = root.find('Number($E.split(".")[0]) < 17');
     if (kernelMatch) {
      edits.push(kernelMatch.replace(`("linux"!==$znode.os.platform()&&${kernelMatch.text()})`));
     }
     // 2. Default feature toggles (scoped pair query instead of scanning all !1 literals)
     const callPair = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableCall$' } } })
      .find(p => p.field('value')?.text() === '!1');
     if (callPair) edits.push(callPair.replace('enableCall:!0'));
     const videoPair = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableVideoCall$' } } })
      .find(p => p.field('value')?.text() === '!1');
     if (videoPair) edits.push(videoPair.replace('enableVideoCall:!0'));

     if (edits.length > 0) {
      content = applyEdits(root, edits);
      changed = true;
      astPatched = true;
      logger.dim(`Patched callgate via AST in ${file} (${edits.length} edits)`);
     }
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
 if (!workerFile) logger.success(`zcall callgate patched (${patchedCount} files)`);
}

function patchCallgateViaSnippetAst(content) {
 let updated = content;
 let editCount = 0;

 const kernelIdx = updated.indexOf('split(".")[0])<17');
 if (kernelIdx !== -1) {
  const block = extractEnclosingBlock(updated, kernelIdx, { levels: 1 });
  if (block) {
   const root = parseAst(block.snippet);
   const kernelMatch = root.find('Number($E.split(".")[0]) < 17');
   if (kernelMatch) {
    const patched = applyEdits(root, [
     kernelMatch.replace(`("linux"!==$znode.os.platform()&&${kernelMatch.text()})`),
    ]);
    updated = updated.slice(0, block.start) + patched + updated.slice(block.end);
    editCount++;
   }
  }
 }

 const toggleIdx = updated.indexOf('enableCall:');
 if (toggleIdx !== -1) {
  const block = extractEnclosingBlock(updated, toggleIdx, { levels: 1 });
  if (block) {
   const root = parseAst(block.snippet);
   const edits = [];
   const callPair = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableCall$' } } })
    .find(p => p.field('value')?.text() === '!1');
   if (callPair) edits.push(callPair.replace('enableCall:!0'));
   const videoPair = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableVideoCall$' } } })
    .find(p => p.field('value')?.text() === '!1');
   if (videoPair) edits.push(videoPair.replace('enableVideoCall:!0'));
   if (edits.length > 0) {
    const patched = applyEdits(root, edits);
    updated = updated.slice(0, block.start) + patched + updated.slice(block.end);
    editCount += edits.length;
   }
  }
 }

 return { content: updated, editCount };
}

function collectCallgateEdits(content, root) {
 if (content.includes('"linux"!==$znode.os.platform()') || content.includes('"linux" !== $znode.os.platform()')) {
  return { edits: [], alreadyPatched: true };
 }
 const edits = [];
 // 1. Kernel gate: Number(e.split(".")[0]) < 17
 const kernelMatch = root.find('Number($E.split(".")[0]) < 17');
 if (kernelMatch) {
  edits.push(kernelMatch.replace(`("linux"!==$znode.os.platform()&&${kernelMatch.text()})`));
 }
 // 2. Default feature toggles (scoped pair query instead of scanning all !1 literals)
 const callPair = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableCall$' } } })
  .find(p => p.field('value')?.text() === '!1');
 if (callPair) edits.push(callPair.replace('enableCall:!0'));
 const videoPair = root.findAll({ rule: { kind: 'pair', has: { field: 'key', regex: '^enableVideoCall$' } } })
  .find(p => p.field('value')?.text() === '!1');
 if (videoPair) edits.push(videoPair.replace('enableVideoCall:!0'));

 return { edits, alreadyPatched: false };
}

function resolveGlobTarget(pattern) {
 const dir = path.dirname(pattern);
 const base = path.basename(pattern);
 const [prefix, suffix] = base.split('*');
 const file = fs.existsSync(dir)
  ? (fs.readdirSync(dir).find(f => f.startsWith(prefix) && f.endsWith(suffix || '')) || null)
  : null;
 return file ? path.join(dir, file) : null;
}

if (require.main === module) {
 main();
}

module.exports = {
 main,
 GLOB_TARGETS,
 collectCallgateEdits,
 patchCallgateViaSnippetAst,
 resolveGlobTarget,
 ORIGINAL,
 PATCHED,
 DEFAULTS_ORIGINAL,
 DEFAULTS_PATCHED,
};
