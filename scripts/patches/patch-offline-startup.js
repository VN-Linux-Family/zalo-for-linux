'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const APP_DIR = path.join(__dirname, '..', '..', 'app');
const PLUGIN_DIR = path.join(__dirname, '..', '..', 'plugins', 'offline');
const MARKER = '/* zalo-linux-offline-v1 */';

function replaceRequired(source, from, to, label) {
  if (from instanceof RegExp) {
    if (!from.test(source)) throw new Error(`Offline startup: missing ${label}; upstream bundle changed`);
    return source.replace(from, to);
  }
  if (!source.includes(from)) throw new Error(`Offline startup: missing ${label}; upstream bundle changed`);
  return source.split(from).join(to);
}

function patchAuthBundle(source) {
  if (source.includes(MARKER)) return source;
  const offline = 'globalThis.__zaloOffline&&globalThis.__zaloOffline.active';
  const startupPatches = [
    // Cached messages (including nested quoted messages) can mount before a
    // highlight request exists. The upstream renderer assumes an array and
    // its error boundary otherwise removes the entire conversation pane.
    [/([a-zA-Z0-9_$]+)\.propTypes=\{replyMessage:/,
      (match, id) => `${id}.defaultProps={focusId:[]};${id}.propTypes={replyMessage:`],
    [/this\.isSendTimeOutside=this\.isSendTimeOutside&&this\.props\.isLast;let\s+([a-zA-Z0-9_$]+)=([a-zA-Z0-9_$]+)\.props\.children;/,
      (match, childVar, propVar) => `if(!${propVar})return null;this.isSendTimeOutside=this.isSendTimeOutside&&this.props.isLast;let ${childVar}=${propVar}.props.children;`],
    ['apiEnable(e,t){let n=a.default.cloud.enable;',
      `apiEnable(e,t){if(${offline})return!1;let n=a.default.cloud.enable;`],
    [/init\(e\)\{return this\._getUserData=e,([a-zA-Z0-9_$]+)\.b\.clearUpdateInfoTimer\(\),this\._doRequest\(\)\}/,
      (match, timerMod) => `init(e){return this._getUserData=e,${timerMod}.b.clearUpdateInfoTimer(),${offline}?Promise.resolve():this._doRequest()}`],
    ['_isNeedFetchFriendList(){', `_isNeedFetchFriendList(){if(${offline})return!1;`],
    ['_isNeedFetchGroupList(){', `_isNeedFetchGroupList(){if(${offline})return!1;`]
  ];
  // Keep the DB reads and UI dispatches, but do not wait for optional server
  // refreshes before showing cached contacts, groups and conversations.
  for (const [from, to] of startupPatches) {
    source = replaceRequired(source, from, to, 'local preload guard');
  }

  // Hidden chat guard (supports legacy return Promise.resolve() and modern async return)
  const legacyHiddenChat = ',!this.enableHiddenChat())return Promise.resolve();';
  const modernHiddenChat = /([a-zA-Z0-9_$]+)=this\.enableHiddenChat\(\);if\((.*?),\!\1\)return;await this\._migrateLegacyPinToVault\(\);/;
  if (source.includes(legacyHiddenChat)) {
    source = replaceRequired(source, legacyHiddenChat, `,(!this.enableHiddenChat()||(${offline})))return Promise.resolve();`, 'hidden chat guard');
  } else {
    source = replaceRequired(source, modernHiddenChat,
      (match, varName, info) => `${varName}=this.enableHiddenChat();if(${info},(!${varName}||(${offline})))return;await this._migrateLegacyPinToVault();`,
      'hidden chat guard');
  }

  // X4fA owns authentication; dThN opens the existing per-user SQLite/IDB
  // database and loads PROFILE_ME. Only the network login-info boundary changes.
  source = replaceRequired(source, 'fetchLoginInfo(e,t,n=!1){',
    'fetchLoginInfo(e,t,n=!1){return globalThis.__zaloOffline?' +
    'globalThis.__zaloOffline.fetchLoginInfo(()=>this._linuxFetchLoginInfo(e,t,n),' +
    '{clientId:e,userId:this.getMostRecentLoggedInUserId(),' +
    'onOffline:()=>a("gwig").b.setStatusDisconnectNetwork()}):this._linuxFetchLoginInfo(e,t,n)}' +
    '_linuxFetchLoginInfo(e,t,n=!1){', 'login-info method');

  const sessionRenewalRegex = /async function\s+([a-zA-Z0-9_$]+)\s*\(([^)]*)\)\{([^{}]*?Session end time is empty)/;
  source = replaceRequired(source, sessionRenewalRegex,
    `async function $1($2){if(${offline})return!1;$3`, 'session renewal');

  for (const hook of ['async logout(e,t=null){', 'async logoutFacebook(){', 'async _doLogout(t,n){']) {
    source = replaceRequired(source, hook,
      hook + 'if(globalThis.__zaloOffline)await globalThis.__zaloOffline.invalidate();', hook);
  }
  for (const hook of ['onKickoutSession(e,t,n){', 'clearLastSignin(){', '_onLoginSuccess(e,t,n,a,s){']) {
    source = replaceRequired(source, hook,
      hook + 'if(globalThis.__zaloOffline)globalThis.__zaloOffline.invalidate();', hook);
  }

  const readyRegex = /this\.props\.dispatch\(\{type:([a-zA-Z0-9_$]+)\.FetchActions\.USERID_FETCHED,payload:([a-zA-Z0-9_$]+)\}\),this\.phoneNumber/;
  source = replaceRequired(source, readyRegex,
    `this.props.dispatch({type:$1.FetchActions.USERID_FETCHED,payload:$2}),globalThis.__zaloOffline&&globalThis.__zaloOffline.markReady(),this.phoneNumber`, 'profile ready');
  return MARKER + '\n' + source;
}

async function main(appDir = APP_DIR) {
  const pc = path.join(appDir, 'pc-dist');
  const lazy = path.join(pc, 'lazy');
  if (!fs.existsSync(lazy)) throw new Error('Offline startup: extracted app is missing');
  const bundles = fs.readdirSync(lazy).filter(name => /^default-login-main-startup-shared-worker-znotification\..*\.js$/.test(name));
  if (bundles.length !== 1) throw new Error('Offline startup: expected exactly one authentication bundle');
  // Validate every anchor before changing any extracted files.
  const bundle = path.join(lazy, bundles[0]);
  const patched = patchAuthBundle(fs.readFileSync(bundle, 'utf8'));
  const preload = path.join(appDir, 'main-dist', 'preload-render.js');
  let preloadSource = fs.readFileSync(preload, 'utf8');
  if (!preloadSource.includes(MARKER)) preloadSource += '\n;' + MARKER + '\n' + fs.readFileSync(path.join(PLUGIN_DIR, 'preload.js'), 'utf8');
  else if (!preloadSource.includes("refresh: () => ipcRenderer.invoke('linux-offline:refresh')")) {
    preloadSource = replaceRequired(preloadSource,
      "clear: () => ipcRenderer.invoke('linux-offline:clear')",
      "clear: () => ipcRenderer.invoke('linux-offline:clear'),\n    refresh: () => ipcRenderer.invoke('linux-offline:refresh')",
      'offline preload refresh');
  }
  const pages = ['index.html', 'login.html'].map(name => {
    const file = path.join(pc, name);
    let source = fs.readFileSync(file, 'utf8');
    if (!source.includes('linux-offline.js')) source = replaceRequired(source, '<head>',
      '<head><script src="linux-offline.js"></script>', `${name} head`);
    return [file, source];
  });
  fs.writeFileSync(bundle, patched);
  fs.writeFileSync(preload, preloadSource);
  fs.copyFileSync(path.join(PLUGIN_DIR, 'renderer.js'), path.join(pc, 'linux-offline.js'));
  for (const [file, source] of pages) fs.writeFileSync(file, source);
  logger.success('Offline startup patch applied');
}

if (require.main === module) main().catch(error => { console.error(error.message); process.exitCode = 1; });
module.exports = { main, patchAuthBundle };
