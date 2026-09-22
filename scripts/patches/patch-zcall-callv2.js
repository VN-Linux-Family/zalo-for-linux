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
  //     Also compose the bundled i386-GStreamer env from ZCALL_GST_RUNTIME_I386
  //     — ONLY for this spawn, never globally (leaking LD_LIBRARY_PATH/GST_*
  //     into screenbridge/validate/wineboot would break them). The ternary
  //     yields null when no bundle: Object.assign skips null. Only classic
  //     wine is supported for calls; the 64-bit gst-runtime serves the
  //     screen-share bridge alone (spawned by the plugin, not here).
  {
    from: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"]))',
    to: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""},process.env.ZCALL_GST_RUNTIME_I386?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME_I386+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/gstreamer-1.0",GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/system",GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null)}))',
  },
  {
    from: 'e.on("data",(e=>{z(e)})),e.on("end"',
    // Buffered token check: TCP can deliver the token in FRAGMENTS — the
    // old one-shot indexOf check destroyed the socket on a partial first
    // chunk, killing ZaloCall mid-init (page fault in its teardown on some
    // hosts — the Mint video-call crash). Accumulate until the full token
    // length is buffered before verifying.
    to: 'e.on("data",(n=>{if(!e.t){e.buf=(e.buf||"")+n.toString();if(e.buf.length<=TK.length)return;if(e.buf.indexOf(TK)!==0)return e.destroy();n=e.buf.slice(TK.length+1);e.t=!0;e.buf=null}z(n)})),e.on("end"',
  },
  {
    from: 'e.on("data",(t=>{d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))}))',
    to: 'e.on("data",(t=>{if(!e.t){e.buf=(e.buf||"")+t.toString();if(e.buf.length<=TK.length)return;if(e.buf.indexOf(TK)!==0)return e.destroy();t=e.buf.slice(TK.length+1);e.t=!0;e.buf=null}d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))}))',
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
  // 12. Retired v4l2loopback camera trigger — the camera fix now ships as
  //     a bundled i386 GStreamer stack, so these entries strip the old
  //     trigger code back out and fold the i386 GST env into the spawn.
  {
    optional: true,
    // migrate builds that carry the old spawn-time trigger version down
    // to the exit-unlink form first (NOTE: `to` puts the exit handler
    // FIRST — the natural error+exit order would be a prefix substring
    // of `from` and the includes(to) guard would skip the migration);
    // the removal entry below then strips the exit handler entirely.
    from: 'A.on("error",(e=>{L=!1,d.zsymb(22,"4OM2ud",["client error","6Br8Rv"],e)})),A.on("exit",(()=>{L=!1,process.env.ZCALL_CAM_TRIGGER&&(()=>{try{require("fs").unlinkSync(process.env.ZCALL_CAM_TRIGGER)}catch(e){}})()})),process.env.ZCALL_CAM_TRIGGER&&(()=>{try{require("fs").writeFileSync(process.env.ZCALL_CAM_TRIGGER,"")}catch(e){}})()',
    to: 'A.on("exit",(()=>{L=!1,process.env.ZCALL_CAM_TRIGGER&&(()=>{try{require("fs").unlinkSync(process.env.ZCALL_CAM_TRIGGER)}catch(e){}})()})),A.on("error",(e=>{L=!1,d.zsymb(22,"4OM2ud",["client error","6Br8Rv"],e)}))',
  },
  {
    // strip the trigger-cleanup exit handler back out
    remove: 'A.on("exit",(()=>{L=!1,process.env.ZCALL_CAM_TRIGGER&&(()=>{try{require("fs").unlinkSync(process.env.ZCALL_CAM_TRIGGER)}catch(e){}})()})),',
  },
  {
    // strip the CT/CF/CV helpers from the module scope
    remove: ',CV=0,CT=t=>{if(!t||!process.env.ZCALL_CAM_TRIGGER)return;try{const q=process.env.ZCALL_CAM_TRIGGER;if("request"===t.type&&"makeCall"===t.command&&t.data&&(3===t.data.type||6===t.data.type))return require("fs").writeFileSync(q,"");if("control"===t.type&&t.data&&"voip"===t.data.act_type){if("request"===t.data.act||"group_request"===t.data.act)return void(CV=t.data.type||t.data.callType||0);if("answer"===t.data.act||"group_answer"===t.data.act)return void((0===CV||3===CV||6===CV)&&require("fs").writeFileSync(q,""));if("cancel"===t.data.act||"reject"===t.data.act||"busy"===t.data.act||"timeout"===t.data.act)CV=0}if("request"===t.type&&"endCall"===t.command)require("fs").unlinkSync(q)}catch(e){}},CF=()=>{if(!process.env.ZCALL_CAM_TRIGGER)return;try{require("fs").unlinkSync(process.env.ZCALL_CAM_TRIGGER)}catch(e){}}',
  },
  {
    // strip the trigger hook from the renderer->helper handler
    remove: ',CT(t)',
  },
  {
    optional: true,
    // migrate the callState-free trigger removal back to the original form
    from: 'else (S.webContents.send("call-update",e.command,e.data),"callState"===e.command&&e.data&&"free"===e.data.state&&CF());break;',
    to: 'else S.webContents.send("call-update",e.command,e.data);break;',
  },
  {
    optional: true,
    // migrate the no-cross-append env: when BOTH gst branches were set
    // the 64-bit branch overwrote the i386 LD_LIBRARY_PATH and classic
    // wine fell back to the host 32-bit gst (camera bug on Full builds)
    from: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""},process.env.ZCALL_GST_RUNTIME_I386?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME_I386+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/gstreamer-1.0",GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/system",GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null)}))',
    to: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""},process.env.ZCALL_GST_RUNTIME_I386?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME_I386+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/gstreamer-1.0",GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/system",GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null)}))',
  },
  {
    optional: true,
    // migrate the cross-append env down to the i386-only form: the
    // 64-bit call branch is retired — only classic wine is supported
    // and the 64-bit gst-runtime now serves the screen bridge alone
    from: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""},process.env.ZCALL_GST_RUNTIME_I386?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME_I386+(process.env.ZCALL_GST_RUNTIME?":"+process.env.ZCALL_GST_RUNTIME+"/usr/lib/x86_64-linux-gnu":"")+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/gstreamer-1.0"+(process.env.ZCALL_GST_RUNTIME?":"+process.env.ZCALL_GST_RUNTIME+"/usr/lib/x86_64-linux-gnu/gstreamer-1.0":""),GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/system"+(process.env.ZCALL_GST_RUNTIME?":"+process.env.ZCALL_GST_RUNTIME+"/system":""),GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null,process.env.ZCALL_GST_RUNTIME?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME+"/usr/lib/x86_64-linux-gnu"+(process.env.ZCALL_GST_RUNTIME_I386?":"+process.env.ZCALL_GST_RUNTIME_I386:"")+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME+"/usr/lib/x86_64-linux-gnu/gstreamer-1.0"+(process.env.ZCALL_GST_RUNTIME_I386?":"+process.env.ZCALL_GST_RUNTIME_I386+"/gstreamer-1.0":""),GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME+"/system"+(process.env.ZCALL_GST_RUNTIME_I386?":"+process.env.ZCALL_GST_RUNTIME_I386+"/system":""),GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null)}))',
    to: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""},process.env.ZCALL_GST_RUNTIME_I386?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME_I386+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/gstreamer-1.0",GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/system",GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null)}))',
  },
  {
    // migrate the spawn env to include the bundled i386 GStreamer branch
    from: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""},process.env.ZCALL_GST_RUNTIME?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME+"/usr/lib/x86_64-linux-gnu"+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME+"/usr/lib/x86_64-linux-gnu/gstreamer-1.0",GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME+"/system",GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null)}))',
    to: '[e,"\\\\\\\\.\\\\pipe\\\\PipeZCallRecv","\\\\\\\\.\\\\pipe\\\\PipeZCallSend"],{env:Object.assign({},process.env,{LD_PRELOAD:process.env.ZCALL_PROXY_SO||process.env.LD_PRELOAD||""},process.env.ZCALL_GST_RUNTIME_I386?{LD_LIBRARY_PATH:process.env.ZCALL_GST_RUNTIME_I386+(process.env.LD_LIBRARY_PATH?":"+process.env.LD_LIBRARY_PATH:""),GST_PLUGIN_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/gstreamer-1.0",GST_PLUGIN_SYSTEM_PATH:process.env.ZCALL_GST_RUNTIME_I386+"/system",GST_REGISTRY:process.env.ZCALL_GST_REGISTRY||""}:null)}))',
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
    // Chained after the buffered send-site above (its `from` matches that
    // site's NEW `to` text — keep them in lockstep).
    from: 't=e.buf.slice(TK.length+1);e.t=!0;e.buf=null}d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))',
    to: 't=e.buf.slice(TK.length+1);e.t=!0;e.buf=null}W(e),d.zsymb(4,"VafRm1",["serverSend on data","ySFwkp"],t),y||(F=!1,W(e))',
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

  let content = fs.readFileSync(MAIN_JS, 'utf8');
  let applied = 0;

  for (const entry of REPLACEMENTS) {
    // Removal entries strip a previously-applied snippet (e.g. code from a
    // retired feature). Idempotent — skip when the text is already gone.
    // The regular from/to guard cannot express removals: the new text is
    // '' which `includes` always matches.
    if (entry.remove) {
      const count = content.split(entry.remove).length - 1;
      if (count === 0) {
        logger.dim('call-v2 patch removal not needed: ' + entry.remove.slice(0, 50) + '...');
        continue;
      }
      content = content.split(entry.remove).join('');
      applied += count;
      logger.dim(`call-v2 patch removed (x${count}): ${entry.remove.slice(0, 60)}...`);
      continue;
    }
    const { from, to } = entry;
    if (content.includes(to)) {
      logger.dim('call-v2 patch already applied: ' + to.slice(0, 50) + '...');
      applied++;
      continue;
    }
    const count = content.split(from).length - 1;
    if (count === 0) {
      // optional entries migrate OLD patched builds — on a fresh main.js
      // their absence is the EXPECTED case, not a defect.
      if (entry.optional) {
        logger.dim('call-v2 migration not needed: ' + from.slice(0, 60) + '...');
      } else {
        logger.warn('call-v2 pattern not found: ' + from.slice(0, 60) + '...');
      }
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
