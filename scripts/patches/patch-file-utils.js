const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const FILE_UTILS_DIR = path.join(NATIVELIBS_DIR, 'file-utils');

async function main() {
  logger.info('Installing file-utils native addon...');

  const fallbackNode = path.join(FILE_UTILS_DIR, 'target', 'release', 'file-utils.node');
  const builtNode = resolvePrebuiltBinary('file-utils.node', fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for file-utils at: ${builtNode}. Please run "npm run build:native" first.`);
  }

  const destDir = path.join(APP_NATIVE_LIBS, 'file-utils', 'linux');
  fs.ensureDirSync(destDir);

  fs.copyFileSync(builtNode, path.join(destDir, 'file-utils.node'));

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
