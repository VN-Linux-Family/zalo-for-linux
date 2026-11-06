const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { ROOT, APP_DIR, APP_NATIVE_LIBS, NATIVELIBS_DIR, BUILDER_RUST_SCRIPT: BUILDER_SCRIPT } = require('../utils/paths');
const OS_SYS_INFO_DIR = path.join(NATIVELIBS_DIR, 'os-sys-info');
const NODE_BINARY = 'os_sys_info.node';
const NODE_BINARY_SRC = 'os-sys-info.node';

async function main() {
  logger.info('Building os-sys-info from source...');

  if (!fs.existsSync(path.join(OS_SYS_INFO_DIR, 'Cargo.toml'))) {
    logger.warn('os-sys-info not found, skipping');
    return;
  }

  const releaseDir = path.join(OS_SYS_INFO_DIR, 'target', 'release');
  const builtNode = path.join(releaseDir, NODE_BINARY_SRC);
  const alreadyBuilt = fs.existsSync(builtNode);

  if (!alreadyBuilt || process.env.FORCE_BUILD === 'true') {
    try {
      execSync(`node "${BUILDER_SCRIPT}" "${OS_SYS_INFO_DIR}"`, {
        cwd: ROOT,
        stdio: 'pipe'
      });
    } catch (error) {
      logger.error('Failed to build os-sys-info', error.message);
      if (error.stdout) logger.dim(error.stdout.toString());
      throw new Error('Failed to build os-sys-info');
    }
  } else {
    logger.dim('os-sys-info already built, skipping compilation');
  }

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Built native module not found: ${builtNode}`);
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

  logger.success('os-sys-info built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };
