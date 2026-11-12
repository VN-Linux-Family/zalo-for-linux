'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { APP_DIR } = require('../utils/paths');
const { parseAst } = require('../utils/ast-transform');

const MARKER = '/* zalo-linux-offline */';
const PRELOAD_MARKER = 'zalo-linux-offline-preload';
const SHARED_PRELOAD_MARKER = 'zalo-linux-offline-shared-preload';
const HTML_MARKER = 'zalo-linux-offline-renderer';

const PRELOAD_BOOTSTRAP = `
;/* ${PRELOAD_MARKER} */
(() => {
  const { contextBridge, ipcRenderer } = require('electron');
  const api = Object.freeze({
    loadRenderer: () => ipcRenderer.sendSync('linux-offline:renderer') || '',
    request: (action, payload) => ipcRenderer.invoke('linux-offline:request', action, payload),
    reportActive: active => ipcRenderer.send('linux-offline:active', active === true)
  });

  if (process.contextIsolated) {
    contextBridge.exposeInMainWorld('__zaloOfflineBridge', api);
  } else if (typeof window !== 'undefined') {
    Object.defineProperty(window, '__zaloOfflineBridge', {
      configurable: false,
      enumerable: false,
      writable: false,
      value: api
    });
  }
})();
`;

const SHARED_PRELOAD_BOOTSTRAP = `
;/* ${SHARED_PRELOAD_MARKER} */
(() => {
  const { contextBridge, ipcRenderer } = require('electron');
  const api = Object.freeze({
    isActive: () => ipcRenderer.sendSync('linux-offline:is-active') === true
  });

  if (process.contextIsolated) {
    contextBridge.exposeInMainWorld('__zaloOfflineBridge', api);
  } else if (typeof window !== 'undefined') {
    Object.defineProperty(window, '__zaloOfflineBridge', {
      configurable: false,
      enumerable: false,
      writable: false,
      value: api
    });
  }
})();
`;

const HTML_BOOTSTRAP = `
<script>/* ${HTML_MARKER} */
(() => {
  try {
    const bridge = window.__zaloOfflineBridge;
    if (!bridge || typeof bridge.loadRenderer !== 'function') return;
    const source = bridge.loadRenderer();
    if (typeof source !== 'string' || !source) return;
    const script = document.createElement('script');
    script.textContent = source;
    (document.head || document.documentElement).appendChild(script);
    script.remove();
  } catch (error) {
    console.error('[offline] Renderer bootstrap failed:', error);
  }
})();
</script>
`;

function patchPreloadContent(source) {
  const count = source.split(PRELOAD_MARKER).length - 1;
  if (count > 1) throw new Error('Duplicate offline preload markers');
  if (count === 1) {
    const required = [
      "'__zaloOfflineBridge'",
      "'linux-offline:renderer'",
      "'linux-offline:request'",
      "'linux-offline:active'"
    ];
    const missing = required.filter(value => !source.includes(value));
    if (missing.length) {
      throw new Error(`Offline preload marker is incomplete (${missing.join(', ')})`);
    }
    return { content: source, status: 'already' };
  }

  return { content: source + '\n' + PRELOAD_BOOTSTRAP + '\n', status: 'patched' };
}

function patchSharedPreloadContent(source) {
  const count = source.split(SHARED_PRELOAD_MARKER).length - 1;
  if (count > 1) throw new Error('Duplicate offline shared-worker preload markers');
  if (count === 1) {
    const required = [
      "'__zaloOfflineBridge'",
      "'linux-offline:is-active'",
      'isActive: () =>'
    ];
    const missing = required.filter(value => !source.includes(value));
    if (missing.length) {
      throw new Error(`Offline shared-worker preload marker is incomplete (${missing.join(', ')})`);
    }
    return { content: source, status: 'already' };
  }

  return { content: source + '\n' + SHARED_PRELOAD_BOOTSTRAP + '\n', status: 'patched' };
}

function patchHtmlContent(source, filename) {
  const count = source.split(HTML_MARKER).length - 1;
  if (count > 1) throw new Error('Duplicate offline renderer bootstrap markers');
  if (count === 1) {
    if (!source.includes('bridge.loadRenderer()')) {
      throw new Error('Offline renderer bootstrap marker is incomplete in ' + filename);
    }
    return { content: source, status: 'already' };
  }

  const csp = /<meta\b(?=[^>]*http-equiv=["']Content-Security-Policy["'])[^>]*>/i;
  if (csp.test(source)) {
    return {
      content: source.replace(csp, match => match + HTML_BOOTSTRAP),
      status: 'patched'
    };
  }

  const head = /<head(?:\s[^>]*)?>/i;
  if (!head.test(source)) throw new Error('Missing <head> in ' + filename);
  return {
    content: source.replace(head, match => match + HTML_BOOTSTRAP),
    status: 'patched'
  };
}

function writeAtomic(filePath, content) {
  const temporary = filePath + '.offline-patch-tmp';
  try {
    fs.writeFileSync(temporary, content);
    fs.renameSync(temporary, filePath);
  } catch (error) {
    try { fs.rmSync(temporary, { force: true }); } catch (_) {}
    throw error;
  }
}

function replaceRequired(source, from, to, label) {
  if (from instanceof RegExp) {
    if (!from.test(source)) throw new Error(`Offline startup: missing ${label}; upstream bundle changed`);
    return source.replace(from, to);
  }
  if (!source.includes(from)) throw new Error(`Offline startup: missing ${label}; upstream bundle changed`);
  return source.split(from).join(to);
}

function matchingDelimiter(source, openIndex, open, close) {
  let depth = 0;
  let quote = null;
  let lineComment = false;
  let blockComment = false;
  let regex = false;
  let regexClass = false;
  let canStartRegex = true;

  for (let i = openIndex; i < source.length; i++) {
    const char = source[i];
    const next = source[i + 1];
    if (lineComment) { if (char === '\n') lineComment = false; continue; }
    if (blockComment) { if (char === '*' && next === '/') { blockComment = false; i++; } continue; }
    if (quote) {
      if (char === '\\') { i++; continue; }
      if (char === quote) quote = null;
      continue;
    }
    if (regex) {
      if (char === '\\') { i++; continue; }
      if (char === '[') regexClass = true;
      else if (char === ']') regexClass = false;
      else if (char === '/' && !regexClass) { regex = false; while (/[a-z]/i.test(source[i + 1] || '')) i++; }
      continue;
    }
    if (char === '/' && next === '/') { lineComment = true; i++; continue; }
    if (char === '/' && next === '*') { blockComment = true; i++; continue; }
    if (char === '/' && canStartRegex) { regex = true; continue; }
    if (char === '"' || char === "'" || char === '`') { quote = char; canStartRegex = false; continue; }
    if (char === open) depth++;
    else if (char === close && --depth === 0) return i;
    if (!/\s/.test(char)) canStartRegex = /[({[=,:;!?&|+*%~<>-]/.test(char);
  }
  return -1;
}

function parseMethodSnippet(source, name, predicate, label) {
  const pattern = new RegExp(`(?:async\\s+)?${name}\\s*\\(`, 'g');
  let candidate = null;
  let count = 0;
  let match;
  const wrapperPrefix = 'class __OfflinePatchTarget { '.length;

  while ((match = pattern.exec(source))) {
    const nameOffset = match[0].lastIndexOf(name);
    const nameStart = match.index + nameOffset;
    const openParen = source.indexOf('(', nameStart + name.length);
    const closeParen = matchingDelimiter(source, openParen, '(', ')');
    if (closeParen < 0) continue;
    let bodyStart = closeParen + 1;
    while (/\s/.test(source[bodyStart] || '')) bodyStart++;
    if (source[bodyStart] !== '{') continue;
    const bodyEnd = matchingDelimiter(source, bodyStart, '{', '}');
    if (bodyEnd < 0) continue;

    const start = match.index;
    const snippet = source.slice(start, bodyEnd + 1);
    const bodySource = source.slice(bodyStart + 1, bodyEnd);
    if (predicate(null, bodySource)) {
      let root;
      try { root = parseAst(`class __OfflinePatchTarget { ${snippet} }`); }
      catch (_) { pattern.lastIndex = bodyEnd + 1; continue; }
      const node = root.find({ rule: { kind: 'method_definition' } });
      if (node && node.field('name')?.text() === name) {
        const body = node.field('body');
        const parameters = node.field('parameters');
        const snapshot = {
          start,
          end: bodyEnd + 1,
          bodyStart: start + body.range().start.index - wrapperPrefix + 1,
          bodyText: body.text(),
          parametersText: parameters.text(),
          parameterArguments: name === 'fetchLoginInfo' ? parameterArguments(parameters) : null,
          methodNameStart: start + node.field('name').range().start.index - wrapperPrefix,
          requestCallStarts: name === 'init'
            ? body.findAll({ rule: { kind: 'call_expression' } })
              .filter(call => call.text() === 'this._doRequest()')
              .map(call => start + call.range().start.index - wrapperPrefix)
            : [],
          historyCloudGateCalls: name === 'getLastMessagesForConversation'
            ? body.findAll({ rule: { kind: 'call_expression' } })
              .filter(call => /\.apiEnable\([^)]*\.OVER_QUEUE\s*,\s*this\.conversationId\)/.test(call.text()))
              .map(call => ({
                start: start + call.range().start.index - wrapperPrefix,
                end: start + call.range().end.index - wrapperPrefix,
                text: call.text()
              }))
            : []
        };
        count++;
        if (count === 1) candidate = snapshot;
        else candidate = null;
      }
    }
    pattern.lastIndex = bodyEnd + 1;
  }

  if (count !== 1) {
    throw new Error(`Offline startup: expected one AST match for ${label}, found ${count}`);
  }
  return candidate;
}
function parameterArguments(parameters) {
  const args = parameters.namedChildren().map(parameter => {
    if (parameter.kind() === 'identifier') return parameter.text();
    if (parameter.kind() === 'assignment_pattern') return parameter.field('left')?.text();
    return null;
  });
  if (!args.length || args.some(argument => !argument || !/^[\w$]+$/.test(argument))) {
    throw new Error('Offline startup: unsupported fetchLoginInfo parameter structure');
  }
  return args;
}

function patchAuthMethods(source) {
  const edits = [];
  const offline = 'globalThis.__zaloOffline&&globalThis.__zaloOffline.active';
  const method = (name, predicate, label) => parseMethodSnippet(source, name, predicate, label);
  const addBodyGuard = (entry, code) => {
    edits.push({ start: entry.bodyStart, text: code });
  };

  const apiEnable = method('apiEnable', (_node, body) =>
    body.includes('.cloud.enable') && body.includes('switch('), 'apiEnable');
  if (!apiEnable.bodyText.includes(offline)) {
    addBodyGuard(apiEnable, `if(${offline})return!1;`);
  }

  const init = method('init', (_node, body) =>
    body.includes('_getUserData') && body.includes('clearUpdateInfoTimer') &&
    body.includes('this._doRequest()'), 'authentication init');
  if (!init.bodyText.includes(offline)) {
    if (init.requestCallStarts.length !== 1) throw new Error(`Offline startup: expected one init request call, found ${init.requestCallStarts.length}`);
    edits.push({
      start: init.requestCallStarts[0],
      text: `${offline}?Promise.resolve():`
    });
  }

  const fetch = method('fetchLoginInfo', (_node, body) =>
    body.includes('new Promise') || body.includes('_linuxFetchLoginInfo'), 'login-info method');
  if (!fetch.bodyText.includes('globalThis.__zaloOffline.fetchLoginInfo')) {
    const args = fetch.parameterArguments;
    if (args.length !== 3) throw new Error(`Offline startup: expected three fetchLoginInfo parameters, found ${args.length}`);
    const moduleFunctions = [...source.slice(0, fetch.start).matchAll(/[,{]["'][^"']+["']\s*:\s*function\(([^)]*)\)\{/g)];
    const moduleParameters = moduleFunctions[moduleFunctions.length - 1]?.[1].split(',').map(value => value.trim());
    const loaderName = moduleParameters?.[2];
    if (!loaderName || !/^[a-zA-Z0-9_$]+$/.test(loaderName)) {
      throw new Error('Offline startup: cannot identify authentication module loader');
    }
    const nameStart = fetch.methodNameStart;
    const nameEnd = nameStart + 'fetchLoginInfo'.length;
    const original = source.slice(fetch.start, fetch.end);
    const localNameStart = nameStart - fetch.start;
    const localNameEnd = nameEnd - fetch.start;
    const originalMethod = original.slice(0, localNameStart) + '_linuxFetchLoginInfo' + original.slice(localNameEnd);
    const parameterText = fetch.parametersText;
    const callArgs = args.join(',');
    const wrapper = `fetchLoginInfo${parameterText}{return globalThis.__zaloOffline?` +
      `globalThis.__zaloOffline.fetchLoginInfo(()=>this._linuxFetchLoginInfo(${callArgs}),` +
      `{clientId:${args[0]},userId:this.getMostRecentLoggedInUserId(),` +
      `onOffline:()=>${loaderName}("gwig").b.setStatusDisconnectNetwork()}):` +
      `this._linuxFetchLoginInfo(${callArgs})}`;
    edits.push({ start: fetch.start, end: fetch.end, text: wrapper + originalMethod });
  }

  const historyLoader = method('getLastMessagesForConversation', (_node, body) =>
    body.includes('isConversationOverQueueVerified') &&
    body.includes('loadLastMessageLocalFrom'), 'initial conversation history loader');
  const historyOffline = `(${offline})||(globalThis.__zaloOfflineBridge&&globalThis.__zaloOfflineBridge.isActive&&globalThis.__zaloOfflineBridge.isActive())`;
  const historyCloudGuard = `!(${historyOffline})&&`;
  if (!historyLoader.bodyText.includes(historyCloudGuard)) {
    if (historyLoader.historyCloudGateCalls.length !== 1) {
      throw new Error(`Offline startup: expected one Over-Queue gate in initial conversation history loader, found ${historyLoader.historyCloudGateCalls.length}`);
    }
    const call = historyLoader.historyCloudGateCalls[0];
    edits.push({
      start: call.start,
      end: call.end,
      text: `${historyCloudGuard}${call.text}`
    });
  }

  const messageBubble = method('render', (_node, body) =>
    body.includes('this.props.focusId.indexOf(b.msgId)') &&
    body.includes('this.props.focusType'), 'message bubble renderer');
  const focusIdGuard = 'if(globalThis.__zaloOffline&&globalThis.__zaloOffline.active&&!Array.isArray(this.props.focusId))this.props.focusId=[];';
  if (!messageBubble.bodyText.includes(focusIdGuard)) {
    if ((messageBubble.bodyText.match(/this\.props\.focusId\.indexOf\(b\.msgId\)/g) || []).length !== 2) {
      throw new Error('Offline startup: expected two focusId lookups in message bubble renderer');
    }
    edits.push({ start: messageBubble.bodyStart, text: focusIdGuard });
  }

  const hooks = [
    ['logout', body => body.includes('_doLogout') && body.includes('this.userId'), true, 'logout'],
    ['logoutFacebook', body => body.includes('_doLogout') && body.includes('this.userId'), true, 'Facebook logout'],
    ['_doLogout', body => body.includes('deactiveApp') && body.includes('onUserLogOff'), true, 'logout cleanup'],
    ['onKickoutSession', body => body.includes('KICK_OUT_SESSION'), false, 'kickout session'],
    ['clearLastSignin', body => body.includes('RECENT_PHONE') && body.includes('setMostRecentLoggedInUserInfo'), false, 'clear last signin'],
    ['_onLoginSuccess', body => body.includes('KEY_REMEMBER') && body.includes('getValueRememberLogin'), false, 'login success']
  ];
  for (const [name, predicate, awaitInvalidate, label] of hooks) {
    const entry = method(name, (_node, body) => predicate(body), label);
    if (!entry.bodyText.includes('globalThis.__zaloOffline.invalidate()')) {
      addBodyGuard(entry,
        `if(globalThis.__zaloOffline)${awaitInvalidate ? 'await ' : ''}globalThis.__zaloOffline.invalidate();`);
    }
  }

  const readyRegex = /this\.props\.dispatch\(\{type:([a-zA-Z0-9_$]+)\.FetchActions\.USERID_FETCHED,payload:([a-zA-Z0-9_$]+)\}\),this\.phoneNumber/;
  const readyMatches = [...source.matchAll(new RegExp(readyRegex.source, 'g'))];
  if (!readyMatches.length) throw new Error('Offline startup: missing profile-ready boundary');
  const ready = readyMatches[0];
  edits.push({
    start: ready.index,
    end: ready.index + ready[0].length,
    text: `this.props.dispatch({type:${ready[1]}.FetchActions.USERID_FETCHED,payload:${ready[2]}}),globalThis.__zaloOffline&&globalThis.__zaloOffline.markReady(),this.phoneNumber`
  });

  edits.sort((a, b) => b.start - a.start);
  let result = source;
  let previousStart = Infinity;
  for (const edit of edits) {
    if (edit.start > previousStart) throw new Error('Offline startup: overlapping AST edits');
    result = result.slice(0, edit.start) + edit.text + result.slice(edit.end ?? edit.start);
    previousStart = edit.start;
  }
  return result;
}
function patchAuthBundle(source) {
  if (source.includes(MARKER)) {
    const required = [
      'globalThis.__zaloOffline.fetchLoginInfo',
      'this._linuxFetchLoginInfo',
      'globalThis.__zaloOffline.markReady()',
      'globalThis.__zaloOffline.invalidate()',
      'globalThis.__zaloOffline&&globalThis.__zaloOffline.active',
      'globalThis.__zaloOfflineBridge.isActive&&globalThis.__zaloOfflineBridge.isActive()',
      'if(globalThis.__zaloOffline&&globalThis.__zaloOffline.active&&!Array.isArray(this.props.focusId))this.props.focusId=[];'
    ];
    const missing = required.filter(value => !source.includes(value));
    if (missing.length) {
      throw new Error(`Offline startup: incomplete marked authentication patch (${missing.join(', ')})`);
    }
    return source;
  }

  const offline = 'globalThis.__zaloOffline&&globalThis.__zaloOffline.active';
  const startupPatches = [
    [/([a-zA-Z0-9_$]+)\.propTypes=\{replyMessage:/,
      (match, id) => `${id}.defaultProps={focusId:[]};${id}.propTypes={replyMessage:`],
    [/this\.isSendTimeOutside=this\.isSendTimeOutside&&this\.props\.isLast;let\s+([a-zA-Z0-9_$]+)=([a-zA-Z0-9_$]+)\.props\.children;/,
      (match, childVar, propVar) => `if(!${propVar})return null;this.isSendTimeOutside=this.isSendTimeOutside&&this.props.isLast;let ${childVar}=${propVar}.props.children;`],
    ['_isNeedFetchFriendList(){', `_isNeedFetchFriendList(){if(${offline})return!1;`],
    ['_isNeedFetchGroupList(){', `_isNeedFetchGroupList(){if(${offline})return!1;`]
  ];

  for (const [from, to] of startupPatches) {
    source = replaceRequired(source, from, to, 'local preload guard');
  }

  const legacyHiddenChat = ',!this.enableHiddenChat())return Promise.resolve();';
  const modernHiddenChat = /([a-zA-Z0-9_$]+)=this\.enableHiddenChat\(\);if\((.*?),\!\1\)return;await this\._migrateLegacyPinToVault\(\);/;
  if (source.includes(legacyHiddenChat)) {
    source = replaceRequired(source, legacyHiddenChat, `,(!this.enableHiddenChat()||(${offline})))return Promise.resolve();`, 'hidden chat guard');
  } else {
    source = replaceRequired(source, modernHiddenChat,
      (match, varName, info) => `${varName}=this.enableHiddenChat();if(${info},(!${varName}||(${offline})))return;await this._migrateLegacyPinToVault();`,
      'hidden chat guard');
  }

  const sessionRenewalRegex = /async function\s+([a-zA-Z0-9_$]+)\s*\(([^)]*)\)\{([^{}]*?Session end time is empty)/;
  source = replaceRequired(source, sessionRenewalRegex,
    `async function $1($2){if(${offline})return!1;$3`, 'session renewal');

  source = patchAuthMethods(source);
  return MARKER + '\n' + source;
}

function logPatchResult(label, target, status) {
  if (status === 'patched') logger.dim(`Patched ${label} in ${target}`);
  else logger.info(`${label} already present in ${target}; no changes needed`);
}

async function main(appDir = process.env.ZALO_APP_DIR || APP_DIR) {
  logger.info('Patching Offline startup integration...');
  try {
    const pc = path.join(appDir, 'pc-dist');
    const lazy = path.join(pc, 'lazy');
    const preloadPath = path.join(appDir, 'main-dist', 'preload-render.js');
    const sharedPreloadPath = path.join(appDir, 'main-dist', 'preload-shared-worker.js');
    const pagePaths = ['index.html', 'login.html'].map(name => path.join(pc, name));

    if (!fs.existsSync(lazy)) throw new Error('Offline startup: extracted app is missing');
    for (const filePath of [preloadPath, sharedPreloadPath, ...pagePaths]) {
      if (!fs.existsSync(filePath)) throw new Error('Offline startup target not found: ' + filePath);
    }

    const bundles = fs.readdirSync(lazy).filter(name =>
      /^default-login-main-startup-shared-worker-znotification\..*\.js$/.test(name)
    );
    if (bundles.length !== 1) {
      throw new Error('Offline startup: expected exactly one authentication bundle');
    }

    // Prepare all outputs first. The patch never resolves, reads or copies a
    // plugin file: the offline plugin supplies its renderer through its own IPC.
    const bundlePath = path.join(lazy, bundles[0]);
    const originalBundle = fs.readFileSync(bundlePath, 'utf8');
    const patchedBundle = patchAuthBundle(originalBundle);
    const bundleStatus = patchedBundle === originalBundle ? 'already' : 'patched';
    const preload = patchPreloadContent(fs.readFileSync(preloadPath, 'utf8'));
    const sharedPreload = patchSharedPreloadContent(fs.readFileSync(sharedPreloadPath, 'utf8'));
    const pages = pagePaths.map(filePath => ({
      filePath,
      ...patchHtmlContent(fs.readFileSync(filePath, 'utf8'), path.basename(filePath))
    }));

    if (bundleStatus === 'patched') writeAtomic(bundlePath, patchedBundle);
    if (preload.status === 'patched') writeAtomic(preloadPath, preload.content);
    if (sharedPreload.status === 'patched') writeAtomic(sharedPreloadPath, sharedPreload.content);
    for (const page of pages) {
      if (page.status === 'patched') writeAtomic(page.filePath, page.content);
    }

    logPatchResult('Offline authentication hooks', bundles[0], bundleStatus);
    logPatchResult('Offline preload bridge', path.basename(preloadPath), preload.status);
    logPatchResult('Offline shared-worker state bridge', path.basename(sharedPreloadPath), sharedPreload.status);
    for (const page of pages) {
      logPatchResult('Offline HTML renderer bootstrap', path.basename(page.filePath), page.status);
    }
    logger.success('Offline startup integration verified for all supported Zalo pages');

    return {
      bundle: bundles[0],
      preload: preload.status,
      sharedPreload: sharedPreload.status,
      pages: pages.map(page => ({ file: path.basename(page.filePath), status: page.status }))
    };
  } catch (error) {
    logger.error('Offline startup patch failed:', error.message);
    throw error;
  }
}

if (require.main === module) {
  main().catch(() => { process.exitCode = 1; });
}

module.exports = {
  main,
  patchAuthBundle,
  MARKER,
  PRELOAD_MARKER,
  HTML_MARKER,
  patchPreloadContent,
  patchSharedPreloadContent,
  SHARED_PRELOAD_MARKER,
  patchHtmlContent
};
