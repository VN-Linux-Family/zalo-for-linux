const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_NATIVE_LIBS, NATIVELIBS_DIR, resolvePrebuiltBinary } = require('../utils/paths');
const MP4THUMB_DIR = path.join(NATIVELIBS_DIR, 'mp4thumb');

async function main() {
  logger.info('Installing mp4thumb native addon...');

  const fallbackNode = path.join(MP4THUMB_DIR, 'target', 'release', 'mp4thumb.node');
  const builtNode = resolvePrebuiltBinary('mp4thumb.node', fallbackNode);

  if (!fs.existsSync(builtNode)) {
    throw new Error(`Missing pre-built binary for mp4thumb at: ${builtNode}. Please run "npm run build:native" first.`);
  }

  const destDir = path.join(APP_NATIVE_LIBS, 'mp4thumb', 'linux');
  fs.ensureDirSync(destDir);

  fs.copyFileSync(builtNode, path.join(destDir, 'mp4thumb.node'));

  const indexJsPath = path.join(APP_NATIVE_LIBS, 'mp4thumb', 'index.js');
  if (fs.existsSync(indexJsPath)) {
    let content = fs.readFileSync(indexJsPath, 'utf8');

    if (!content.includes("process.platform === 'linux'")) {
      content = content.replace(
        `else {\n            if(process.arch === 'arm64'){`,
        `else if(process.platform === 'linux') {\n            thumbModule = require(\`./linux/mp4thumb.node\`);\n        }\n        else {\n            if(process.arch === 'arm64'){`
      );
      fs.writeFileSync(indexJsPath, content, 'utf8');
      logger.dim('Patched index.js for Linux support');
    }
  }

  logger.success('mp4thumb built and installed');
}

if (require.main === module) {
  main();
}

module.exports = { main };