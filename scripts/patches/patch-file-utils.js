const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_DIR, APP_NATIVE_LIBS, NATIVELIBS_DIR, BUILDER_RUST_SCRIPT: BUILDER_SCRIPT } = require('../utils/paths');
const FILE_UTILS_DIR = path.join(NATIVELIBS_DIR, 'file-utils');

async function main() {
  logger.info('Building file-utils from source...');

  if (!fs.existsSync(path.join(FILE_UTILS_DIR, 'Cargo.toml'))) {
    logger.warn('file-utils not found, skipping');
    return;
  }

  const releaseDir = path.join(FILE_UTILS_DIR, 'target', 'release');
  const builtNode = path.join(releaseDir, 'file-utils.node');
  const alreadyBuilt = fs.existsSync(builtNode);

  if (!alreadyBuilt || process.env.FORCE_BUILD === 'true') {
    try {
      execSync(`node "${BUILDER_SCRIPT}" "${FILE_UTILS_DIR}"`, {
        cwd: ROOT,
        stdio: 'pipe'
      });
    } catch (error) {
      logger.error('Failed to build file-utils', error.message);
      if (error.stdout) logger.dim(error.stdout.toString());
      throw new Error(`Failed to build file-utils`);
    }
  } else {
    logger.dim('file-utils already built, skipping compilation');
  }

  // releaseDir already defined above
  const nodeFiles = fs.readdirSync(releaseDir).filter(f => f.endsWith('.node'));

  const destDir = path.join(APP_NATIVE_LIBS, 'file-utils', 'linux');
  fs.ensureDirSync(destDir);

  for (const file of nodeFiles) {
    fs.copyFileSync(
      path.join(releaseDir, file),
      path.join(destDir, file)
    );
  }

  const indexJsPath = path.join(APP_NATIVE_LIBS, 'file-utils', 'index.js');
  if (fs.existsSync(indexJsPath)) {
    let content = fs.readFileSync(indexJsPath, 'utf8');

    if (!content.includes("process.platform === 'linux'")) {
      content = content.replace(
        `} else {\n    return {error: 'not support'};\n  }`,
        `} else if (process.platform === 'linux'){\n    return require('./linux/file-utils.node');\n  } else {\n    return {error: 'not support'};\n  }`
      );
      fs.writeFileSync(indexJsPath, content, 'utf8');
      logger.dim('Patched index.js for Linux support');
    }
  }

  logger.success('file-utils built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };
