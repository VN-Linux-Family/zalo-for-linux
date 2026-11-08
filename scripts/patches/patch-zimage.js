const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const ZIMAGE_DIR = path.join(NATIVELIBS_DIR, 'zimage');

async function main() {
  logger.info('Installing zimage native addon...');

  const fallbackNode = path.join(ZIMAGE_DIR, 'target', 'release', 'zimage.node');
  const builtNode = resolvePrebuiltBinary('zimage.node', fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for zimage at: ${builtNode}. Please run "npm run build:native" first.`);
  }

  const destDir = path.join(APP_NATIVE_LIBS, 'zimage', 'linux_x64');
  fs.ensureDirSync(destDir);

  fs.copyFileSync(builtNode, path.join(destDir, 'zimage.node'));

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