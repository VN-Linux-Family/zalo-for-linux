'use strict';

const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');
const { APP_MAIN_DIST } = require('../utils/paths');

// Keep Zalo's custom frameless titlebar on Linux. The renderer patch removes
// the fixed macOS gutter and uses Electron 43 WCO safe-area geometry for the
// desktop-configured control position; the app remains frameless.
const FRAME_VALUE = '!1';
const TITLEBAR_VALUE = '"hidden"';
const OVERLAY_VALUE = '"linux"===process.platform?{color:"rgba(0,0,0,0)",symbolColor:"#000000"}:false';
const PHOTO_OVERLAY_VALUE = '"linux"===process.platform?{color:"rgba(0,0,0,0)",symbolColor:"#ffffff"}:false';

function findWindowOptionObjects(root) {
  const styles = root.findAll({
    rule: { kind: 'pair', has: { field: 'key', regex: '^titleBarStyle$' } },
  }).filter(pair => pair.field('value')?.text() === TITLEBAR_VALUE);

  return styles
    .map(pair => pair.parent())
    .filter(object => object?.kind() === 'object');
}

function collectEdits(content, root = parseAst(content)) {
  const options = findWindowOptionObjects(root);
  if (!options.length) {
    throw new Error('Linux client-titlebar patch: original Zalo hidden-titlebar windows not found');
  }

  const edits = [];
  let hasLoginWindow = false;

  for (const object of options) {
    const pairs = object.children().filter(node => node.kind() === 'pair');
    const frame = pairs.find(pair => pair.field('key')?.text() === 'frame');
    const titleBarStyle = pairs.find(pair => pair.field('key')?.text() === 'titleBarStyle');
    const titleBarOverlay = pairs.find(pair => pair.field('key')?.text() === 'titleBarOverlay');
    if (titleBarOverlay) {
      throw new Error('Linux client-titlebar patch: expected original Zalo window options without titleBarOverlay');
    }

    const isPhotoViewer = /title:\s*['\"]Zalo Photo['\"]/.test(object.text());
    const desiredOverlay = isPhotoViewer ? PHOTO_OVERLAY_VALUE : OVERLAY_VALUE;
    const minHeight = pairs.find(pair => pair.field('key')?.text() === 'minHeight');
    const isLoginWindow = /\bMIN_LOGIN_HEIGHT\b/.test(minHeight?.field('value')?.text() ?? '');
    hasLoginWindow ||= isLoginWindow;

    if (isLoginWindow) {
      if (!frame) throw new Error('Linux client-titlebar patch: original login frame option not found');
      if (frame.field('value').text() !== FRAME_VALUE) edits.push(frame.replace(`frame:${FRAME_VALUE}`));
    }
    if (!titleBarStyle) throw new Error('Linux client-titlebar patch: original hidden titlebar option not found');
    edits.push(titleBarStyle.replace(`titleBarStyle:${TITLEBAR_VALUE},titleBarOverlay:${desiredOverlay}`));
  }

  if (!hasLoginWindow) {
    throw new Error('Linux client-titlebar patch: original login window options not found');
  }
  return { edits };
}

function patchInitialWindowTheme(content, root = parseAst(content)) {
  const candidates = root.findAll({ rule: { kind: 'variable_declarator' } }).filter(declaration => {
    const value = declaration.field('value');
    if (value?.kind() !== 'object') return false;
    const keys = value.children().filter(node => node.kind() === 'pair')
      .map(pair => pair.field('key')?.text().replace(/["']/g, ''));
    return keys.includes('titleBarOverlay') && keys.includes('backgroundColor');
  });
  const matches = [];
  for (const declaration of candidates) {
    const statement = declaration.parent();
    if (!statement || statement.kind() !== 'lexical_declaration') continue;
    const themeReads = statement.findAll({ rule: { kind: 'variable_declarator' } }).filter(node => {
      const value = node.field('value');
      const args = value?.field('arguments')?.namedChildren() ?? [];
      return value?.kind() === 'call_expression' &&
        value.field('function')?.kind() === 'member_expression' &&
        value.field('function').text().endsWith('.get') && args[0]?.kind() === 'string' &&
        JSON.parse(args[0].text()) === 'app_theme';
    });
    if (themeReads.length !== 1) continue;
    const configName = themeReads[0].field('name')?.text();
    const overlay = declaration.field('value').children().find(node =>
      node.kind() === 'pair' && node.field('key')?.text().replace(/["']/g, '') === 'titleBarOverlay'
    );
    if (configName && overlay) matches.push({ overlay, configName });
  }
  if (matches.length !== 1) {
    throw new Error(`Linux client-titlebar patch: expected one initial main-window theme options object, found ${matches.length}`);
  }
  const { overlay, configName } = matches[0];
  const value = overlay.field('value');
  if (value?.text().includes('__zaloInitialWcoTheme')) {
    throw new Error('Linux client-titlebar patch: initial WCO theme already patched');
  }
  const replacement = `titleBarOverlay:"linux"===process.platform?{color:"rgba(0,0,0,0)",symbolColor:${configName}&&${configName}.theme==="dark"?"#ffffff":"#000000"}:false`;
  return applyEdits(root, [overlay.replace(replacement)]);
}

function patchTitlebarContent(content) {
  if (content.includes('__zaloPhotoViewer')) {
    throw new Error('Linux client-titlebar patch: expected original Zalo main bundle without WCO markers');
  }

  const photoTitleIndex = content.indexOf('title:"Zalo Photo"');
  if (photoTitleIndex < 0) throw new Error('Linux photo viewer patch: original constructor not found');
  const maximizeIndex = content.indexOf('o.maximize&&', photoTitleIndex);
  if (maximizeIndex < 0) throw new Error('Linux photo viewer patch: constructor tail not found');
  let output = content.slice(0, maximizeIndex) + 'y.__zaloPhotoViewer=!0,' + content.slice(maximizeIndex);

  const setterAnchor = 'setPhotoViewerWindow:function(e){y=e;';
  if (!output.includes(setterAnchor)) throw new Error('Linux photo viewer patch: original window registration not found');
  output = output.replace(setterAnchor, 'setPhotoViewerWindow:function(e){y=e,e&&(e.__zaloPhotoViewer=!0);');

  const themeUpdateAnchor = 'updateBgColorByAppConfig:function(e)';
  const themeBackgroundAnchor = 'r&&Ae.setBackgroundColor(r)';
  if (!output.includes(themeUpdateAnchor) || !output.includes(themeBackgroundAnchor)) {
    throw new Error('Linux client-titlebar patch: original Zalo app-theme background update not found');
  }
  if (output.includes('setTitleBarOverlay(')) {
    throw new Error('Linux client-titlebar patch: unexpected titlebar overlay mutation in original Zalo theme path');
  }
  return patchInitialWindowTheme(output);
}

function patchContent(content) {
  const root = parseAst(content);
  const result = collectEdits(content, root);
  const output = patchTitlebarContent(result.edits.length ? applyEdits(root, result.edits) : content);
  return { content: output, status: 'patched' };
}

async function main() {
  const mainJsPath = path.join(APP_MAIN_DIST, 'main.js');
  if (!fs.existsSync(mainJsPath)) {
    throw new Error('Linux client-titlebar patch: original main.js not found');
  }

  const result = patchContent(fs.readFileSync(mainJsPath, 'utf8'));
  if (result.status === 'patched') {
    fs.writeFileSync(mainJsPath, result.content, 'utf8');
    logger.dim('Patched main.js: keep the custom frameless Linux titlebar');
  }
}

if (require.main === module) main();
module.exports = { main, patchContent, patchTitlebarContent, patchInitialWindowTheme, collectEdits, FRAME_VALUE, TITLEBAR_VALUE, OVERLAY_VALUE, PHOTO_OVERLAY_VALUE };
