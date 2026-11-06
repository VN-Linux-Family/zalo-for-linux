const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_DIR, APP_PC_DIST } = require('../utils/paths');

function patch(source) {
  // OCR button visibility (may reside in a split module outside EzwU in newer bundles)
  const visible = 'show:!0,disabled:"supported"!==$1.support||"ready"!==$1.scanStatus';
  if (source.includes('show:"unsupported"!==')) {
    const visibilityRegex = /show:"unsupported"!==([a-zA-Z0-9_$]+)\.support,disabled:"supported"!==\1\.support\|\|"ready"!==\1\.scanStatus/g;
    if (!visibilityRegex.test(source)) {
      throw new Error('OCR button pattern changed');
    }
    source = source.replace(visibilityRegex, visible);
  } else if (!source.includes('show:!0,disabled:"supported"!==')) {
    const alreadyVisible = /show:!0,disabled:"supported"!==([a-zA-Z0-9_$]+)\.support\|\|"ready"!==\1\.scanStatus/;
    if (!alreadyVisible.test(source)) {
      throw new Error('OCR button pattern changed');
    }
  }

  // OCR feature flag pattern
  if (source.includes('.key("is_enabled").value')) {
    const config = /(\w+=\(\)=>)[a-zA-Z0-9_$.]+\.key\("is_enabled"\)\.value(?=;function)/;
    if (!config.test(source)) {
      throw new Error('OCR feature flag pattern changed');
    }
    source = source.replace(config, '$1!0');
  } else if (!source.includes('()=>!0;function')) {
    const enabled = /\w+=\(\)=>!0;function/;
    if (!enabled.test(source)) {
      throw new Error('OCR feature flag pattern changed');
    }
  }

  // A drag starting in OCR can generate its click on the viewer background.
  // Catch that click before React's background handler closes the window.
  const gestureMarker = 'let ocrGestureStarted=!1;';
  if (!source.includes(gestureMarker)) {
    const reset = /const u=\(\)=>\{const e=(\w+)\.current,t=c\.getSelection\(\);/g;
    const down = /([a-zA-Z0-9_$]+)=e=>\{var [^;]+;const (\w+)=(\w+)\.current;/g;
    const resets = [...source.matchAll(reset)];
    const downs = [...source.matchAll(down)];
    if (resets.length !== 1 || downs.length !== 1 || resets[0][1] !== downs[0][3]) {
      throw new Error('OCR selection gesture pattern changed');
    }
    const handler = downs[0][1];
    const target = downs[0][2];
    const textLayer = resets[0][1];
    const replacements = [
      [resets[0][0],
      gestureMarker + `const ocrGestureClick=e=>{const t=ocrGestureStarted;ocrGestureStarted=!1;t&&${textLayer}.current&&!${textLayer}.current.contains(e.target)&&(e.preventDefault(),e.stopPropagation())},` + resets[0][0].slice('const '.length)],
      [downs[0][0],
      downs[0][0] + `ocrGestureStarted=0===e.button&&Boolean(${target}&&${target}.contains(e.target));`],
      [`return c.addEventListener("mousedown",${handler},!0),`,
      `return c.addEventListener("click",ocrGestureClick,!0),c.addEventListener("mousedown",${handler},!0),`],
      [`c.removeEventListener("mousedown",${handler},!0),`,
      `c.removeEventListener("click",ocrGestureClick,!0),c.removeEventListener("mousedown",${handler},!0),`],
    ];
    for (const [original, replacement] of replacements) {
      if (source.split(original).length !== 2) {
        throw new Error(`OCR selection pattern changed: ${original}`);
      }
      source = source.replace(original, replacement);
    }
  }

  // Releasing a drag outside OCR must not collapse the last valid selection.
  if (source.includes('if(t.dragging&&')) {
    const dragRelease = /if\(t\.dragging&&(\w+)&&a\.contains\(\1\.node\)\)\{([^{}]+)\}else t\.selectAnchorOnMouseUp\?/g;
    const releases = [...source.matchAll(dragRelease)];
    if (releases.length === 1) {
      source = source.replace(dragRelease,
        'if(t.dragging){if($1&&a.contains($1.node)){$2}}else t.selectAnchorOnMouseUp?');
    } else {
      throw new Error('OCR selection mouseup pattern changed');
    }
  } else if (!source.includes('if(t.dragging){if(')) {
    const preservedRelease = /if\(t\.dragging\)\{if\((\w+)&&a\.contains\(\1\.node\)\)\{([^{}]+)\}\}else t\.selectAnchorOnMouseUp\?/g;
    if ([...source.matchAll(preservedRelease)].length !== 1) {
      throw new Error('OCR selection mouseup pattern changed');
    }
  }

  return source;
}

function main() {
  logger.info('Patching OCR UI...');
  const root = APP_PC_DIST;
  const changes = [];
  let found = 0;
  const files = [
    ...fs.readdirSync(root)
      .filter(name => /^(compact-app-pc|search-worker|sync-v2-sub-worker)\.[\da-f]+\.js$/.test(name)),
    ...fs.readdirSync(path.join(root, 'lazy'))
      .filter(name => /^default-login-main-startup-shared-worker-znotification\.[\da-f]+\.js$/.test(name))
      .map(name => path.join('lazy', name)),
  ];
  for (const entry of files) {
    const file = path.join(root, entry);
    try {
      const source = fs.readFileSync(file, 'utf8');
      if (!source.includes('ocr-text-overlay')) {
        logger.warn(`OCR UI: skipped ${path.relative(APP_DIR, file)}: OCR module not found`);
        continue;
      }
      const updated = patch(source);
      found++;
      if (updated !== source) changes.push([file, updated]);
    } catch (error) {
      logger.warn(`OCR UI: failed to patch ${path.relative(APP_DIR, file)}: ${error.message}`);
      throw error;
    }
  }
  if (!found) throw new Error('No OCR bundles found in pc-dist');
  // Validate every bundle before writing any changes.
  for (const [file, updated] of changes) {
    try {
      fs.writeFileSync(file, updated);
      logger.dim(`  Patched ${path.relative(APP_DIR, file)}`);
    } catch (error) {
      logger.warn(`OCR UI: failed to write ${path.relative(APP_DIR, file)}: ${error.message}`);
      throw error;
    }
  }
  logger.success(`OCR UI: ${changes.length} patched, ${found - changes.length} already patched.`);
}

if (require.main === module) {
  try {
    main();
  } catch (error) {
    logger.error('Failed to patch OCR UI', error.message);
    process.exit(1);
  }
}

module.exports = { main, patch };
