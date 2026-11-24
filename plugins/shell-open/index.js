/**
 * plugins/shell-open/index.js
 *
 * Non-blocking `shell.openPath()` for Linux.
 *
 * Electron's platform_util::OpenPath() launches `xdg-open` with
 * wait_for_exit = true (shell/common/platform_util_linux.cc: OpenPath ->
 * XDGUtil -> process.WaitForExit()) on the browser UI thread. While the handler
 * stays open - ONLYOFFICE, LibreOffice, an image viewer - the whole Zalo browser
 * process is stuck: the window stops responding, and everything served from the
 * same thread (tray, MPRIS, notification replies) blocks with it. A main thread
 * has been observed in wait4() for hours on an open .docx.
 *
 * Replace it with the equivalent detached spawn: the real `xdg-open` still
 * resolves the handler from mimeapps.list / the desktop file, but the promise
 * settles when the process is exec'd instead of when the handler exits.
 *
 * Flatpak builds keep Electron's implementation: it goes through the OpenURI
 * portal, which is already asynchronous.
 */

'use strict';

const { spawn } = require('child_process');

const DETACHED_MARKER = Symbol.for('zalo-for-linux.detached-open-path');

function errorMessage(error) {
  if (error && error.message) return error.message;
  return String(error);
}

/**
 * Builds a `shell.openPath` replacement bound to the given spawn function.
 *
 * Electron resolves with `''` on success and with the error text on failure; it
 * never rejects. We resolve on the child's 'spawn'/'error' events, so a missing
 * `xdg-open` is still reported, but the handler's own exit status is not - by
 * design: waiting for it is the bug this replaces.
 *
 * @param {Function} spawnFn child_process.spawn-compatible function
 * @returns {(targetPath: string) => Promise<string>}
 */
function createOpenPath(spawnFn = spawn) {
  return function openPathDetached(targetPath) {
    return new Promise((resolve) => {
      let settled = false;
      const settle = (message) => {
        if (settled) return;
        settled = true;
        resolve(message);
      };

      let child;
      try {
        child = spawnFn('xdg-open', [String(targetPath)], {
          detached: true,
          stdio: 'ignore',
        });
      } catch (error) {
        settle(errorMessage(error));
        return;
      }

      if (!child || typeof child.once !== 'function') {
        settle('');
        return;
      }

      child.once('error', (error) => settle(errorMessage(error)));
      child.once('spawn', () => settle(''));
      if (typeof child.unref === 'function') child.unref();
    });
  };
}

/**
 * Installs the replacement on an Electron `shell` module. Safe to call twice.
 *
 * @param {object} shell Electron's shell module (or a stand-in in tests)
 * @param {object} [options]
 * @param {Function} [options.spawn] spawn implementation, for tests
 * @returns {boolean} true when this call installed the replacement
 */
function install(shell, options = {}) {
  if (!shell || typeof shell.openPath !== 'function') return false;
  if (shell.openPath[DETACHED_MARKER]) return false;

  const openPath = createOpenPath(options.spawn);
  openPath[DETACHED_MARKER] = true;
  shell.openPath = openPath;
  return true;
}

module.exports = { createOpenPath, install, DETACHED_MARKER };
