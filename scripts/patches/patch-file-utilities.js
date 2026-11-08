const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const FILE_UTILITIES_DIR = path.join(NATIVELIBS_DIR, 'file-utilities');

async function main() {
  logger.info('Installing file-utilities native addon...');

  const fallbackNode = path.join(FILE_UTILITIES_DIR, 'target', 'release', 'file-utilities.node');
  const builtNode = resolvePrebuiltBinary('file-utilities.node', fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for file-utilities at: ${builtNode}. Please run "npm run build:native" first.`);
  }

  const destDir = path.join(APP_NATIVE_LIBS, 'file-utilities', 'linux');
  fs.ensureDirSync(destDir);

  fs.copyFileSync(builtNode, path.join(destDir, 'file-utilities.node'));

  const indexJsPath = path.join(APP_NATIVE_LIBS, 'file-utilities', 'index.js');
  if (fs.existsSync(indexJsPath)) {
    let content = fs.readFileSync(indexJsPath, 'utf8');

    if (!content.includes("case 'linux':")) {
      content = content.replace(
        `default:\n      throw new Error(\`Unsupported OS: \${platform}, architecture: \${arch}\`)\n  }\n}`,
        `case 'linux':\n      return join(__dirname, 'linux', 'file-utilities.node')\n    default:\n      throw new Error(\`Unsupported OS: \${platform}, architecture: \${arch}\`)\n  }\n}`
      );
      fs.writeFileSync(indexJsPath, content, 'utf8');
      logger.dim('Patched index.js for Linux support');
    }
  }

  logger.success('file-utilities built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };
