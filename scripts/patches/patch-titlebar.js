const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');

const { APP_DIR, APP_MAIN_DIST } = require('../utils/paths');
const FRAME_FLAG = '!global.__zaloNativeWayland';

// The login/main window is the only one sized by MIN_LOGIN_HEIGHT. Matching the
// first `frame:` in the file broke when Zalo 26.11 added an earlier window (#188).
function findMainFramePair(root) {
  return root.findAll({ rule: { kind: 'pair' } }).find(pair => {
    if (pair.field('key')?.text() !== 'frame') return false;
    const object = pair.parent();
    if (!object || object.kind() !== 'object') return false;
    return object.children().some(sibling =>
      sibling.kind() === 'pair' &&
      sibling.field('key')?.text() === 'minHeight' &&
      /\bMIN_LOGIN_HEIGHT$/.test(sibling.field('value')?.text() ?? ''));
  });
}

// Returns { content, status } where status is 'patched', 'already' or 'missing'.
function patchContent(content) {
  try {
    const pair = findMainFramePair(parseAst(content));
    if (pair) {
      if (pair.field('value').text() === FRAME_FLAG) return { content, status: 'already' };
      const root = pair.getRoot().root();
      return { content: applyEdits(root, [pair.replace(`frame:${FRAME_FLAG}`)]), status: 'patched' };
    }
  } catch (err) {
    logger.warn('AST parser error on titlebar patch, trying fallback:', err.message);
  }

  if (content.includes(`T,frame:${FRAME_FLAG}`)) return { content, status: 'already' };
  if (content.includes('T,frame:!1') || content.includes('T,frame:!0')) {
    return {
      content: content.replace(/T,frame:!(?:1|0)(?![\w.])/g, `T,frame:${FRAME_FLAG}`),
      status: 'patched',
    };
  }
  return { content, status: 'missing' };
}

async function main() {
  const mainJsPath = path.join(APP_MAIN_DIST, 'main.js');

  if (!fs.existsSync(mainJsPath)) {
    logger.warn('main.js not present, skipping titlebar patch');
    return;
  }

  const result = patchContent(fs.readFileSync(mainJsPath, 'utf8'));
  if (result.status === 'already') {
    logger.dim('main.js title bar already patched');
  } else if (result.status === 'patched') {
    fs.writeFileSync(mainJsPath, result.content, 'utf8');
    logger.dim('Patched main.js: native title bar except on native Wayland');
  } else {
    logger.warn('Main window frame option not found in main.js, skipping patch');
  }
}

if (require.main === module) {
  main();
}

module.exports = { main, patchContent };
