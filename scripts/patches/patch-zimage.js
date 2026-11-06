const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_DIR, APP_NATIVE_LIBS, NATIVELIBS_DIR, BUILDER_RUST_SCRIPT: BUILDER_SCRIPT } = require('../utils/paths');
const ZIMAGE_DIR = path.join(NATIVELIBS_DIR, 'zimage');

async function main() {
  logger.info('Building zimage from source...');

  if (!fs.existsSync(path.join(ZIMAGE_DIR, 'Cargo.toml'))) {
    logger.warn('zimage not found, skipping');
    return;
  }

  const releaseDir = path.join(ZIMAGE_DIR, 'target', 'release');
  const builtNode = path.join(releaseDir, 'zimage.node');
  const alreadyBuilt = fs.existsSync(builtNode);

  if (!alreadyBuilt || process.env.FORCE_BUILD === 'true') {
    try {
      execSync(`node "${BUILDER_SCRIPT}" "${ZIMAGE_DIR}"`, {
        cwd: ROOT,
        stdio: 'pipe'
      });
    } catch (error) {
      logger.error('Failed to build zimage', error.message);
      if (error.stdout) logger.dim(error.stdout.toString());
      throw new Error(`Failed to build zimage`);
    }
  } else {
    logger.dim('zimage already built, skipping compilation');
  }

  // releaseDir already defined above
  const nodeFiles = fs.readdirSync(releaseDir).filter(f => f.endsWith('.node'));

  const destDir = path.join(APP_NATIVE_LIBS, 'zimage', 'linux_x64');
  fs.ensureDirSync(destDir);

  for (const file of nodeFiles) {
    fs.copyFileSync(
      path.join(releaseDir, file),
      path.join(destDir, file)
    );
  }

  const indexJsPath = path.join(APP_NATIVE_LIBS, 'zimage', 'index.js');
  if (fs.existsSync(indexJsPath)) {
    let content = fs.readFileSync(indexJsPath, 'utf8');

    if (!content.includes("process.platform === 'linux'")) {
      content = content.replace(
        `\t\t} else {\n\t\t\tos = 'darwin_x64';\n\t\t}\n\t}\n}`,
        `\t\t} else {\n\t\t\tos = 'darwin_x64';\n\t\t}\n\t} else if (process.platform === 'linux') {\n\t\tos = 'linux_x64';\n\t}\n}`
      );
      fs.writeFileSync(indexJsPath, content, 'utf8');
      logger.dim('Patched index.js for Linux support');
    }
  }

  logger.success('zimage built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };