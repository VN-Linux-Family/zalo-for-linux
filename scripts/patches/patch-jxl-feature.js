/**
 * patch-jxl-feature.js
 *
 * Zalo receives photos as JPEG XL and lets Chromium decode them in <img>.
 * 26.11.10 turned that on with `--enable-features=JXL`, the flag name of
 * Chromium 91-109. 26.11.20 ships Electron 43 and renamed it to
 * `JXLImageFormat`, which Chromium 108 (our Electron 22) does not know, so
 * every newly received photo stayed a blurred preview with a download button.
 *
 * Enable both names: JXL for the Electron we run today, JXLImageFormat for
 * when we move to a newer one.
 */

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { APP_MAIN_DIST } = require('../utils/paths');

// kr.commandLine.appendSwitch("enable-features","JXLImageFormat")
const SWITCH = /(\.commandLine\.appendSwitch\("enable-features",")([\w,]*)("\))/g;
const NAMES = ['JXL', 'JXLImageFormat'];

// Returns { content, status }: 'patched', 'already' or 'missing'.
function patchContent(content) {
  let found = false;
  let changed = false;
  const out = content.replace(SWITCH, (all, head, value, tail) => {
    const features = value.split(',').filter(Boolean);
    if (!features.some((f) => NAMES.includes(f))) return all;
    found = true;
    for (const name of NAMES) {
      if (!features.includes(name)) {
        features.push(name);
        changed = true;
      }
    }
    return head + features.join(',') + tail;
  });
  if (!found) return { content, status: 'missing' };
  return { content: out, status: changed ? 'patched' : 'already' };
}

async function main(mainDist = APP_MAIN_DIST) {
  const file = path.join(mainDist, 'main.js');
  if (!fs.existsSync(file)) throw new Error(`main.js not found: ${file}`);
  const { content, status } = patchContent(fs.readFileSync(file, 'utf8'));
  if (status === 'missing') {
    throw new Error('JPEG XL patch not applied, Zalo no longer enables JXL in main.js');
  }
  if (status === 'patched') fs.writeFileSync(file, content, 'utf8');
  logger.success('JPEG XL decoding enabled for Electron 22 (JXL,JXLImageFormat)');
}

if (require.main === module) {
  main().catch((error) => {
    logger.error('JPEG XL patch failed:', error.message);
    process.exit(1);
  });
}

module.exports = { main, patchContent };
