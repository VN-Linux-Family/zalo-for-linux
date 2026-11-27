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
  return `/*wco-theme-zalo-hook-v6*/
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
  if (content.includes(IPC_CHANNEL) && content.includes('setTitleBarOverlay({ color:')) return { content, status: 'already' };
  const updated = content + MAIN_LISTENER;
  parseAst(updated);
  return { content: updated, status: 'patched' };
}

function patchPreloadContent(content) {
  if (content.includes('setWcoTheme:{apiKey:"' + IPC_CHANNEL + '"')) return { content, status: 'already' };
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

function patchReactContent(content) {
  let root = parseAst(content);
  if (content.includes(REACT_MARKER)) {
    if (content.includes('wco-theme-zalo-hook-v6')) return { content, status: 'already' };
    throw new Error('WCO React patch: legacy or unknown bridge found; expected pristine Zalo bundle');
  }

  const renderFunctions = [
    ...root.findAll({ rule: { kind: 'method_definition' } }),
    ...root.findAll({ rule: { kind: 'arrow_function' } }),
  ].filter(render => {
    const values = stringValues(render);
    return values.includes('titleBar') &&
      render.text().includes('.createElement(');
  });
  const modules = new Map();
  for (const render of renderFunctions) {
    const createCalls = render.findAll({ rule: { kind: 'call_expression' } }).filter(call =>
      call.field('function')?.text().endsWith('.createElement')
    );
    const returns = render.findAll({ rule: { kind: 'return_statement' } }).filter(statement =>
      stringValues(statement).includes('titleBar')
    );
    if (!createCalls.length || !returns.length) continue;
    const module = findModuleFunction(render);
    if (!module) continue;
    const react = createCalls[0].field('function').text().replace(/\.createElement$/, '');
    const entry = modules.get(module) || { module, react, methods: [] };
    entry.methods.push({ render, statement: returns[0] });
    modules.set(module, entry);
  }
  if (!modules.size) {
    throw new Error('WCO React patch: React titlebar render method not found by AST');
  }

  const themeModuleId = findThemeHookModuleId(content, root);
  const reactBridge = buildReactBridge(themeModuleId);
  const edits = [];
  for (const { module, react, methods: targets } of modules.values()) {
    let moduleText = module.text();
    const body = module.field('body') || module.findAll({ rule: { kind: 'statement_block' } })[0];
    if (!body) throw new Error('WCO React patch: module function body not found');
    const bodyAt = moduleText.indexOf(body.text());
    if (bodyAt < 0) throw new Error('WCO React patch: cannot locate module body');
    for (const { render, statement } of targets) {
      const methodText = render.text();
      const argument = statement.text().replace(/^return\s*/, '');
      const wrapped = `${react}.createElement(${react}.Fragment,null,${argument},${react}.createElement(${REACT_MARKER},null))`;
      const methodAt = moduleText.indexOf(methodText);
      const argumentAt = moduleText.indexOf(argument, methodAt);
      if (methodAt < 0 || argumentAt < 0) throw new Error('WCO React patch: cannot locate titlebar render expression');
      moduleText = moduleText.slice(0, argumentAt) + wrapped + moduleText.slice(argumentAt + argument.length);
    }
    const declaration = reactBridge.replaceAll('React.', `${react}.`);
    const bodySource = body.text();
    const directive = /^\{\s*(?:(["'])use strict\1\s*;)?/.exec(bodySource);
    const declarationAt = bodyAt + (directive ? directive[0].length : 1);
    moduleText = moduleText.slice(0, declarationAt) + '\n' + declaration + '\n' + moduleText.slice(declarationAt);
    edits.push(module.replace(moduleText));
  }
  return { content: applyEdits(root, edits), status: 'patched' };
}

function patchFile(filePath, transform) {
  if (!fs.existsSync(filePath)) throw new Error(`WCO React patch: file not found: ${filePath}`);
  const original = fs.readFileSync(filePath, 'utf8');
  const result = transform(original);
  if (result.status !== 'already') fs.writeFileSync(filePath, result.content, 'utf8');
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
      if (!String(error && error.message).includes('React titlebar render method not found by AST')) throw error;
      logger.dim(`WCO React bridge skipped (no titlebar component): ${path.relative(APP_PC_DIST, file)}`);
      continue;
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
module.exports = { main, patchMainContent, patchPreloadContent, patchReactContent, findCompactTargets, findLazyTargets, findThemeHookModuleId, buildReactBridge, IPC_CHANNEL, MAIN_MARKER, REACT_MARKER, MAIN_LISTENER };
