'use strict';

const fs = require('fs');
const path = require('path');
const { fileURLToPath } = require('url');
const { createStore, sessionKey } = require('./store');

function resolvePagePath(filePath) {
  const resolved = path.resolve(filePath);
  try {
    return fs.realpathSync(resolved);
  } catch (_) {
    return resolved;
  }
}

function register({ app, ipcMain, appDir }) {
  const store = createStore(app.getPath('userData'));
  const pages = new Set(
    ['index.html', 'login.html'].map(name => resolvePagePath(path.join(appDir, 'pc-dist', name)))
  );
  const statePages = new Set([
    ...pages,
    resolvePagePath(path.join(appDir, 'pc-dist', 'shared-worker.html'))
  ]);
  const localeMessages = JSON.stringify(require('./locale.json').messages);
  const rendererSource = "'use strict';\nwindow.__zaloLinuxOfflineMessages = " + localeMessages + ';\n' +
    fs.readFileSync(path.join(__dirname, 'renderer.js'), 'utf8').replace(/^'use strict';\s*/, '');
  let generation = 0;
  let offlineActive = false;

  function assertSender(event, allowedPages = pages) {
    const frame = event && event.senderFrame;
    if (!frame || frame !== event.sender.mainFrame) {
      console.warn('[offline] Rejected IPC request from a non-main frame');
      throw new Error('Invalid offline session sender');
    }

    let page;
    try {
      const url = new URL(frame.url);
      if (url.protocol !== 'file:') throw new Error('Invalid protocol');
      page = resolvePagePath(fileURLToPath(url));
    } catch (_) {
      console.warn('[offline] Rejected IPC request from an invalid page URL');
      throw new Error('Invalid offline session page');
    }

    if (!allowedPages.has(page)) {
      console.warn('[offline] Rejected IPC request from an untrusted page');
      throw new Error('Invalid offline session page');
    }
  }

  function getRenderer(event) {
    try {
      assertSender(event);
      return rendererSource;
    } catch (error) {
      console.warn('[offline] Renderer source request rejected:', error.message);
      return '';
    }
  }

  async function getSessionKey(contents) {
    return sessionKey(await contents.session.cookies.get({ name: 'zpw_sek' }));
  }

  async function refresh(contents) {
    let started = false;
    const allowUnload = event => event.preventDefault();
    contents.on('will-prevent-unload', allowUnload);
    try {
      await new Promise((resolve, reject) => {
        const timer = setTimeout(() => {
          cleanup();
          reject(new Error('Reload timed out'));
        }, 20000);

        function cleanup() {
          clearTimeout(timer);
          contents.removeListener('did-finish-load', done);
          contents.removeListener('did-fail-load', failed);
          contents.removeListener('did-start-navigation', navigation);
        }

        function done() {
          cleanup();
          resolve();
        }

        function failed(_event, code, message, _url, isMainFrame) {
          if (!isMainFrame) return;
          cleanup();
          reject(new Error(message || 'Reload failed: ' + code));
        }

        function navigation(_event, _url, _isInPlace, isMainFrame) {
          if (isMainFrame) started = true;
        }

        contents.once('did-finish-load', done);
        contents.on('did-fail-load', failed);
        contents.on('did-start-navigation', navigation);
        try {
          contents.reloadIgnoringCache();
        } catch (error) {
          cleanup();
          reject(error);
        }
      });
      return started;
    } finally {
      contents.removeListener('will-prevent-unload', allowUnload);
    }
  }

  async function handleRequest(event, action, payload) {
    assertSender(event);

    switch (action) {
      case 'read': {
        const current = generation;
        const key = await getSessionKey(event.sender);
        return current === generation ? store.read(key) : null;
      }

      case 'write': {
        const current = generation;
        const key = await getSessionKey(event.sender);
        return current === generation && store.write(key, payload);
      }

      case 'clear':
        generation++;
        store.clear();
        console.info('[offline] Cached session data cleared');
        return true;

      case 'refresh':
        return refresh(event.sender);

      default:
        console.warn('[offline] Rejected unknown IPC action:', action);
        throw new Error('Unsupported offline action');
    }
  }

  ipcMain.on('linux-offline:active', (event, active) => {
    try {
      assertSender(event);
      offlineActive = active === true;
    } catch (error) {
      console.warn('[offline] Rejected offline-state report:', error.message);
    }
  });

  ipcMain.on('linux-offline:is-active', event => {
    try {
      assertSender(event, statePages);
      event.returnValue = offlineActive;
    } catch (error) {
      console.warn('[offline] Rejected shared-worker state request:', error.message);
      event.returnValue = false;
    }
  });

  ipcMain.on('linux-offline:renderer', event => {
    event.returnValue = getRenderer(event);
  });

  ipcMain.handle('linux-offline:request', async (event, action, payload) => {
    try {
      return await handleRequest(event, action, payload);
    } catch (error) {
      console.error('[offline] ' + String(action) + ' request failed:', error);
      throw error;
    }
  });

  console.info('[offline] Plugin registered; IPC bridge is ready');
}

module.exports = { register };
