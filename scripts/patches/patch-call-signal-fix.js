const fs = require('fs');
const path = require('path');

let logger;
try {
  logger = require('../utils/logger');
} catch (_) {
  logger = {
    info: (...args) => console.log('[INFO]', ...args),
    warn: (...args) => console.warn('[WARN]', ...args),
    error: (...args) => console.error('[ERROR]', ...args),
    success: (...args) => console.log('[SUCCESS]', ...args),
    dim: (...args) => console.log(' ', ...args)
  };
}

const APP_DIR = path.join(__dirname, '..', '..', 'app');

/**
 * Patch Zalo Call Signal Queue Deadlock:
 * Fixes outgoing/incoming calls getting stuck at "Đang kết nối..." (Connecting...)
 * caused by Signal 401 (requestCall) and call signaling being held inside requestQueue
 * without ever being dequeued on Linux.
 */
async function main() {
  const pcDistDir = path.join(APP_DIR, 'pc-dist');
  const lazyDir = path.join(pcDistDir, 'lazy');

  if (!fs.existsSync(pcDistDir)) {
    logger.warn('pc-dist directory not found, skipping call-signal fix');
    return;
  }

  function escapeRegex(string) {
    return string.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  }

  function patchFile(filePath, replacements) {
    if (!fs.existsSync(filePath)) {
      logger.warn(`File not found: ${filePath}`);
      return false;
    }

    let content = fs.readFileSync(filePath, 'utf8');
    let changed = false;

    for (const [from, to] of replacements) {
      if (content.includes(from)) {
        content = content.replace(typeof from === 'string' ? new RegExp(escapeRegex(from), 'g') : from, to);
        changed = true;
      }
    }

    if (changed) {
      fs.writeFileSync(filePath, content, 'utf8');
      logger.dim(`Patched call signal queue in ${path.basename(filePath)}`);
      return true;
    }
    return false;
  }

  function findFiles(dir, pattern) {
    if (!fs.existsSync(dir)) return [];
    return fs.readdirSync(dir)
      .filter(f => pattern.test(f))
      .map(f => path.join(dir, f));
  }

  // 1. compact-app-pc bundle
  const compactFiles = findFiles(pcDistDir, /^compact-app-pc\..*\.js$/);
  for (const f of compactFiles) {
    patchFile(f, [
      ['using_queue:1', 'using_queue:0'],
      ['if(a)if(A.default.call.using_queue)', 'if(a)if(!1&&A.default.call.using_queue)'],
      [
        'function P(e={}){const{limit:t=1/0,maxTimeout:n=L}=e,a=[];let i;const r=e=>{i=e}',
        'function P(e={}){const{limit:t=1/0,maxTimeout:n=L}=e,a=[];let i=w;const r=e=>{i=e}'
      ],
      [
        'this.requestQueue=P(),this.retryQueue=[]',
        'this.requestQueue=P(),this.requestQueue.dequeue(),this.retryQueue=[]'
      ]
    ]);
  }

  // 2. default-login bundle in lazy/
  const defaultLoginFiles = findFiles(lazyDir, /^default-login-main-startup-shared-worker-znotification\..*\.js$/);
  for (const f of defaultLoginFiles) {
    patchFile(f, [
      ['using_queue:1', 'using_queue:0'],
      ['if(a)if(A.default.call.using_queue)', 'if(a)if(!1&&A.default.call.using_queue)'],
      [
        'function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let s;const i=e=>{s=e}',
        'function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let s=D;const i=e=>{s=e}'
      ],
      [
        'this.requestQueue=L(),this.retryQueue=[]',
        'this.requestQueue=L(),this.requestQueue.dequeue(),this.retryQueue=[]'
      ]
    ]);
  }

  // 3. search-worker bundle
  const searchWorkerFiles = findFiles(pcDistDir, /^search-worker\..*\.js$/);
  for (const f of searchWorkerFiles) {
    patchFile(f, [
      ['using_queue:1', 'using_queue:0'],
      ['if(a)if(A.default.call.using_queue)', 'if(a)if(!1&&A.default.call.using_queue)'],
      [
        'function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i;const s=e=>{i=e}',
        'function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i=w;const s=e=>{i=e}'
      ],
      [
        'this.requestQueue=L(),this.retryQueue=[]',
        'this.requestQueue=L(),this.requestQueue.dequeue(),this.retryQueue=[]'
      ]
    ]);
  }

  // 4. sync-v2-sub-worker bundle
  const syncWorkerFiles = findFiles(pcDistDir, /^sync-v2-sub-worker\..*\.js$/);
  for (const f of syncWorkerFiles) {
    patchFile(f, [
      ['using_queue:1', 'using_queue:0'],
      ['if(a)if(A.default.call.using_queue)', 'if(a)if(!1&&A.default.call.using_queue)'],
      [
        'function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i;const s=e=>{i=e}',
        'function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i=w;const s=e=>{i=e}'
      ],
      [
        'this.requestQueue=L(),this.retryQueue=[]',
        'this.requestQueue=L(),this.requestQueue.dequeue(),this.retryQueue=[]'
      ]
    ]);
  }

  logger.success('Call signal queue deadlock patches applied');
}

if (require.main === module) {
  main();
}

module.exports = { main };
