'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');
const { APP_MAIN_DIST, APP_PC_DIST } = require('../utils/paths');

const IPC_CHANNEL = 'zalo-wco-theme';
const MAIN_MARKER = '__zaloWcoIpcMain';
const REACT_MARKER = '__zaloWcoThemeBridge';
function buildReactBridge(themeModuleId) {
  return `
const __zaloWcoUseTheme=n(${JSON.stringify(themeModuleId)}).a;
const ${REACT_MARKER}=()=>{
  const {isDark}=__zaloWcoUseTheme();
  React.useEffect(()=>{
    if(window.$zapp&&typeof window.$zapp.setWcoTheme==="function")window.$zapp.setWcoTheme(isDark?"#ffffff":"#000000");
  },[isDark]);
  return null;
};`;
}

function findThemeHookModuleId(content, root = parseAst(content)) {
  // Search inside each webpack module function, not a fixed byte window after its
  // header. A neighboring module can otherwise satisfy the signature and make us
  // import an unrelated component as a hook (which crashes the titlebar render).
  const matches = root.findAll({ rule: { kind: 'function_expression' } }).filter(module => {
    const source = module.text();
    return /getThemeSetting/.test(source) && /SETTING_UPDATED/.test(source) &&
      /isDark/.test(source) && /useState/.test(source) && /useEffect/.test(source) &&
      /THEME\.DARK/.test(source);
  }).map(module => {
    const pair = module.parent();
    const key = pair?.kind() === 'pair' ? pair.field('key')?.text() : null;
    if (!key) return null;
    let id = key;
    if ((key.startsWith('"') && key.endsWith('"')) || (key.startsWith("'") && key.endsWith("'"))) {
      try { id = JSON.parse(key); } catch (_) { id = key.slice(1, -1); }
    }
    return { id, signature: module.text() };
  }).filter(Boolean);

  if (!matches.length || matches.some(match => match.signature !== matches[0].signature)) {
    throw new Error(`WCO React patch: expected one unique Zalo theme hook implementation, found ${matches.length}`);
  }
  // Some Zalo bundles embed the same hook under more than one webpack ID.
  // Identical implementations are interchangeable; differing matches fail closed.
  return matches[0].id;
}
const MAIN_LISTENER = `\n;(() => {\n  const { ipcMain: ${MAIN_MARKER}, BrowserWindow: __zaloWcoBrowserWindow } = require('electron');\n  ${MAIN_MARKER}.on('${IPC_CHANNEL}', (_event, symbolColor) => {\n    if (typeof symbolColor !== 'string' || !/^(#[0-9a-f]{3,8}|rgba?\\([^)]*\\)|hsla?\\([^)]*\\))$/i.test(symbolColor)) return;\n    for (const win of __zaloWcoBrowserWindow.getAllWindows()) {\n      try {\n        if (!win || win.isDestroyed() || win.__zaloPhotoViewer === true || typeof win.setTitleBarOverlay !== 'function') continue;\n        win.setTitleBarOverlay({ color: 'rgba(0,0,0,0)', symbolColor });\n      } catch (_) {}\n    }\n  });\n})();\n`;

function patchMainContent(content) {
  if (content.includes(MAIN_MARKER)) return { content, status: 'already' };
  const updated = content + MAIN_LISTENER;
  parseAst(updated);
  return { content: updated, status: 'patched' };
}

function patchPreloadContent(content) {
  if (content.includes('setWcoTheme:{apiKey:"zalo-wco-theme"')) return { content, status: 'already' };
  const root = parseAst(content);
  const objects = root.findAll({ rule: { kind: 'object' } }).filter(object =>
    object.children().some(node => node.kind() === 'pair' && node.field('key')?.text().replace(/["']/g, '') === 'getAppVisibilityStatus')
  );
  if (objects.length !== 1) throw new Error(`WCO React patch: expected one preload API map, found ${objects.length}`);
  const object = objects[0];
  const visibility = object.children().find(node => node.kind() === 'pair' && node.field('key')?.text().replace(/["']/g, '') === 'getAppVisibilityStatus');
  const typePair = visibility.field('value')?.findAll({ rule: { kind: 'pair' } }).find(pair => pair.field('key')?.text().replace(/[\"']/g, '') === 'type');
  const type = typePair?.field('value')?.text();
  const enumPrefix = type && type.replace(/\.(REQUEST|RAW_SEND|NOTIFY|SUBSCRIBE|UNSUBSCRIBE)$/, '');
  if (!enumPrefix) throw new Error('WCO React patch: cannot identify preload IPC type enum');
  const updatedObject = object.text().replace(/}\s*$/, `,setWcoTheme:{apiKey:"${IPC_CHANNEL}",type:${enumPrefix}.RAW_SEND}}`);
  return { content: applyEdits(root, [object.replace(updatedObject)]), status: 'patched' };
}

function stringValues(node) {
  return node.findAll({ rule: { kind: 'string' } }).map(value => {
    try { return JSON.parse(value.text()); } catch (_) { return value.text().slice(1, -1); }
  });
}

function findModuleFunction(method) {
  let node = method.parent();
  while (node) {
    if (node.kind() === 'function_declaration' || node.kind() === 'function_expression' || node.kind() === 'arrow_function') return node;
    node = node.parent();
  }
  return null;
}

const THEME_HOOK_MARKER = '__zaloWcoThemeHookSync';

function patchReactContent(content) {
  if (content.includes(THEME_HOOK_MARKER)) return { content, status: 'already' };
  const root = parseAst(content);
  const themeModuleId = findThemeHookModuleId(content, root);
  const modulePair = root.findAll({ rule: { kind: 'pair' } }).find(pair => {
    const key = pair.field('key')?.text().replace(/["']/g, '');
    return key === themeModuleId;
  });
  if (!modulePair) throw new Error(`WCO React patch: theme hook module ${themeModuleId} not found`);

  const module = modulePair.field('value');
  const hook = module.findAll({ rule: { kind: 'arrow_function' } }).find(fn =>
    /isDark\s*:/.test(fn.text()) && /THEME\.DARK/.test(fn.text()) && /return\s*\{/.test(fn.text())
  );
  if (!hook) throw new Error('WCO React patch: theme hook function not found');

  const hookSource = hook.text();
  const reactAlias = /Object\((\w+)\.useEffect\)\(/.exec(hookSource)?.[1];
  const themeAlias = /isDark\s*:\s*(\w+)\s*===\s*([\w$.]+\.DARK)/.exec(hookSource);
  const returnStatement = hook.findAll({ rule: { kind: 'return_statement' } }).find(statement => /isDark/.test(statement.text()));
  if (!reactAlias || !themeAlias || !returnStatement) {
    throw new Error('WCO React patch: cannot identify React alias or effective theme in Zalo theme hook');
  }

  const themeValue = themeAlias[1];
  const darkExpression = themeAlias[2];
  const effect = `Object(${reactAlias}.useEffect)(()=>{if(window.$zapp&&typeof window.$zapp.setWcoTheme==="function")window.$zapp.setWcoTheme(${themeValue}===${darkExpression}?"#ffffff":"#000000")},[${themeValue}]);`;
  const returnAt = hookSource.indexOf(returnStatement.text());
  if (returnAt < 0) throw new Error('WCO React patch: cannot locate theme hook return');
  const updatedHook = hookSource.slice(0, returnAt) + effect + returnStatement.text() + `/*${THEME_HOOK_MARKER}*/` + hookSource.slice(returnAt + returnStatement.text().length);
  const moduleSource = module.text();
  const hookAt = moduleSource.indexOf(hookSource);
  if (hookAt < 0) throw new Error('WCO React patch: cannot locate theme hook source');
  const updatedModule = moduleSource.slice(0, hookAt) + updatedHook + moduleSource.slice(hookAt + hookSource.length);
  const pairSource = modulePair.text();
  const moduleAt = pairSource.indexOf(moduleSource);
  if (moduleAt < 0) throw new Error('WCO React patch: cannot locate theme module source');
  const updatedPair = pairSource.slice(0, moduleAt) + updatedModule + pairSource.slice(moduleAt + moduleSource.length);
  const updated = applyEdits(root, [modulePair.replace(updatedPair)]);
  parseAst(updated);
  return { content: updated, status: 'patched' };
}

function patchFile(filePath, transform) {
  if (!fs.existsSync(filePath)) throw new Error(`WCO React patch: file not found: ${filePath}`);
  const original = fs.readFileSync(filePath, 'utf8');
  const result = transform(original);
  fs.writeFileSync(filePath, result.content, 'utf8');
  return result.status;
}

function findCompactTargets(directory) {
  if (!fs.existsSync(directory)) return [];
  return fs.readdirSync(directory, { withFileTypes: true })
    .filter(entry => entry.isFile() && /^compact-app-pc\..+\.js$/.test(entry.name))
    .map(entry => path.join(directory, entry.name))
    .sort();
}

function findLazyTargets(directory) {
  if (!fs.existsSync(directory)) return [];
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const file = path.join(directory, entry.name);
    if (entry.isDirectory()) return findLazyTargets(file);
    return entry.isFile() && entry.name.endsWith('.js') &&
      (/^login-startup\./.test(entry.name) || /^default-login-main-startup-shared-worker\.[^.]+\.js$/.test(entry.name)) ? [file] : [];
  });
}

async function main() {
  const mainPath = path.join(APP_MAIN_DIST, 'main.js');
  const preloadPath = path.join(APP_MAIN_DIST, 'preload-render.js');
  patchFile(mainPath, patchMainContent);
  patchFile(preloadPath, patchPreloadContent);
  const targets = [...findCompactTargets(APP_PC_DIST), ...findLazyTargets(path.join(APP_PC_DIST, 'lazy'))];
  if (!targets.length) throw new Error('WCO React patch: Zalo login/titlebar lazy bundles not found');
  let patched = 0;
  for (const file of targets) {
    let status;
    try {
      status = patchFile(file, patchReactContent);
    } catch (error) {
      const message = String(error && error.message);
      if (message.includes('expected one unique Zalo theme hook implementation, found 0')) {
        logger.dim(`WCO React theme hook skipped (not present): ${path.relative(APP_PC_DIST, file)}`);
        continue;
      }
      throw error;
    }
    if (status === 'patched') patched++;
    logger.dim(`WCO React bridge ${status}: ${path.relative(APP_PC_DIST, file)}`);
  }
  if (!patched && !fs.readFileSync(mainPath, 'utf8').includes(MAIN_MARKER)) {
    throw new Error('WCO React patch: no expected WCO patch markers found');
  }
  logger.success('WCO React theme bridge verified');
}

if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
module.exports = { main, patchMainContent, patchPreloadContent, patchReactContent, findCompactTargets, findLazyTargets, findThemeHookModuleId, IPC_CHANNEL, MAIN_MARKER, MAIN_LISTENER, THEME_HOOK_MARKER };
