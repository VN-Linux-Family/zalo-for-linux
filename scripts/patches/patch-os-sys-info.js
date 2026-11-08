const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_MAIN_DIST, APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const OS_SYS_INFO_DIR = path.join(NATIVELIBS_DIR, 'os-sys-info');
const NODE_BINARY = 'os_sys_info.node';
const NODE_BINARY_SRC = 'os-sys-info.node';

async function main() {
  logger.info('Installing os-sys-info native addon...');

  const fallbackNode = path.join(OS_SYS_INFO_DIR, 'target', 'release', NODE_BINARY_SRC);
  const builtNode = resolvePrebuiltBinary(NODE_BINARY_SRC, fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for os-sys-info at: ${builtNode}. Please run "npm run build:native" first.`);
  }

  const destDir = path.join(APP_NATIVE_LIBS, 'os-sys-info', 'linux');
  fs.ensureDirSync(destDir);
  fs.copyFileSync(builtNode, path.join(destDir, NODE_BINARY));

  const indexJsPath = path.join(APP_NATIVE_LIBS, 'os-sys-info', 'index.js');
  if (fs.existsSync(indexJsPath)) {
    let content = fs.readFileSync(indexJsPath, 'utf8');
    const linuxLoader = `    if (process.platform === 'linux') {
        if (process.arch === 'x64' || process.arch === 'arm64') {
            return require('./linux/${NODE_BINARY}');
        }
        return null;
    }
`;

    if (!content.includes("process.platform === 'linux'")) {
      const marker = '    return null;\n}\n\nlet native';
      if (!content.includes(marker)) {
        throw new Error('Could not find loadNative() insertion point in os-sys-info/index.js');
      }
      content = content.replace(marker, `${linuxLoader}    return null;\n}\n\nlet native`);
      fs.writeFileSync(indexJsPath, content, 'utf8');
      logger.dim('Patched os-sys-info/index.js for Linux support');
    }
  } else {
    logger.warn('os-sys-info/index.js not found, skipped Linux loader patch');
  }

  const mainJsPath = path.join(APP_MAIN_DIST, 'main.js');
  if (fs.existsSync(mainJsPath)) {
    let mainContent = fs.readFileSync(mainJsPath, 'utf8');
    const targetPattern = '"win32"!==process.platform&&"darwin"!==process.platform';
    const replacement = '"win32"!==process.platform&&"darwin"!==process.platform&&"linux"!==process.platform';
    if (mainContent.includes(targetPattern)) {
      mainContent = mainContent.replace(targetPattern, replacement);
      fs.writeFileSync(mainJsPath, mainContent, 'utf8');
      logger.dim('Patched main-dist/main.js to enable osSysInfo on Linux');
    }
  }

  logger.success('os-sys-info built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };
