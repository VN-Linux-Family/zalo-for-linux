'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { parseAst, applyEdits } = require('../utils/ast-transform');
const { APP_MAIN_DIST } = require('../utils/paths');

const PLATFORM_GUARD = 'if("darwin"!==process.platform){this.isAppSigned=!1;return!1;}';

function patchMainContent(source) {
  if (source.includes(PLATFORM_GUARD)) return source;

  const root = parseAst(source);
  const matches = root.findAll({ rule: { kind: 'method_definition' } }).filter(method =>
    method.field('name')?.text() === 'checkAppSigned' &&
    method.text().includes('codesign') &&
    method.text().includes('isAppSigned')
  );

  if (matches.length !== 1) {
    throw new Error(`Linux guards: expected one codesign checkAppSigned method, found ${matches.length}`);
  }

  const body = matches[0].field('body');
  if (!body || !body.text().startsWith('{')) {
    throw new Error('Linux guards: checkAppSigned method has no supported body');
  }

  const edit = body.replace(`{${PLATFORM_GUARD}${body.text().slice(1)}`);
  return applyEdits(root, [edit]);
}

function patchFile(filePath) {
  const source = fs.readFileSync(filePath, 'utf8');
  const patched = patchMainContent(source);
  if (patched !== source) fs.writeFileSync(filePath, patched, 'utf8');
  return patched !== source;
}

async function main(mainDist = APP_MAIN_DIST) {
  const filePath = path.join(mainDist, 'main.js');
  if (!fs.existsSync(filePath)) {
    throw new Error('Linux guards: main bundle not found: ' + filePath);
  }

  if (patchFile(filePath)) {
    logger.dim('Disabled macOS codesign probing on non-macOS platforms');
  } else {
    logger.info('macOS codesign platform guard already present');
  }
  logger.success('Linux platform guards verified');
}

if (require.main === module) {
  main().catch(error => {
    logger.error('Linux guards patch failed:', error.message);
    process.exit(1);
  });
}

module.exports = { main, patchMainContent, patchFile, PLATFORM_GUARD };
