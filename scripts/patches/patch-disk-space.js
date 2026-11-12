const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');

const { APP_PC_DIST: PC_DIST_DIR } = require('../utils/paths');

function patchDiskSpace(content) {
  if (!content.includes('analyzeMainDisk(){')) return content;

  const start = content.indexOf('"7r+T":function');
  const method = content.indexOf('analyzeMainDisk(){', start);
  if (start === -1 || method === -1) {
    throw new Error('Disk checker module not found.');
  }
  const imports = content.slice(start, method);
  const pathModule = imports.match(/([A-Za-z_$][\w$]*)=[A-Za-z_$][\w$]*\("IpzU"\)/);
  if (!pathModule) throw new Error('Disk checker path module import not found.');

  // Measure Zalo's data filesystem; immutable / can correctly report zero free bytes.
  const replacement = `analyzeMainDisk(){let e=Object(${pathModule[1]}.getZaloDirSync)();`;
  const remainder = content.slice(method);
  if (remainder.startsWith(replacement)) return content;
  const rootPath = /^analyzeMainDisk\(\)\{let e="";e="\/";/;
  if (!rootPath.test(remainder)) throw new Error('Disk checker root path anchor not found.');
  return content.slice(0, method) + remainder.replace(rootPath, replacement);
}

function findBundles(directory) {
  return fs.readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const file = path.join(directory, entry.name);
    return entry.isDirectory() ? findBundles(file) :
      entry.isFile() && entry.name.endsWith('.js') ? [file] : [];
  });
}

async function main() {
  logger.info('Patching disk space checks to use the Zalo data directory...');
  const files = findBundles(PC_DIST_DIR);
  let checked = 0;
  let updated = 0;

  for (const file of files) {
    const original = fs.readFileSync(file, 'utf8');
    if (!original.includes('analyzeMainDisk(){')) continue;
    patchDiskSpace(original);
    checked += 1;
  }
  if (!checked) throw new Error('No disk checker bundles found.');

  for (const file of files) {
    const original = fs.readFileSync(file, 'utf8');
    if (!original.includes('analyzeMainDisk(){')) continue;
    const patched = patchDiskSpace(original);
    if (patched === original) continue;
    fs.writeFileSync(file, patched);
    updated += 1;
    logger.dim(`Patched ${path.relative(PC_DIST_DIR, file)}`);
  }
  logger.success(`Disk space patch applied (${checked} checked, ${updated} updated)`);
}

if (require.main === module) {
  main().catch((error) => {
    logger.error('Disk space patch failed:', error.message);
    process.exitCode = 1;
  });
}

module.exports = { main, patchDiskSpace };
