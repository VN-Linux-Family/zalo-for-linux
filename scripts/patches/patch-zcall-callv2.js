/**
 * patch-zcall-callv2.js
 *
 * Makes the call-v2 helper (ZaloCall.exe) work on Linux by spawning it under
 * Wine with a named-pipe -> TCP bridge.
 *
 * Background:
 *   Zalo PC 26.x does not use the old zcall .node addon anymore. The main
 *   process spawns a Qt helper (plugins/capture/ZaloCall.exe on Windows,
 *   ZaloHelper.app on macOS) and talks to it over two local channels with
 *   AES-128-CBC-encrypted JSON:
 *     - Windows: named pipes  \\.\pipe\PipeZCallRecv / PipeZCallSend
 *     - macOS:   unix sockets /tmp/socketzalorecv2021 / socketzalosend2021
 *
 *   On Linux neither branch works (no macOS helper; Wine 9.9+ removed AF_UNIX
 *   support, so unix sockets are out). This patch:
 *     1. makes the main process listen on TCP ports 29631/29632 on Linux
 *     2. spawns pipebridge.js under Wine (pure-Node, Electron 2 runtime),
 *        which hosts the named pipes and pumps bytes to the TCP ports
 *     3. spawns the Windows ZaloCall.exe under Wine with the named-pipe args
 *
 *   The transport crypto, message handling and renderer IPC stay untouched.
 */

const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const MAIN_JS = path.join(__dirname, '..', '..', 'app', 'main-dist', 'main.js');

const REPLACEMENTS = [
  // 1. channel addresses: TCP ports on Linux (inline platform checks — no new
  //    variables: the module scope already uses every short name)
  {
    from: 'y="win32"===n("jle/").platform(),g=y?"\\\\\\\\.\\\\pipe\\\\PipeZCallSend":"/tmp/socketzalosend2021",v=y?"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv":"/tmp/socketzalorecv2021"',
    to: 'y="win32"===n("jle/").platform(),g=y?"\\\\\\\\.\\\\pipe\\\\PipeZCallSend":"linux"===n("jle/").platform()?29632:"/tmp/socketzalosend2021",v=y?"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv":"linux"===n("jle/").platform()?29631:"/tmp/socketzalorecv2021"',
  },
  // 2. binary path: add Linux branch before the macOS branch
  {
    from: ':(e=u()?o.join(__dirname,"..","native","qt-call-cap-mac","ZaloHelper.app")',
    to: ':("linux"===process.platform?e=o.join(__dirname,"..","native","qt-call-and-cap","ZaloCall.exe"):(e=u()?o.join(__dirname,"..","native","qt-call-cap-mac","ZaloHelper.app")',
  },
  {
    from: 'e=o.join(e,"Contents","MacOS","ZaloCall")),e}();',
    to: 'e=o.join(e,"Contents","MacOS","ZaloCall"))),e}();',
  },
  // 3. spawn: on Linux, start pipebridge then ZaloCall under Wine.
  //    NOTE: `from` is anchored with the leading `;` so it cannot re-match
  //    inside this replacement's own else branch (which keeps the original
  //    text) — without the anchor, every re-run of the patch script nests
  //    another dead linux branch into the ternary.
  {
    from: ';A=i(e,[v,g]),A.stdout.setEncoding("utf8")',
    to: ';"linux"===process.platform?(i(process.env.ZCALL_WINE||"wine",[o.join(__dirname,"..","native","qt-call-and-cap","pipebridge.exe"),"29631","29632"]),A=i(process.env.ZCALL_WINE||"wine",[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"])):A=i(e,[v,g]),A.stdout.setEncoding("utf8")',
  },
  // 4. listen: TCP on Linux, unix socket elsewhere
  {
    from: 'I.listen(v,(',
    to: 'I.listen("linux"===process.platform?{port:v,host:"127.0.0.1"}:v,(',
  },
  {
    from: 'C.listen(g,(',
    to: 'C.listen("linux"===process.platform?{port:g,host:"127.0.0.1"}:g,(',
  },
  // 5. EADDRINUSE recovery: skip fs.unlink on Linux (v/g are ports there)
  {
    from: 'y||a.unlink(v,',
    to: '"linux"===process.platform||y||a.unlink(v,',
  },
  {
    from: 'y||(U=!1,a.unlink(g,',
    to: '"linux"===process.platform||y||(U=!1,a.unlink(g,',
  },
  // 6. Re-send the init payload right before every makeCall. The helper
  //    rejects makeCall with error -11 ("init_error") if it has not seen
  //    the init data yet; sending O first (same TCP stream, order
  //    preserved) removes that race.
  {
    from: '.on("call-send-to-native",((e,t)=>{t._optional?delete t._optional:K(),D(t)}))',
    to: '.on("call-send-to-native",((e,t)=>{t._optional?delete t._optional:K(),t&&"makeCall"===t.command&&O&&D(O),D(t)}))',
  },
  // 7. Fix the send queue's F flag. On the non-win32 path the flag is only
  //    cleared when the helper sends data back on the send channel — which
  //    it almost never does — so after the first message every further
  //    message stalls in the queue forever (init goes through, makeCall
  //    never arrives). Reset the flag shortly after each write.
  {
    from: 'F=!0,e.write(t)',
    to: 'F=!0,e.write(t),setTimeout((()=>{F=!1,W(e)}),100)',
  },
  // 8. Auth token handshake: the main process generates a random token,
  //    passes it to pipebridge, and requires it as the first line of every
  //    TCP connection. Drops connections that do not present the token.
  {
    from: 'let S,D,O,N,A,C=null,I=null,L=!1,P=[],M=!1,k=!0,x=[],F=!1,U=!1',
    to: 'let S,D,O,N,A,C=null,I=null,L=!1,P=[],M=!1,k=!0,x=[],F=!1,U=!1,TK=null',
  },
  // 9. Reset the "native started" flag when the helper spawn FAILS (e.g.
  //    wine missing). Without this, a failed first spawn leaves L=true
  //    forever and later call attempts never re-spawn until app restart.
  {
    from: 'A.on("error",(e=>{d.zsymb(22,"4OM2ud",["client error","6Br8Rv"],e)}))',
    to: 'A.on("error",(e=>{L=!1,d.zsymb(22,"4OM2ud",["client error","6Br8Rv"],e)}))',
  },
  {
    from: 'i(process.env.ZCALL_WINE||"wine",[o.join(__dirname,"..","native","qt-call-and-cap","pipebridge.exe"),"29631","29632"]),A=i(process.env.ZCALL_WINE||"wine"',
    to: 'TK="zcall-"+Math.random().toString(36).slice(2)+Date.now().toString(36),i(process.env.ZCALL_WINE||"wine",[o.join(__dirname,"..","native","qt-call-and-cap","pipebridge.exe"),"29631","29632",TK]),A=i(process.env.ZCALL_WINE||"wine"',
  },
  // 10. Wayland screen-share bridge: preload the streamproxy shim (when the
  //     plugin set ZCALL_PROXY_SO) so ZaloCall's screen-capture reads are
  //     served from the bridge display while the app itself stays native.
  {
    from: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"]))',
    to: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""})}))',
  },
  {
    from: 'e.on("data",(e=>{z(e)})),e.on("end"',
    to: 'e.on("data",(n=>{if(!e.t){e.t=!0;const t=n.toString();if(t.indexOf(TK)!==0)return e.destroy();n=t.slice(TK.length+1)}z(n)})),e.on("end"',
  },
  {
    from: 'e.on("data",(t=>{d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))}))',
    to: 'e.on("data",(t=>{if(!e.t){e.t=!0;const i=t.toString();if(i.indexOf(TK)!==0)return e.destroy();t=i.slice(TK.length+1)}d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))}))',
  },
  // 11. Helper restart support. The Wayland screen bridge restarts ZaloCall
  //     mid-session (DISPLAY is read at spawn time), so the helper must be
  //     able to die and come back: reset L on exit, and spawn pipebridge
  //     (which owns the auth token) only ONCE per session — pipebridge
  //     re-creates the pipes itself and waits for the next ZaloCall.
  //     (BB, not B: the module already declares B.)
  {
    from: 'F=!1,U=!1,TK=null',
    to: 'F=!1,U=!1,TK=null,BB=!1',
  },
  {
    from: 'TK="zcall-"+Math.random().toString(36).slice(2)+Date.now().toString(36),i(process.env.ZCALL_WINE||"wine",[o.join(__dirname,"..","native","qt-call-and-cap","pipebridge.exe"),"29631","29632",TK]),A=i(process.env.ZCALL_WINE||"wine"',
    to: 'BB||(BB=!0,TK="zcall-"+Math.random().toString(36).slice(2)+Date.now().toString(36),i(process.env.ZCALL_WINE||"wine",[o.join(__dirname,"..","native","qt-call-and-cap","pipebridge.exe"),"29631","29632",TK])),A=i(process.env.ZCALL_WINE||"wine"',
  },
  {
    from: 'A.on("error",(e=>{L=!1,d.zsymb(22,"4OM2ud",["client error","6Br8Rv"],e)}))',
    to: 'A.on("error",(e=>{L=!1,d.zsymb(22,"4OM2ud",["client error","6Br8Rv"],e)})),A.on("exit",(()=>{L=!1}))',
  },
  // 12. Queue sends while the helper is restarting instead of writing to a
  //     destroyed socket (unhandled socket error would crash the main
  //     process). The queue is flushed when pipebridge reconnects (token
  //     line below) or after each helper message.
  {
    from: 'D=t=>{y?V(e,t):G(e,t)',
    // No trailing `}`: D is the last statement of the connection callback
    // and the original `}}))` closes D, the callback and C.on( — adding a
    // brace here breaks the bundle syntax.
    to: 'D=t=>{y?V(e,t):e&&!e.destroyed?G(e,t):x.push(t)',
  },
  {
    from: 't=i.slice(TK.length+1)}d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))',
    to: 't=i.slice(TK.length+1)}W(e),d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))',
  },
  {
    from: 'else if(e){if(x.length){const t=x.shift();$(e,t)',
    to: 'else if(e&&!e.destroyed){if(x.length){const t=x.shift();$(e,t)',
  },
];

async function main() {
  if (!fs.existsSync(MAIN_JS)) {
    logger.warn('main.js not found, skipping call-v2 patch');
    return;
  }

  // if we are on aa64, skip the patch (because the binary is x64 only)
  if (process.arch === 'arm64' || process.arch === 'aarch64') {
    logger.info('skipping call-v2 patch on arm64');
    return;
  }

  let content = fs.readFileSync(MAIN_JS, 'utf8');
  let applied = 0;

  for (const { from, to } of REPLACEMENTS) {
    if (content.includes(to)) {
      logger.dim('call-v2 patch already applied: ' + to.slice(0, 50) + '...');
      applied++;
      continue;
    }
    const count = content.split(from).length - 1;
    if (count === 0) {
      logger.warn('call-v2 pattern not found: ' + from.slice(0, 60) + '...');
      continue;
    }
    content = content.split(from).join(to);
    applied += count;
    logger.dim(`call-v2 patched (x${count}): ${from.slice(0, 60)}...`);
  }

  if (applied > 0) {
    fs.writeFileSync(MAIN_JS, content, 'utf8');
    logger.success('zcall call-v2 patch applied');
  }
}

if (require.main === module) {
  main();
}

module.exports = { main };
