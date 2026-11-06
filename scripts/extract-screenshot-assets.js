/**
 * extract-screenshot-assets.js
 *
 * Extracts Zalo's screen capture icon font from app/pc-dist/fonts/
 * into plugins/screenshot/ui/fonts/zalo-pc-screencap.ttf at build/prepare time.
 * Nothing is committed to source control: the proprietary font is extracted
 * from the app bundle.
 */

const fs = require('fs-extra');
const path = require('path');
const logger = require('./utils/logger');

const APP_DIR = path.join(__dirname, '..', 'app');
const OUT_DIR = path.join(__dirname, '..', 'plugins', 'screenshot', 'ui', 'fonts');

function findFont(dir) {
  if (!fs.existsSync(dir)) return null;
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const fullPath = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      const res = findFont(fullPath);
      if (res) return res;
    } else if (/^zalo-pc-screencap(\.[0-9a-f]+)?\.ttf$/.test(entry.name)) {
      return fullPath;
    }
  }
  return null;
}

async function main(options = {}) {
  const appDir = (options && options.appDir) || APP_DIR;
  const outDir = (options && options.outDir) || OUT_DIR;

  const pcFontsDir = path.join(appDir, 'pc-dist', 'fonts');
  const fontFile = findFont(pcFontsDir) || findFont(appDir);

  if (!fontFile) {
    throw new Error('Screenshot font zalo-pc-screencap.*.ttf not found in app directory');
  }

  fs.ensureDirSync(outDir);
  const targetFile = path.join(outDir, 'zalo-pc-screencap.ttf');
  fs.copySync(fontFile, targetFile);

  logger.success(`Screenshot font extracted: ${path.basename(fontFile)} -> plugins/screenshot/ui/fonts/zalo-pc-screencap.ttf`);
}

module.exports = { main, findFont };

if (require.main === module) {
  main().catch((e) => {
    logger.error(e.message);
    process.exit(1);
  });
}
