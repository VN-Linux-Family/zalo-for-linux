const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { spawnSync } = require('child_process');
const { parseAst, applyEdits } = require('../utils/ast-transform');

const { APP_DIR, APP_MAIN_DIST: MAIN_DIR } = require('../utils/paths');
const LINUX_DIRECTORY_GUARD_PATTERN =
  /if\("linux"===process\.platform&&([A-Za-z_$][\w$]*)\.isDirectory\(\)\)return [A-Za-z_$][\w$]*\(e\)/;

function patchShellOpenLinux(content) {
  if (LINUX_DIRECTORY_GUARD_PATTERN.test(content)) return content;

  try {
    const root = parseAst(content);
    const openFn = root.find('function $FN($E) { return $OBJ.shell.openPath($E); }');
    const dirCheck = root.find('if (!$STAT.isDirectory()) { $$$BODY }');
    if (openFn && dirCheck) {
      const openPath = openFn.getMatch('FN').text();
      const stat = dirCheck.getMatch('STAT').text();
      const body = dirCheck.field('consequence');
      const edit = dirCheck.replace(
        `if("linux"===process.platform&&${stat}.isDirectory())return ${openPath}(e);if(!${stat}.isDirectory()){${body.text().slice(1)}`
      );
      logger.dim('Patched shell.openPath via AST');
      return applyEdits(root, [edit]);
    }
  } catch (err) {
    logger.warn('AST shell-open patch skipped, using fallback:', err.message);
  }

  const shellModuleStart = content.indexOf('Mz8P:function');
  if (shellModuleStart === -1) {
    throw new Error('Shell module not found.');
  }

  const shellModule = content.slice(shellModuleStart);
  const openPathMatch = shellModule.match(
    /function ([A-Za-z_$][\w$]*)\(e\)\{return [A-Za-z_$][\w$]*\.shell\.openPath\(e\)\}/
  );
  if (!openPathMatch) {
    throw new Error('Electron shell.openPath helper not found.');
  }
  const openPath = openPathMatch[1];

  const directoryBranchPattern =
    /if\(([A-Za-z_$][\w$]*)\)\{if\(!\1\.isDirectory\(\)\)\{/;
  const directoryBranchMatch = shellModule.match(directoryBranchPattern);
  if (!directoryBranchMatch) {
    throw new Error('Shell directory branch not found.');
  }
  const stat = directoryBranchMatch[1];

  const patchedShellModule = shellModule.replace(
    directoryBranchPattern,
    `if(${stat}){if("linux"===process.platform&&${stat}.isDirectory())return ${openPath}(e);` +
    `if(!${stat}.isDirectory()){`
  );
  return content.slice(0, shellModuleStart) + patchedShellModule;
}


function patchFile(filePath) {
  const original = fs.readFileSync(filePath, 'utf8');
  if (!original.includes('Mz8P:function') || !original.includes('shell.openPath')) return null;
  const patched = patchShellOpenLinux(original);
  if (patched !== original) fs.writeFileSync(filePath, patched, 'utf8');
  return patched !== original;
}

function patchFileIsolated(filePath) {
  const runner = 'const changed = require(process.argv[1]).patchFile(process.argv[2]); process.exitCode = changed === null ? 11 : changed ? 10 : 0;';
  const result = spawnSync(process.execPath, ['-e', runner, __filename, filePath], {
    stdio: 'inherit',
    env: process.env,
  });
  if (result.error) throw result.error;
  if (result.status !== 0 && result.status !== 10 && result.status !== 11) throw new Error(`Shell-open patch failed for ${path.basename(filePath)} (exit code ${result.status})`);
  return result.status === 11 ? null : result.status === 10;
}

async function main() {
  logger.info('Patching Linux folder opening...');
  if (!fs.existsSync(MAIN_DIR)) {
    throw new Error('Main bundle directory not found.');
  }

  const bundleNames = fs.readdirSync(MAIN_DIR).filter((name) => name.endsWith('.js'));
  let checkedCount = 0;
  let updatedCount = 0;
  for (const name of bundleNames) {
    const filePath = path.join(MAIN_DIR, name);
    const changed = patchFileIsolated(filePath);
    if (changed === null) continue;
    checkedCount += 1;
    if (changed) {
      updatedCount += 1;
      logger.dim(`Patched Linux folder opening in ${path.relative(APP_DIR, filePath)}`);
    }
  }

  if (!checkedCount) throw new Error('No shell bundles found.');
  logger.success(
    `Linux folder-opening patch applied (${checkedCount} checked, ${updatedCount} updated)`
  );
}

if (require.main === module) {
  main().catch((error) => {
    logger.error('Linux folder-opening patch failed:', error.message);
    process.exit(1);
  });
}

module.exports = {
  main,
  patchShellOpenLinux,
  patchFile
};
