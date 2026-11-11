'use strict';

const fs = require('fs');
const path = require('path');
const vm = require('vm');
const logger = require('../utils/logger');
const { parseAst } = require('../utils/ast-transform');
const { APP_PC_DIST } = require('../utils/paths');

const MARKER = 'zalo-language-bridge';
const VALID_INDEX_GUARD = 'this.currentLanguageIndex===0||this.currentLanguageIndex===1';
// The semicolon is intentional: minified upstream methods may end in a call
// expression with no semicolon. Never concatenate a new `if` statement to it.
const EVENT_BRIDGE = `/* ${MARKER} */;if(typeof window!=="undefined"&&typeof CustomEvent==="function"&&(${VALID_INDEX_GUARD})){window.__ZALO_LANGUAGE_BRIDGE__={version:1,locale:this["getCurrentLanguageName"](),timestamp:Date.now()};window.dispatchEvent(new CustomEvent("zalo-language:changed",{detail:window.__ZALO_LANGUAGE_BRIDGE__}));}`;

function collect(node, predicate, out = []) {
  if (predicate(node)) out.push(node);
  for (const child of node.children()) collect(child, predicate, out);
  return out;
}

function managerClass(root) {
  const candidates = root.findAll('new class { $$$BODY }').filter((node) => {
    const value = node.text();
    return value.includes('currentLanguageIndex') && value.includes('getCurrentLanguageName') &&
      value.includes('callUpdate') && value.includes('removeUpdate');
  });
  if (candidates.length !== 1) {
    throw new Error(`Expected exactly one Zalo Language Manager class, found ${candidates.length}`);
  }
  return candidates[0];
}

function methods(manager, name) {
  return collect(manager, (node) => node.kind() === 'method_definition' &&
    node.children().some((child) => child.kind() === 'property_identifier' && child.text() === name));
}

function validateJavaScript(source, filename = 'zalo-language-bridge.js') {
  // ast-grep is used for structural edits, but its parser is error-tolerant.
  // vm.Script is the final syntax gate used by the actual JavaScript engine.
  new vm.Script(source, { filename });
}

function patchContent(source) {
  if (typeof source !== 'string') throw new TypeError('Bundle source must be a string');
  let working = source;
  let root = parseAst(working);
  let manager = managerClass(root);
  const markerWasPresent = working.includes('/* ' + MARKER + ' */');
  if (markerWasPresent && working.split('/* ' + MARKER + ' */').length - 1 !== 1) {
    throw new Error('Duplicate Zalo language bridge markers; refusing to continue');
  }

  if (markerWasPresent) {
    const change = methods(manager, 'changeToLang');
    if (change.length !== 1 || !change[0].text().includes(EVENT_BRIDGE)) {
      throw new Error('Existing Zalo language bridge marker does not match the current patch; refusing to patch over it');
    }
    validateJavaScript(working);
    if (!working.includes(VALID_INDEX_GUARD) ||
        !working.includes('version:1,locale:this["getCurrentLanguageName"](),timestamp:Date.now()')) {
      throw new Error('Existing Zalo language bridge failed current postconditions');
    }
    return { content: working, status: 'already' };
  }

  const changeMethods = methods(manager, 'changeToLang');
  if (changeMethods.length !== 1) {
    throw new Error('Zalo Language Manager change method is ambiguous; refusing unsafe patch');
  }
  const changeText = changeMethods[0].text();
  const updated = root.commitEdits([
    changeMethods[0].replace(changeText.slice(0, -1) + EVENT_BRIDGE + '}')
  ]);

  parseAst(updated);
  validateJavaScript(updated);
  const count = updated.split('/* ' + MARKER + ' */').length - 1;
  if (count !== 1 || !updated.includes(VALID_INDEX_GUARD) ||
      !updated.includes('version:1,locale:this["getCurrentLanguageName"](),timestamp:Date.now()')) {
    throw new Error('Language bridge postconditions failed');
  }
  return { content: updated, status: 'patched' };
}

function patchFile(filePath) {
  if (!fs.existsSync(filePath)) throw new Error('Zalo language bundle not found: ' + filePath);
  const result = patchContent(fs.readFileSync(filePath, 'utf8'));
  if (result.status === 'patched') fs.writeFileSync(filePath, result.content, 'utf8');
  return result.status;
}

function findTargetBundles(appPcDist) {
  const compactBundles = fs.readdirSync(appPcDist)
    .filter((name) => /^compact-app-pc.*\.js$/.test(name));
  const lazyDir = path.join(appPcDist, 'lazy');
  const loginBundles = fs.existsSync(lazyDir)
    ? fs.readdirSync(lazyDir).filter((name) => /^default-login-main-startup-shared-worker-znotification\..*\.js$/.test(name))
    : [];
  if (compactBundles.length !== 1) {
    throw new Error(`Expected one compact-app-pc bundle, found ${compactBundles.length}`);
  }
  if (loginBundles.length !== 1) {
    throw new Error(`Expected one active login/main Language Manager bundle, found ${loginBundles.length}`);
  }
  return [
    path.join(appPcDist, compactBundles[0]),
    path.join(lazyDir, loginBundles[0])
  ];
}

async function main(appPcDist = APP_PC_DIST) {
  logger.info('Patching Zalo language bridge...');
  try {
    // These are the two documented UI bundle targets. No other workers or
    // application render bundles are modified.
    const targets = findTargetBundles(appPcDist);
    const results = targets.map((filePath) => ({ filePath, status: patchFile(filePath) }));
    for (const result of results) {
      const name = path.relative(appPcDist, result.filePath);
      if (result.status === 'patched') logger.dim(`Patched Zalo locale event in ${name}`);
      else logger.info(`Zalo locale event already present in ${name}; no changes needed`);
    }
    logger.success('Zalo locale bridge syntax and AST checks passed for both documented bundles');
    return {
      filePath: results[0].filePath,
      status: results.every((result) => result.status === 'already') ? 'already' : 'patched',
      results
    };
  } catch (error) {
    logger.error('Zalo language bridge patch failed:', error.message);
    throw error;
  }
}

if (require.main === module) {
  main().catch(() => process.exit(1));
}

module.exports = {
  main, patchContent, patchFile, findTargetBundles, validateJavaScript,
  MARKER, VALID_INDEX_GUARD, EVENT_BRIDGE
};
