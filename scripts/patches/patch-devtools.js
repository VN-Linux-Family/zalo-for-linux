/**
 * patch-devtools.js
 *
 * Zalo 26.11.20 added a `canOpenDevTools()` module that always returns false.
 * Every window is created with `webPreferences.devTools: canOpenDevTools()`,
 * and main.js closes DevTools as soon as they open unless it returns true, so
 * the Dev Mode plugin's "Toggle DevTools" (Ctrl+Shift+I) did nothing.
 *
 * Let it return true. The Dev Mode plugin still decides: it closes DevTools on
 * any window while Dev Mode is off, as it did with 26.11.10 (whose main window
 * had `devTools: true`).
 */

const fs = require('fs');
const path = require('path');
const logger = require('../utils/logger');
const { APP_MAIN_DIST } = require('../utils/paths');

const FILES = ['main.js', 'compact-app.js'];

// "Qd+r":function(e,t,n){"use strict";function r(){return!1}n.r(t),n.d(t,"canOpenDevTools",function(){return r})}
const GATE = /function ([\w$]+)\(\)\{return!1\}([\w$]+)\.r\(([\w$]+)\),\2\.d\(\3,"canOpenDevTools",(\(?function\(\)\{return \1\}\)?)/;
const GATE_DONE = /function ([\w$]+)\(\)\{return\s*!0\}([\w$]+)\.r\(([\w$]+)\),\2\.d\(\3,"canOpenDevTools",\(?function\(\)\{return \1\}/;

// Returns { content, status }: 'patched', 'already', 'absent' (older Zalo) or 'missing'.
function patchContent(content) {
  if (!content.includes('"canOpenDevTools"')) return { content, status: 'absent' };
  if (GATE_DONE.test(content)) return { content, status: 'already' };
  if (!GATE.test(content)) return { content, status: 'missing' };
  return {
    content: content.replace(GATE, 'function $1(){return!0}$2.r($3),$2.d($3,"canOpenDevTools",$4'),
    status: 'patched',
  };
}

async function main(mainDist = APP_MAIN_DIST) {
  const missing = [];
  let present = 0;
  for (const name of FILES) {
    const file = path.join(mainDist, name);
    if (!fs.existsSync(file)) continue;
    const { content, status } = patchContent(fs.readFileSync(file, 'utf8'));
    if (status === 'absent') continue;
    present++;
    if (status === 'missing') missing.push(name);
    else if (status === 'patched') {
      fs.writeFileSync(file, content, 'utf8');
      logger.dim(`Patched canOpenDevTools in ${name}`);
    }
  }
  if (missing.length) {
    throw new Error('DevTools patch not applied, canOpenDevTools changed in: ' + missing.join(', '));
  }
  logger.success(present ? 'DevTools gate patched (Dev Mode decides)' : 'No DevTools gate in this Zalo version');
}

if (require.main === module) {
  main().catch((error) => {
    logger.error('DevTools patch failed:', error.message);
    process.exit(1);
  });
}

module.exports = { main, patchContent };
