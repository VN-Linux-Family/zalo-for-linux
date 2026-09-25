/**
 * patch-zcall-wine-tuning.js
 *
 * Hardens the Linux ZaloCall.exe-under-Wine spawn in main-dist/main.js.
 * Works on top of either Linux spawn variant (patch-zcall-callv2.js or the
 * env-aware `_we` variant) and skips steps whose anchor is absent.
 *
 *   1. WINEDLLOVERRIDES: disable winemenubuilder (no .desktop/icon churn in
 *      ~/.local/share/applications) and mscoree/mshtml (no Mono/Gecko
 *      install prompts when the prefix is fresh).
 *   2. Stale helper sweep before the first spawn of a session: a previous
 *      unclean exit leaves pipebridge.exe/ZaloCall.exe alive in the same
 *      wineserver, still owning \\.\pipe\PipeZCall*, so the new helper
 *      connects to the orphan and calls never start.
 *   3. Reset the "native started" flag when the spawn fails (wine missing),
 *      otherwise later call attempts never re-spawn until app restart.
 *   4. Re-send the init payload before every makeCall (the helper rejects
 *      makeCall with -11 "init_error" if it has not seen init yet).
 *   5. Bind the helper TCP channels to 127.0.0.1 where the spawn variant
 *      left them on all interfaces (the `_we` variant). patch-zcall-callv2.js
 *      already binds loopback, so this step is a no-op there.
 *   6. Start app/native/zcall-raise (zcall-bridge/zcall-raise.c) and stop it
 *      with the helper: it activates a new call window that GNOME left
 *      minimized or behind Zalo, which also keeps wine's window state in
 *      sync (otherwise the call UI stays frozen on screen after the call).
 */

const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const MAIN_JS = path.join(__dirname, '..', '..', 'app', 'main-dist', 'main.js');

const DLL_OVERRIDES = 'winemenubuilder.exe=d;mscoree=d;mshtml=d';
// n("QduZ") is child_process in the bundle (the spawn code uses it too).
// execFileSync: no shell, so pkill cannot match its own parent `sh -c`.
const KILL_STALE = 'try{n("QduZ").execFileSync("pkill",["-9","-f","qt-call-and-cap/(pipebridge|ZaloCall)\\\\.exe"],{stdio:"ignore",timeout:3000})}catch(_){}';

// (Re)start the native zcall-raise window helper whenever it is not running.
const WIN_SPAWN = '"linux"===process.platform&&(()=>{try{const f=global.__zcallWinHelper;if(f&&f.exitCode===null&&f.signalCode===null)return;const p=o.join(__dirname,"..","native","zcall-raise");if(!require("fs").existsSync(p))return;const c=require("child_process").spawn(p,[],{stdio:"ignore"});c.on("error",(()=>{}));global.__zcallWinHelper=c}catch(_){}})(),';
const WIN_STOP = 'stop:function(){try{global.__zcallWinHelper&&global.__zcallWinHelper.kill()}catch(_){}';

const STEPS = [
  {
    name: 'WINEDLLOVERRIDES (env-aware spawn)',
    from: 'const _we=Object.assign({},process.env,{WINEDEBUG:"-all",',
    to: `const _we=Object.assign({},process.env,{WINEDEBUG:"-all",WINEDLLOVERRIDES:process.env.WINEDLLOVERRIDES||"${DLL_OVERRIDES}",`,
    already: 'WINEDLLOVERRIDES:process.env.WINEDLLOVERRIDES||',
  },
  {
    name: 'stale helper sweep (env-aware spawn)',
    from: 'if(Bp){try{Bp.kill()}catch(_){}Bp=null}const _wb=',
    to: 'if(Bp){try{Bp.kill()}catch(_){}Bp=null}if(!A){' + KILL_STALE + '}const _wb=',
    already: 'qt-call-and-cap/(pipebridge|ZaloCall)',
  },
  {
    name: 'stale helper sweep (call-v2 spawn)',
    from: ';"linux"===process.platform?(BB||(BB=!0,',
    to: ';"linux"===process.platform?(!A&&(()=>{' + KILL_STALE + '})(),BB||(BB=!0,',
    already: 'qt-call-and-cap/(pipebridge|ZaloCall)',
  },
  {
    name: 'reset native-started flag on spawn error',
    from: /A\.on\("error",\(e=>\{d\.zsymb\((\d+),"([^"]+)",\["client error","([^"]+)"\],e\)/,
    to: 'A.on("error",(e=>{L=!1,d.zsymb($1,"$2",["client error","$3"],e)',
    already: /A\.on\("error",\(e=>\{L=!1,/,
  },
  {
    name: 'resend init before makeCall',
    from: '.on("call-send-to-native",((e,t)=>{t._optional?delete t._optional:K(),D(t)}))',
    to: '.on("call-send-to-native",((e,t)=>{t._optional?delete t._optional:K(),t&&"makeCall"===t.command&&O&&D(O),D(t)}))',
    already: '"makeCall"===t.command&&O&&D(O)',
  },
  // In the `_we` spawn variant the first listen() of each helper channel
  // bound every interface, exposing the call control ports 29631/29632 on
  // the LAN (patch-zcall-callv2.js already binds loopback, so the anchor is
  // absent there). Once patched, the anchor is gone, so no `already` check
  // is needed.
  {
    name: 'bind helper recv channel to loopback',
    from: 'I.listen(v,(',
    to: 'I.listen("linux"===process.platform?{port:v,host:"127.0.0.1"}:v,(',
  },
  {
    name: 'bind helper send channel to loopback',
    from: 'C.listen(g,(',
    to: 'C.listen("linux"===process.platform?{port:g,host:"127.0.0.1"}:g,(',
  },
  {
    name: 'start zcall-raise window helper',
    from: ':A=i(e,[v,g]),A.stdout.setEncoding("utf8")',
    to: ':A=i(e,[v,g]),' + WIN_SPAWN + 'A.stdout.setEncoding("utf8")',
    already: 'global.__zcallWinHelper',
  },
  {
    name: 'stop zcall-raise with the helper',
    from: /stop:function\(\)\{(?=if\(Bp\)|A&&A\.kill\(\)\})/,
    to: WIN_STOP,
    already: WIN_STOP,
  },
];

function isApplied(content, already) {
  if (!already) return false;
  return typeof already === 'string' ? content.includes(already) : already.test(content);
}

async function main() {
  if (!fs.existsSync(MAIN_JS)) {
    logger.warn('main.js not found, skipping zcall wine tuning');
    return;
  }
  if (process.arch === 'arm64' || process.arch === 'aarch64') {
    logger.info('skipping zcall wine tuning on arm64');
    return;
  }

  let content = fs.readFileSync(MAIN_JS, 'utf8');
  let applied = 0;
  for (const { name, from, to, already } of STEPS) {
    if (isApplied(content, already)) {
      logger.dim('zcall tuning already applied: ' + name);
      continue;
    }
    const found = typeof from === 'string' ? content.includes(from) : from.test(content);
    if (!found) {
      logger.dim('zcall tuning anchor not present, skipped: ' + name);
      continue;
    }
    content = typeof from === 'string' ? content.split(from).join(to) : content.replace(from, to);
    applied++;
    logger.dim('zcall tuning applied: ' + name);
  }

  if (applied > 0) {
    fs.writeFileSync(MAIN_JS, content, 'utf8');
    logger.success(`zcall wine tuning applied (${applied} step(s))`);
  }
}

if (require.main === module) {
  main();
}

module.exports = { main };
