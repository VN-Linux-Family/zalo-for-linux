const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { spawnSync } = require('child_process');
const { parseAst, applyEdits } = require('../utils/ast-transform');

const { APP_DIR } = require('../utils/paths');

/**
 * Keep ZaDark loaded when Zalo switches to its dark theme (fixes #66).
 *
 * Zalo's dark theme component calls a helper that removes every
 * link[href^="zadark"] and script[src^="zadark"] from the page (the selectors
 * are spelled out character by character). With the desktop in dark mode
 * (patch-auto-theme follows it), ZaDark lost its CSS and icon font right
 * after startup: its sidebar button stayed as an empty slot and the message
 * translation was gone.
 *
 * The helper is found by that character array and turned into a no-op.
 * Only applied to the ZaDark variant (from build.js integrateZaDark).
 */
const REMOVER_RE = new RegExp(
 '(\\b[\\w$]+=\\(\\)=>\\{)' +
 '(var [\\w$,]+;const [\\w$]+=\\["l","i","n","k","\\[","h","r","e","f","\\^","=",\'"\',"z","a","d","a","r","k",\'"\',"\\]"\\]\\.join\\(""\\))',
 'g'
);
const MARKER = '"z","a","d","a","r","k"';
const MARKER_BUF = Buffer.from(MARKER);

function listJsFiles(dir) {
 let files = [];
 for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
  const full = path.join(dir, entry.name);
  if (entry.isDirectory()) files = files.concat(listJsFiles(full));
  else if (entry.name.endsWith('.js')) files.push(full);
 }
 return files;
}

function patchFile(file, pcDistDir) {
 const raw = fs.readFileSync(file);
 if (!raw.includes(MARKER_BUF)) return 'skipped';
 const content = raw.toString('utf8');
 if (content.includes('return;var') || content.includes('return;const') || content.includes('=()=>{return;')) {
  return 'already';
 }

 let updated = content.replace(REMOVER_RE, '$1return;$2');
 if (updated === content) {
  try {
   const root = parseAst(content);
   const zadarkLiteral = root.find('"zadark"');
   if (zadarkLiteral) {
    let fn = zadarkLiteral;
    while (fn && fn.kind() !== 'function_declaration' && fn.kind() !== 'arrow_function' && fn.kind() !== 'function') {
     fn = fn.parent();
    }
    if (fn) {
     const body = fn.field('body');
     const edit = body.replace(`{return;${body.text().slice(1)}`);
     updated = applyEdits(root, [edit]);
     logger.dim(`Neutralized ZaDark remover via AST in ${path.basename(file)}`);
    }
   }
  } catch (err) {
   logger.warn(`AST zadark-keep failed on ${path.basename(file)}, fallback unavailable:`, err.message);
  }
 }

 if (updated === content) return 'unmatched';
 fs.writeFileSync(file, updated, 'utf8');
 logger.dim(`Kept ZaDark in dark theme: ${path.relative(pcDistDir, file)}`);
 return 'patched';
}

function hasMarker(file) {
 return fs.readFileSync(file).includes(MARKER_BUF);
}

function patchFileIsolated(file, pcDistDir) {
 const runner = 'const status = require(process.argv[1]).patchFile(process.argv[2], process.argv[3]); process.exitCode = status === "patched" ? 10 : status === "unmatched" ? 11 : 0;';
 const result = spawnSync(process.execPath, ['-e', runner, __filename, file, pcDistDir], {
  stdio: 'inherit',
  env: process.env,
 });
 if (result.error) throw result.error;
 if (result.status !== 0 && result.status !== 10 && result.status !== 11) throw new Error(`ZaDark keep patch failed for ${path.basename(file)} (exit code ${result.status})`);
 return result.status === 10 ? 'patched' : result.status === 11 ? 'unmatched' : 'skipped';
}

async function main(appDir = APP_DIR) {
 const pcDistDir = path.join(appDir, 'pc-dist');
 if (!fs.existsSync(pcDistDir)) {
  logger.warn('pc-dist not present, skipping ZaDark keep patch');
  return;
 }

 let patched = 0;
 let unmatched = 0;
 for (const file of listJsFiles(pcDistDir)) {
  if (!hasMarker(file)) continue;
  const status = patchFileIsolated(file, pcDistDir);
  if (status === 'patched') patched++;
  else if (status === 'unmatched') unmatched++;
 }

 if (unmatched > 0) {
  logger.warn(`ZaDark remover found in ${unmatched} file(s) but its pattern changed; ZaDark may unload in dark theme`);
 } else if (patched > 0) {
  logger.success(`ZaDark kept in Zalo's dark theme (${patched} files)`);
 }
}

if (require.main === module) {
 main();
}

module.exports = { main, patchFile };
