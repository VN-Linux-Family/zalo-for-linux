const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');

const { APP_PC_DIST, resolveLazyLoginBundles } = require('../utils/paths');

/**
 * Patch Zalo Call Signal Queue Deadlock:
 * Fixes outgoing/incoming calls getting stuck at "Đang kết nối..." (Connecting...)
 * caused by Signal 401 (requestCall) and call signaling being held inside requestQueue
 * without ever being dequeued on Linux.
 *
 * The queue's state starts undefined and only runs while it is "idle", so
 * nothing queued is ever sent. Start it idle and kick it once, and keep call
 * signals off the queue (using_queue) as a second guard.
 *
 * The minifier renames the locals in every release (the idle constant was `w`
 * in one bundle and `D` in the other), so everything is matched by shape.
 */

// const w="idle",M="running",D="pause",L=3e4;function P(e={}){const{limit:t=1/0,maxTimeout:n=L}=e,a=[];let i;
const QUEUE_INIT = /([\w$]+)="idle",([\w$]+)="running",([\w$]+)="pause",([\w$]+)=3e4;function ([\w$]+)\(e=\{\}\)\{const\{limit:t=1\/0,maxTimeout:n=\4\}=e,a=\[\];let ([\w$]+);/;
const QUEUE_INIT_DONE = /([\w$]+)="idle",[\w$]+="running",[\w$]+="pause",[\w$]+=3e4;function [\w$]+\(e=\{\}\)\{const\{limit:t=1\/0,maxTimeout:n=[\w$]+\}=e,a=\[\];let [\w$]+=\1;/;
const QUEUE_KICK = /this\.requestQueue=([\w$]+)\(\),this\.retryQueue=\[\]/;
const QUEUE_KICK_DONE = /this\.requestQueue=[\w$]+\(\),this\.requestQueue\.dequeue\(\),this\.retryQueue=\[\]/;
const USING_QUEUE = /if\(([\w$]+)\)if\(([\w$]+)\.default\.call\.using_queue\)/g;
const USING_QUEUE_DONE = /if\([\w$]+\)if\(!1&&[\w$]+\.default\.call\.using_queue\)/;

// Returns { content, missing } for one bundle.
function patchContent(content) {
  let out = content
    .split('using_queue:1').join('using_queue:0')
    .replace(USING_QUEUE, 'if($1)if(!1&&$2.default.call.using_queue)');
  if (!QUEUE_INIT_DONE.test(out)) {
    out = out.replace(QUEUE_INIT, '$1="idle",$2="running",$3="pause",$4=3e4;function $5(e={}){const{limit:t=1/0,maxTimeout:n=$4}=e,a=[];let $6=$1;');
  }
  if (!QUEUE_KICK_DONE.test(out)) {
    out = out.replace(QUEUE_KICK, 'this.requestQueue=$1(),this.requestQueue.dequeue(),this.retryQueue=[]');
  }

  const missing = [];
  if (!QUEUE_INIT_DONE.test(out)) missing.push('queue initial state');
  if (!QUEUE_KICK_DONE.test(out)) missing.push('queue dequeue');
  if (!USING_QUEUE_DONE.test(out)) missing.push('using_queue guard');
  return { content: out, missing };
}

async function main(pcDistDir = APP_PC_DIST) {
  if (!fs.existsSync(pcDistDir)) {
    throw new Error('call-signal fix: pc-dist directory not found: ' + pcDistDir);
  }

  // The call controller lives in the main renderer and the login/startup
  // worker. Zalo 26.11.20 dropped it from search-worker and sync-v2-sub-worker.
  const required = [
    ...fs.readdirSync(pcDistDir).filter(f => /^compact-app-pc\..*\.js$/.test(f)).map(f => path.join(pcDistDir, f)),
    ...resolveLazyLoginBundles(pcDistDir),
  ];
  const optional = fs.readdirSync(pcDistDir)
    .filter(f => /^(search-worker|sync-v2-sub-worker)\..*\.js$/.test(f))
    .map(f => path.join(pcDistDir, f));

  if (required.length < 2) {
    throw new Error('call-signal fix: compact-app-pc or default-login bundle not found');
  }

  const failures = [];
  for (const file of [...required, ...optional]) {
    const original = fs.readFileSync(file, 'utf8');
    if (optional.includes(file) && !QUEUE_KICK.test(original) && !QUEUE_KICK_DONE.test(original)) continue;
    const { content, missing } = patchContent(original);
    if (missing.length) {
      failures.push(`${path.basename(file)}: ${missing.join(', ')}`);
      continue;
    }
    if (content !== original) {
      fs.writeFileSync(file, content, 'utf8');
      logger.dim(`Patched call signal queue in ${path.basename(file)}`);
    }
  }

  if (failures.length) {
    throw new Error('call-signal fix not applied, Zalo changed: ' + failures.join('; '));
  }
  logger.success('Call signal queue deadlock patches applied');
}

if (require.main === module) {
  main().catch((error) => {
    logger.error('Call signal patch failed:', error.message);
    process.exit(1);
  });
}

module.exports = { main, patchContent };
