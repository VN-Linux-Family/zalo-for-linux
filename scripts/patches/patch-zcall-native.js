/**
 * patch-zcall-native.js
 *
 * Runs Zalo's calls on the standalone native Linux Rust engine.
 *
 * Zalo's main process starts a call helper (ZaloCall.exe on Windows,
 * ZaloHelper.app on macOS) and talks to it over two local channels carrying
 * AES-encrypted JSON: named pipes on Windows, unix sockets on macOS. On Linux
 * this patch makes it
 *   - spawn the standalone Rust executable at ../native/zcall/zcall,
 *     resolved relative to this patched main-dist/main.js file;
 *   - deploy the standalone Rust executable under app/native/zcall.
 * Zalo continues to own helper startup, process lifecycle and encrypted
 * call-v2 socket transport. The plugin's existing authenticated loopback TCP
 * socket remains the UI protocol. Windows and macOS keep their branches.
 *
 * Also fixes two bugs of the non-Windows path that the helper exposes:
 * makeCall racing the init message, and a send queue that stalls after the
 * first message.
 */

const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_DIR, APP_MAIN_DIST, ZCALL_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const MAIN_JS = path.join(APP_MAIN_DIST, 'main.js');

// [what, from, to]: `from` is a string or a RegExp on Zalo's minified
// main-dist/main.js. `n("...")` is the bundle's require, `i` its spawn.
//
// Minifier variable names (win32 flag, ports, servers, child process, queues)
// vary across Zalo releases. Patterns use dynamic identifier matching ([$\w]+)
// anchored to unchanging string literals/telemetry tags to remain resilient.
function getSteps(content) {
 const mG = content.match(/([$\w]+)\.push\.apply\(\1,[$\w]+\),[$\w]+\([$\w]+\)\}/);
 const queueVar = mG ? mG[1] : 'x';

  return [
    ['unix socket paths with TMPDIR support',
      /([$\w]+)="win32"===n\("([^"]+)"\)\.platform\(\),([$\w]+)=\1\?"\\\\\\\\.\\\\pipe\\\\PipeZCallSend":"\/tmp\/socketzalosend2021",([$\w]+)=\1\?"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv":"\/tmp\/socketzalorecv2021"/,
      '$1="win32"===n("$2").platform(),' +
      '$3=$1?"\\\\\\\\.\\\\pipe\\\\PipeZCallSend":(process.env.TMPDIR&&(()=>{try{a.mkdirSync(process.env.TMPDIR,{recursive:!0})}catch(_){}})(),(process.env.TMPDIR||"/tmp").replace(/\\/+$/,"")+"/socketzalosend2021"),' +
      '$4=$1?"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv":(process.env.TMPDIR||"/tmp").replace(/\\/+$/,"")+"/socketzalorecv2021"'],
    ['engine path on Linux',
      /:\(([$\w]+)=[$\w]+\(\)\?[$\w]+\.join\(__dirname,"..","native","qt-call-cap-mac","ZaloHelper\.app"\)/,
      ':("linux"===process.platform?$1=o.join(__dirname,"..","native","zcall","zcall"):($1=u()?o.join(__dirname,"..","native","qt-call-cap-mac","ZaloHelper.app")'],
    ['engine path on Linux (close)',
      /([$\w]+=[$\w]+\.join\([$\w]+,"Contents","MacOS","ZaloCall"\)\)),([$\w]+)\}\(\);/,
      '$1),$2}();'],
    // A failed or ended engine must not leave "started" set, or no call
    // would start it again until Zalo restarts.
    ['restart after the engine ends',
      /([$\w]+)\.on\("error",\((\w+)=>\{([$\w.]+)\((\d+),"([^"]+)",\["client error","([^"]+)"\],\2\)\}\)\)/,
      '$1.on("error",($2=>{L=!1,$3($4,"$5",["client error","$6"],$2)})),$1.on("exit",(()=>{L=!1}))'],
    // The helper rejects makeCall (-11 "init_error") before it has seen init:
    // send init first (same stream, order kept). $1 is the send function,
    // $2 is the cached init payload (they swap names between Zalo versions).
    ['init before makeCall',
      /\.on\("call-send-to-native",\(\(e,t\)=>\{t\._optional\?delete t\._optional:([$\w]+)\(\),([$\w]+)\(t\)\}\)\)\.on\("call-init",\(\(e,t\)=>\{t&&t\._optional&&delete t\._optional,([$\w]+)=t\}\)\)/,
      '.on("call-send-to-native",((e,t)=>{t._optional?delete t._optional:$1(),t&&"makeCall"===t.command&&$3&&$2($3),$2(t)})).on("call-init",((e,t)=>{t&&t._optional&&delete t._optional,$3=t}))'],
    // Off Windows the "sending" flag is only cleared when the helper writes on
    // the send channel, which it hardly ever does: every message after the
    // first stalled. Clear it shortly after each write.
    ['send queue stall',
      /,([$\w]+)=e=>\{if\(([$\w]+)\)if\(([$\w]+)\)[\s\S]{0,1000}?\3=!0,([$\w]+)\.write\(([$\w]+)\)/,
      '$&,setTimeout((()=>{$3=!1,$1($4)}),100)'],
    // An engine that ended leaves a destroyed socket: queue instead of writing.
    ['no write to a closed socket',
      /([$\w]+)=([$\w]+)=>\{([$\w]+)\?([$\w]+)\(([$\w]+),\2\):([$\w]+)\(\5,\2\)/,
      `$1=$2=>{$3?$4($5,$2):$5&&!$5.destroyed?$6($5,$2):${queueVar}.push($2)`],
    ['no write to a closed socket (queue)',
      /else if\(([$\w]+)\)\{if\(([$\w]+)\.length\)\{const ([$\w]+)=\2\.shift\(\);([$\w]+)\(\1,\3\)/,
      'else if($1&&!$1.destroyed){if($2.length){const $3=$2.shift();$4($1,$3)'],
  ];
}

function foundIn(content, from) {
 if (typeof from === 'string') return content.includes(from);
 from.lastIndex = 0;
 const ok = from.test(content);
 from.lastIndex = 0;
 return ok;
}

function deployZcallBinary() {
 const rootBinary = path.join(ROOT, 'target', 'release', 'zcall');
 const localBinary = path.join(ZCALL_DIR, 'target', 'release', 'zcall');
 const fallbackBinary = fs.existsSync(rootBinary) ? rootBinary : localBinary;
 const sourceBinary = resolvePrebuiltBinary('zcall', fallbackBinary);

 const appCallDir = path.join(APP_DIR, 'native', 'zcall');
 fs.ensureDirSync(appCallDir);
 const targetBin = path.join(appCallDir, 'zcall');

 if (!fs.existsSync(sourceBinary)) {
  throw new Error('Missing pre-built Rust zcall binary at: ' + sourceBinary + '. Build it with npm run build:native -- zcall.');
 }
 const stat = fs.statSync(sourceBinary);
 if (!stat.isFile()) throw new Error('Rust zcall binary is not a regular file: ' + sourceBinary);
 if (sourceBinary !== targetBin) fs.copyFileSync(sourceBinary, targetBin);
 fs.chmodSync(targetBin, 0o755);
 const deployed = fs.statSync(targetBin);
 if (!deployed.isFile() || (deployed.mode & 0o111) === 0) {
  throw new Error('Deployed Rust zcall binary is not executable: ' + targetBin);
 }
 logger.success('Deployed native Rust zcall engine to ' + path.relative(ROOT, targetBin));
 return targetBin;
}

async function main(mainJs = MAIN_JS) {
 const deployedBinary = deployZcallBinary();
 if (!fs.existsSync(mainJs)) {
  throw new Error('native call patch: main.js not found');
 }
 let content = fs.readFileSync(mainJs, 'utf8');
 const relativeBinaryMarker = 'o.join(__dirname,"..","native","zcall","zcall")';
 if (content.includes(relativeBinaryMarker)) {
  const cleaned = content.replace(/,process\.emit\("zcall-engine-start",[$\w]+\)/, '');
  if (cleaned !== content) fs.writeFileSync(mainJs, cleaned, 'utf8');
  logger.dim('native Rust call patch already applied');
  return deployedBinary;
 }
 const steps = getSteps(content);
 const missing = [];
 for (const [what, from, to] of steps) {
  if (!foundIn(content, from)) { missing.push(what); continue; }
  content = typeof from === 'string' ? content.split(from).join(to) : content.replace(from, to);
 }
 if (missing.length) {
  // A partial patch would leave calls half wired: keep main.js as it was.
  throw new Error('native call patch not applied, Zalo changed: ' + missing.join('; '));
 }
 fs.writeFileSync(mainJs, content, 'utf8');
 logger.success('native Rust call patch applied');
 return deployedBinary;
}

if (require.main === module) main(process.argv[2]);

module.exports = { main, getSteps, deployZcallBinary };
