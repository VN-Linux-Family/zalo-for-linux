const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');

const APP_DIR = path.join(__dirname, '..', '..', 'app');

async function main() {
  const mainJsPath = path.join(APP_DIR, 'main-dist', 'main.js');

  if (!fs.existsSync(mainJsPath)) {
    logger.warn('main.js not present, skipping titlebar patch');
    return;
  }

  let content = fs.readFileSync(mainJsPath, 'utf8');
  const FRAME_FLAG = '!global.__zaloNativeWayland';

  if (content.includes(`frame:${FRAME_FLAG}`)) {
    logger.dim('main.js title bar already patched');
    return;
  }

  try {
    const root = parseAst(content);
    const values = [...root.findAll('!1'), ...root.findAll('!0'), ...root.findAll('false'), ...root.findAll('true')];
    const pairNode = values.find(n => {
      const p = n.parent();
      return p && p.kind() === 'pair' && p.field('key') && p.field('key').text() === 'frame';
    });
    if (pairNode) {
      const edit = pairNode.parent().replace(`frame:${FRAME_FLAG}`);
      const updated = applyEdits(root, [edit]);
      fs.writeFileSync(mainJsPath, updated, 'utf8');
      logger.dim('Patched main.js via AST: native title bar except on native Wayland');
      return;
    }
  } catch (err) {
    logger.warn('AST parser error on titlebar patch, trying fallback:', err.message);
  }

  const FRAME = `T,frame:${FRAME_FLAG}`;
  if (content.includes('T,frame:!1') || content.includes('T,frame:!0')) {
    content = content.replace(/T,frame:!(?:1|0)(?![\w.])/g, FRAME);
    fs.writeFileSync(mainJsPath, content, 'utf8');
    logger.dim('Patched main.js via fallback: native title bar except on native Wayland');
  } else {
    logger.warn('Pattern T,frame:!1 not found in main.js, skipping patch');
  }
}

if (require.main === module) {
  main();
}

module.exports = { main };