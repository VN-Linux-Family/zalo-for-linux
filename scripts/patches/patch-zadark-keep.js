const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const APP_DIR = path.join(__dirname, '..', '..', 'app');

/**
 * Keep ZaDark loaded when Zalo switches to its dark theme (fixes #66).
 *
 * Zalo's dark theme component calls a helper that removes every
 * link[href^="zadark"] and script[src^="zadark"] from the page (the selectors
 * are spelled out character by character). With the desktop in dark mode
 * (patch-auto-theme follows it), ZaDark lost its CSS and icon font right
 * after startup: its sidebar button stayed as an empty slot and the message
 * translation was gone.
 *
 * The helper is found by that character array and turned into a no-op.
 * Only applied to the ZaDark variant (from build.js integrateZaDark).
 */
const REMOVER_RE = new RegExp(
  '(\\b[\\w$]+=\\(\\)=>\\{)' +
  '(var [\\w$,]+;const [\\w$]+=\\["l","i","n","k","\\[","h","r","e","f","\\^","=",\'"\',"z","a","d","a","r","k",\'"\',"\\]"\\]\\.join\\(""\\))',
  'g'
);
const MARKER = '"z","a","d","a","r","k"';

function listJsFiles(dir) {
  let files = [];
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) files = files.concat(listJsFiles(full));
    else if (entry.name.endsWith('.js')) files.push(full);
  }
  return files;
}

async function main(appDir = APP_DIR) {
  const pcDistDir = path.join(appDir, 'pc-dist');
  if (!fs.existsSync(pcDistDir)) {
    logger.warn('pc-dist not present, skipping ZaDark keep patch');
    return;
  }

  let patched = 0;
  let unmatched = 0;
  for (const file of listJsFiles(pcDistDir)) {
    const content = fs.readFileSync(file, 'utf8');
    if (!content.includes(MARKER)) continue;
    const updated = content.replace(REMOVER_RE, '$1return;$2');
    if (updated !== content) {
      fs.writeFileSync(file, updated, 'utf8');
      patched++;
      logger.dim(`Kept ZaDark in dark theme: ${path.relative(pcDistDir, file)}`);
    } else if (!content.includes('=()=>{return;var')) {
      unmatched++;
    }
  }

  if (unmatched > 0) {
    logger.warn(`ZaDark remover found in ${unmatched} file(s) but its pattern changed; ZaDark may unload in dark theme`);
  } else if (patched > 0) {
    logger.success(`ZaDark kept in Zalo's dark theme (${patched} files)`);
  }
}

if (require.main === module) {
  main();
}

module.exports = { main };
