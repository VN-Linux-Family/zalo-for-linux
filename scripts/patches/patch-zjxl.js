const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const ZJXL_DIR = path.join(NATIVELIBS_DIR, 'zjxl');

async function main() {
  logger.info('Installing zjxl native addon...');

  const fallbackNode = path.join(ZJXL_DIR, 'target', 'release', 'zjxl.node');
  const builtNode = resolvePrebuiltBinary('zjxl.node', fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for zjxl at: ${builtNode}. Please run "npm run build:native" first.`);
  }

  const destDir = path.join(APP_NATIVE_LIBS, 'zjxl', 'build', 'linux');
  fs.ensureDirSync(destDir);

  fs.copyFileSync(builtNode, path.join(destDir, 'zjxl.node'));

  const indexJsPath = path.join(APP_NATIVE_LIBS, 'zjxl', 'index.js');
  if (fs.existsSync(indexJsPath)) {
    let content = fs.readFileSync(indexJsPath, 'utf8');

    if (!content.includes("process.platform === 'linux'")) {
      content = content.replace(
        `} else {\n    return { error: 'not support' };\n  }`,
        `} else if (process.platform === 'linux') {\n    nodeAddon = require('./build/linux/zjxl.node');\n  } else {\n    return { error: 'not support' };\n  }`
      );
      fs.writeFileSync(indexJsPath, content, 'utf8');
      logger.dim('Patched index.js for Linux support');
    }
  }

  logger.success('zjxl built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };
