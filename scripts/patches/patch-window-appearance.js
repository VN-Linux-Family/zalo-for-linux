const fs = require('fs-extra');
const path = require('path');
let logger;
try {
  logger = require('../utils/logger');
} catch (_) {
  logger = {
    info: (...args) => console.log('[INFO]', ...args),
    warn: (...args) => console.warn('[WARN]', ...args),
    error: (...args) => console.error('[ERROR]', ...args),
    success: (...args) => console.log('[SUCCESS]', ...args),
    dim: (...args) => console.log(' ', ...args)
  };
}

const APP_DIR = path.join(__dirname, '..', '..', 'app');

const ROUNDED_PRELOAD_INJECTION = `
// --- Zalo Linux Window Rounded Corners & Background Transparency ---
(function() {
  if (process.platform !== "linux") return;
  const { ipcRenderer } = require("electron");

  const styleId = "zalo-linux-rounded-style";
  function injectRoundedStyle() {
    if (document.getElementById(styleId)) return;
    const style = document.createElement("style");
    style.id = styleId;
    style.textContent = \`
      html {
        background-color: transparent !important;
        background: transparent !important;
        overflow: hidden !important;
        margin: 0 !important;
        padding: 0 !important;
        width: 100% !important;
        height: 100% !important;
      }

      body {
        margin: 0 !important;
        padding: 0 !important;
        width: 100% !important;
        height: 100% !important;
        box-sizing: border-box !important;
        background-color: transparent !important;
        background: transparent !important;
        border-radius: 12px !important;
        clip-path: inset(0 round 12px) !important;
        overflow: hidden !important;
        border: 1px solid rgba(0, 0, 0, 0.14) !important;
      }

      html.dark body,
      body.dark {
        border: 1px solid rgba(255, 255, 255, 0.12) !important;
      }

      html.is-maximized body,
      body.is-maximized {
        border-radius: 0px !important;
        clip-path: none !important;
        border: none !important;
      }

      #app,
      #loading-page {
        width: 100% !important;
        height: 100% !important;
        box-sizing: border-box !important;
        background-color: var(--layer-background, #ffffff) !important;
      }

      html.dark #app,
      body.dark #app,
      html.dark #loading-page,
      body.dark #loading-page {
        background-color: var(--layer-background, #22262B) !important;
      }

      #titleBar {
        height: 38px !important;
        min-height: 38px !important;
        width: 100% !important;
        max-width: 100% !important;
        box-sizing: border-box !important;
        padding: 0 0 0 16px !important;
        display: flex !important;
        align-items: center !important;
        justify-content: space-between !important;
        -webkit-app-region: drag !important;
        position: relative !important;
        z-index: 100 !important;
      }

      body:has(#main-tab) #titleBar,
      body:has(#sidebarNav) #titleBar {
        left: 64px !important;
        width: calc(100% - 64px) !important;
        max-width: calc(100% - 64px) !important;
      }

      #titleBar .title-drag {
        height: 38px !important;
        top: 0 !important;
        left: 0 !important;
        right: 60px !important;
        width: auto !important;
        -webkit-app-region: drag !important;
      }

      #titleBar .title-name {
        line-height: 38px !important;
        height: 38px !important;
        display: flex !important;
        align-items: center !important;
        font-size: 13px !important;
        font-weight: 500 !important;
        -webkit-app-region: drag !important;
        user-select: none !important;
      }

      #container {
        height: calc(100% - 38px) !important;
        max-height: calc(100% - 38px) !important;
      }

      #sidebarNav, #main-tab {
        height: 100% !important;
      }

      .zalo-linux-tb-controls {
        display: flex !important;
        align-items: center !important;
        height: 100% !important;
        margin-left: auto !important;
        -webkit-app-region: no-drag !important;
        z-index: 1000 !important;
      }

      .zalo-linux-close-button {
        -webkit-app-region: no-drag !important;
        pointer-events: auto !important;
        cursor: pointer !important;
        width: 48px !important;
        height: 38px !important;
        display: flex !important;
        align-items: center !important;
        justify-content: center !important;
        background: transparent !important;
        border: none !important;
        outline: none !important;
        padding: 0 !important;
        margin: 0 !important;
        color: var(--text-secondary, #999999) !important;
        border-top-right-radius: 12px !important;
        transition: background-color 0.15s ease, color 0.15s ease !important;
      }

      .zalo-linux-close-button svg {
        width: 12px !important;
        height: 12px !important;
        display: block !important;
        pointer-events: none !important;
      }

      .zalo-linux-close-button:hover {
        background-color: #e81123 !important;
        color: #ffffff !important;
      }

      .zalo-linux-close-button:active {
        background-color: #c40e1d !important;
        color: #ffffff !important;
      }

      html.is-maximized .zalo-linux-close-button,
      body.is-maximized .zalo-linux-close-button {
        border-top-right-radius: 0px !important;
      }
    \`;
    (document.head || document.documentElement).appendChild(style);
  }

  function adjustTitleBarLayout() {
    const tb = document.getElementById("titleBar");
    if (!tb) return;
    const hasSideTab = document.getElementById("main-tab") || document.getElementById("sidebarNav");
    const offset = hasSideTab ? 64 : (tb.offsetLeft || 0);
    if (offset > 0) {
      tb.style.setProperty("left", offset + "px", "important");
      tb.style.setProperty("width", "calc(100% - " + offset + "px)", "important");
      tb.style.setProperty("max-width", "calc(100% - " + offset + "px)", "important");
    } else {
      tb.style.setProperty("left", "0px", "important");
      tb.style.setProperty("width", "100%", "important");
      tb.style.setProperty("max-width", "100%", "important");
    }
  }

  function ensureCloseButton() {
    const tb = document.getElementById("titleBar");
    if (!tb) return;
    adjustTitleBarLayout();
    if (document.getElementById("zalo-linux-titlebar-controls")) return;

    const controls = document.createElement("div");
    controls.id = "zalo-linux-titlebar-controls";
    controls.className = "zalo-linux-tb-controls";

    const closeBtn = document.createElement("button");
    closeBtn.id = "zalo-linux-close-btn";
    closeBtn.className = "zalo-linux-close-button";
    closeBtn.title = "Đóng";
    closeBtn.setAttribute("aria-label", "Đóng");
    closeBtn.innerHTML = '<svg width="12" height="12" viewBox="0 0 12 12" fill="none" xmlns="http://www.w3.org/2000/svg"><path d="M2 2L10 10M10 2L2 10" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>';

    closeBtn.addEventListener("click", (e) => {
      e.preventDefault();
      e.stopPropagation();
      try {
        ipcRenderer.send("zalo-window-close");
      } catch (_) {
        window.close();
      }
    });

    controls.appendChild(closeBtn);
    tb.appendChild(controls);
  }

  function handleMaximized(isMax) {
    if (isMax) {
      document.documentElement.classList.add("is-maximized");
      if (document.body) document.body.classList.add("is-maximized");
    } else {
      document.documentElement.classList.remove("is-maximized");
      if (document.body) document.body.classList.remove("is-maximized");
    }
  }

  ipcRenderer.on("zalo-window-maximized", (e, isMax) => {
    handleMaximized(isMax);
  });

  try {
    ipcRenderer.invoke("zalo-get-maximized-state").then((isMax) => {
      if (isMax) handleMaximized(true);
    }).catch(() => {});
  } catch (_) {}

  function initUI() {
    injectRoundedStyle();
    adjustTitleBarLayout();
    ensureCloseButton();
    try {
      const observer = new MutationObserver(() => {
        adjustTitleBarLayout();
        ensureCloseButton();
      });
      observer.observe(document.body || document.documentElement, { childList: true, subtree: true });
    } catch (_) {}
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", initUI);
  } else {
    initUI();
  }
})();
`;

const SUPPRESS_SET_BG_CODE = `
// --- Zalo Linux Window Background Transparency Preservation ---
try {
  const { BrowserWindow: _zBW } = require('electron');
  if (_zBW && _zBW.prototype && !_zBW.prototype.__zalo_bg_suppressed) {
    _zBW.prototype.__zalo_bg_suppressed = true;
    _zBW.prototype.setBackgroundColor = function(_color) {
      // Suppress on Linux so transparent window corners never get filled with opaque background
    };
  }
} catch (_) {}
`;

// Matches every IPC block this patch has ever appended to main.js (their
// closing `} catch (_) {}` is the only one at column 0).
const MAIN_IPC_BLOCK_RE = /\n*try \{\n  const \{ ipcMain: _zIpc, BrowserWindow: _zBW \} = require\('electron'\);[\s\S]*?\n\} catch \(_\) \{\}\n?/g;

const MAIN_IPC_BLOCK = `
try {
  const { ipcMain: _zIpc, BrowserWindow: _zBW } = require('electron');
  _zIpc.removeHandler('zalo-get-maximized-state');
  _zIpc.handle('zalo-get-maximized-state', (ev) => {
    try {
      const win = _zBW.fromWebContents(ev.sender);
      return win ? win.isMaximized() : false;
    } catch (_) { return false; }
  });
  _zIpc.removeAllListeners('zalo-window-close');
  _zIpc.on('zalo-window-close', (ev) => {
    try {
      const win = _zBW.fromWebContents(ev.sender);
      if (win && !win.isDestroyed()) win.close();
    } catch (_) {}
  });
  // Tray host probe: the main window close handler hides to tray only when
  // a StatusNotifier host exists; otherwise it quits.
  if (process.platform === 'linux') {
    const { execFile: _zExec } = require('child_process');
    const _zName = 'org.kde.StatusNotifierWatcher';
    const _zProbe = () => {
      _zExec('gdbus', ['call', '--session', '--dest', 'org.freedesktop.DBus',
        '--object-path', '/org/freedesktop/DBus',
        '--method', 'org.freedesktop.DBus.NameHasOwner', _zName], { timeout: 3000 }, (err, out) => {
        if (!err) { global.__zaloTrayHost = /true/.test(String(out)); return; }
        _zExec('dbus-send', ['--session', '--print-reply', '--dest=org.freedesktop.DBus',
          '/org/freedesktop/DBus', 'org.freedesktop.DBus.NameHasOwner', 'string:' + _zName],
          { timeout: 3000 }, (err2, out2) => {
            global.__zaloTrayHost = !err2 && /boolean true/.test(String(out2));
          });
      });
    };
    global.__zaloTrayHost = false;
    _zProbe();
    setInterval(_zProbe, 60000).unref();
  }
} catch (_) {}
`;

async function main() {
  const mainDistDir = path.join(APP_DIR, 'main-dist');
  const pcDistDir = path.join(APP_DIR, 'pc-dist');

  // 1. Patch main-dist/main.js
  const mainJsPath = path.join(mainDistDir, 'main.js');
  if (fs.existsSync(mainJsPath)) {
    let content = fs.readFileSync(mainJsPath, 'utf8');
    let changed = false;

    // Suppress setBackgroundColor globally at the top
    if (!content.includes('__zalo_bg_suppressed')) {
      content = SUPPRESS_SET_BG_CODE + '\n' + content;
      changed = true;
      logger.dim('Injected global setBackgroundColor suppression in main.js');
    }

    // Remove auto-open DevTools and console logger
    const devtoolsPattern = ';Ae.webContents.openDevTools({mode:"detach"});Ae.webContents.on("console-message",(e,l,m,ln,s)=>{try{require("fs").appendFileSync("/tmp/zalo-sync-debug.log",`[R-${l}] ${m}\\n`)}catch(_){};console.log(`[R-${l}] ${m}`)});';
    if (content.includes(devtoolsPattern)) {
      content = content.replace(devtoolsPattern, '');
      changed = true;
      logger.dim('Removed auto-open DevTools call from main.js');
    } else if (content.includes(';Ae.webContents.openDevTools({mode:"detach"});')) {
      content = content.replace(';Ae.webContents.openDevTools({mode:"detach"});', ';');
      changed = true;
      logger.dim('Removed detached DevTools call from main.js');
    }

    // Set transparent: true and transparent background on BrowserWindow options
    const bgPattern = 'backgroundColor:(null==r||null===(e=r.color)||void 0===e?void 0:e.backgroundPrimary)||"#ffffff",show:!1';
    if (content.includes(bgPattern)) {
      content = content.replace(bgPattern, 'transparent:!0,backgroundColor:"#00000000",hasShadow:!1,show:!1');
      changed = true;
      logger.dim('Configured transparent BrowserWindow options in main.js');
    }

    // Prevent k.setBackgroundColor(r) from ruining transparency
    const setBgPattern = 'return r&&k.setBackgroundColor(r),k';
    if (content.includes(setBgPattern)) {
      content = content.replace(setBgPattern, 'return k');
      changed = true;
      logger.dim('Suppressed opaque window setBackgroundColor in main.js');
    }

    // Prevent Ae.setBackgroundColor(r) and e.setBackgroundColor(i) from ruining transparency
    if (content.includes('r&&Ae.setBackgroundColor(r)')) {
      content = content.replace('r&&Ae.setBackgroundColor(r)', 'null');
      changed = true;
      logger.dim('Suppressed Ae.setBackgroundColor in main.js');
    }
    if (content.includes('i&&e.setBackgroundColor(i)')) {
      content = content.replace('i&&e.setBackgroundColor(i)', 'null');
      changed = true;
      logger.dim('Suppressed e.setBackgroundColor in main.js');
    }

    // Hook window maximize / unmaximize events to notify renderer and capture window render
    const listenerPattern = '_setupDefaultWindowListener(e){';
    if (content.includes(listenerPattern) && !content.includes('zalo-window-maximized')) {
      content = content.replace(
        listenerPattern,
        listenerPattern + 'e.on("maximize",(()=>{try{e.webContents.send("zalo-window-maximized",!0)}catch(_){}})),e.on("unmaximize",(()=>{try{e.webContents.send("zalo-window-maximized",!1)}catch(_){}})),'
      );
      changed = true;
      logger.dim('Added maximize/unmaximize listeners in main.js');
    }

    // IPC for the maximized state and the custom close button. Strip every
    // earlier injected copy first: a second ipcMain.handle() on the same
    // channel throws (eventNames() does not list invoke handlers), which
    // aborted the block before 'zalo-window-close' was ever registered.
    const withoutOldIpc = content.replace(MAIN_IPC_BLOCK_RE, '').trimEnd();
    const withIpc = withoutOldIpc + '\n' + MAIN_IPC_BLOCK;
    if (withIpc !== content) {
      content = withIpc;
      changed = true;
      logger.dim('Installed window IPC handlers (maximize state, close button, tray probe) in main.js');
    }

    // Without a tray host (stock GNOME has none) Zalo's hide-on-close leaves
    // an invisible window nobody can bring back, so quit instead.
    const hideOnCloseRe = /([$\w]+)\.appIsQuitting\|\|\(e\.preventDefault\(\),"darwin"===process\.platform&&([$\w]+)\.isFullScreen\(\)\?/;
    if (!content.includes('!global.__zaloTrayHost?')) {
      if (hideOnCloseRe.test(content)) {
        content = content.replace(hideOnCloseRe, '$1.appIsQuitting||(e.preventDefault(),"linux"===process.platform&&!global.__zaloTrayHost?$1.quit():"darwin"===process.platform&&$2.isFullScreen()?');
        changed = true;
        logger.dim('Main window close quits when no tray host is available');
      } else {
        logger.warn('Main window close handler not found; hide-on-close left unchanged');
      }
    }

    if (changed) {
      fs.writeFileSync(mainJsPath, content, 'utf8');
      logger.success('Patched main.js for Linux window appearance, close button and disabled DevTools startup');
    }
  }

  // 2. Patch main-dist/compact-app.js
  const compactJsPath = path.join(mainDistDir, 'compact-app.js');
  if (fs.existsSync(compactJsPath)) {
    let content = fs.readFileSync(compactJsPath, 'utf8');
    let changed = false;

    if (!content.includes('__zalo_bg_suppressed')) {
      content = SUPPRESS_SET_BG_CODE + '\n' + content;
      changed = true;
      logger.dim('Injected global setBackgroundColor suppression in compact-app.js');
    }

    const bgPattern = 'backgroundColor:(null==r||null===(e=r.color)||void 0===e?void 0:e.backgroundPrimary)||"#ffffff",show:!1';
    if (content.includes(bgPattern)) {
      content = content.replace(bgPattern, 'transparent:!0,backgroundColor:"#00000000",hasShadow:!1,show:!1');
      changed = true;
    }

    const setBgPattern = 'return r&&k.setBackgroundColor(r),k';
    if (content.includes(setBgPattern)) {
      content = content.replace(setBgPattern, 'return k');
      changed = true;
    }

    if (content.includes('r&&Ae.setBackgroundColor(r)')) {
      content = content.replace('r&&Ae.setBackgroundColor(r)', 'null');
      changed = true;
    }
    if (content.includes('i&&e.setBackgroundColor(i)')) {
      content = content.replace('i&&e.setBackgroundColor(i)', 'null');
      changed = true;
    }

    if (!content.includes('zalo-window-close')) {
      const ipcHandler = `
try {
  const { ipcMain: _zIpc, BrowserWindow: _zBW } = require('electron');
  _zIpc.on('zalo-window-close', (ev) => {
    try {
      const win = _zBW.fromWebContents(ev.sender);
      if (win) win.close();
    } catch (_) {}
  });
} catch (_) {}
`;
      content += '\n' + ipcHandler;
      changed = true;
    }

    if (changed) {
      fs.writeFileSync(compactJsPath, content, 'utf8');
      logger.success('Patched compact-app.js for Linux window appearance');
    }
  }

  // 3. Patch index.html & login.html to prevent body background fill
  const htmlFiles = ['index.html', 'login.html'].map(f => path.join(pcDistDir, f));
  for (const htmlPath of htmlFiles) {
    if (fs.existsSync(htmlPath)) {
      let content = fs.readFileSync(htmlPath, 'utf8');
      let changed = false;

      // Disable body.style.background assignment
      if (content.includes('document.body.style.background = bgColorDark;')) {
        content = content.replace(
          'document.body.style.background = bgColorDark;',
          '/* transparent body */'
        );
        changed = true;
      }
      if (content.includes('document.body.style.background = bgColorDark')) {
        content = content.replace(
          'document.body.style.background = bgColorDark',
          '/* transparent body */'
        );
        changed = true;
      }

      // Ensure inline style doesn't have an opaque body background and has full rounded styles
      const fullInlineStyle = '<style id="zalo-transparent-base">' +
        'html{background:transparent !important;background-color:transparent !important;overflow:hidden !important;margin:0 !important;padding:0 !important;width:100% !important;height:100% !important;}' +
        'body{margin:0 !important;padding:0 !important;width:100% !important;height:100% !important;box-sizing:border-box !important;background:transparent !important;background-color:transparent !important;border-radius:12px !important;clip-path:inset(0 round 12px) !important;overflow:hidden !important;border:1px solid rgba(0,0,0,0.14) !important;}' +
        'html.dark body,body.dark{border:1px solid rgba(255,255,255,0.12) !important;}' +
        'html.is-maximized body,body.is-maximized{border-radius:0px !important;clip-path:none !important;border:none !important;}' +
        '#app,#loading-page{width:100% !important;height:100% !important;box-sizing:border-box !important;background-color:var(--layer-background,#ffffff) !important;}' +
        'html.dark #app,body.dark #app,html.dark #loading-page,body.dark #loading-page{background-color:var(--layer-background,#22262B) !important;}' +
        '#titleBar{height:38px !important;min-height:38px !important;width:100% !important;max-width:100% !important;box-sizing:border-box !important;padding:0 0 0 16px !important;display:flex !important;align-items:center !important;justify-content:space-between !important;-webkit-app-region:drag !important;position:relative !important;z-index:100 !important;}' +
        'body:has(#main-tab) #titleBar,body:has(#sidebarNav) #titleBar{left:64px !important;width:calc(100% - 64px) !important;max-width:calc(100% - 64px) !important;}' +
        '#titleBar .title-drag{height:38px !important;top:0 !important;left:0 !important;right:60px !important;width:auto !important;-webkit-app-region:drag !important;}' +
        '#titleBar .title-name{line-height:38px !important;height:38px !important;display:flex !important;align-items:center !important;font-size:13px !important;font-weight:500 !important;-webkit-app-region:drag !important;user-select:none !important;}' +
        '#container{height:calc(100% - 38px) !important;max-height:calc(100% - 38px) !important;}' +
        '#sidebarNav,#main-tab{height:100% !important;}' +
        '.zalo-linux-tb-controls{display:flex !important;align-items:center !important;height:100% !important;margin-left:auto !important;-webkit-app-region:no-drag !important;z-index:1000 !important;}' +
        '.zalo-linux-close-button{-webkit-app-region:no-drag !important;pointer-events:auto !important;cursor:pointer !important;width:48px !important;height:38px !important;display:flex !important;align-items:center !important;justify-content:center !important;background:transparent !important;border:none !important;outline:none !important;padding:0 !important;margin:0 !important;color:var(--text-secondary,#999999) !important;border-top-right-radius:12px !important;transition:background-color 0.15s ease,color 0.15s ease !important;}' +
        '.zalo-linux-close-button svg{width:12px !important;height:12px !important;display:block !important;pointer-events:none !important;}' +
        '.zalo-linux-close-button:hover{background-color:#e81123 !important;color:#ffffff !important;}' +
        '.zalo-linux-close-button:active{background-color:#c40e1d !important;color:#ffffff !important;}' +
        'html.is-maximized .zalo-linux-close-button,body.is-maximized .zalo-linux-close-button{border-top-right-radius:0px !important;}' +
        '</style>';

      if (content.includes('<style id="zalo-transparent-base">')) {
        content = content.replace(/<style id="zalo-transparent-base">[\s\S]*?<\/style>/, fullInlineStyle);
        changed = true;
      } else {
        content = content.replace('</head>', `${fullInlineStyle}</head>`);
        changed = true;
      }

      if (changed) {
        fs.writeFileSync(htmlPath, content, 'utf8');
        logger.dim(`Patched ${path.basename(htmlPath)} with full Linux window rounded styles`);
      }
    }
  }

  // 4. Inject rounded corner style and maximize handler into preloads
  const preloads = [
    path.join(mainDistDir, 'preload-render.js'),
    path.join(mainDistDir, 'compact-app-preload.js')
  ];
  for (const preloadPath of preloads) {
    if (fs.existsSync(preloadPath)) {
      let content = fs.readFileSync(preloadPath, 'utf8');
      if (content.includes('// --- Zalo Linux Window Rounded Corners & Background Transparency ---')) {
        // Replace existing injection with updated version
        const startIdx = content.indexOf('// --- Zalo Linux Window Rounded Corners & Background Transparency ---');
        content = content.substring(0, startIdx).trimEnd() + '\n' + ROUNDED_PRELOAD_INJECTION + '\n';
        fs.writeFileSync(preloadPath, content, 'utf8');
        logger.dim(`Updated rounded styles in ${path.basename(preloadPath)}`);
      } else {
        content = content.trimEnd() + '\n' + ROUNDED_PRELOAD_INJECTION + '\n';
        fs.writeFileSync(preloadPath, content, 'utf8');
        logger.dim(`Injected rounded styles into ${path.basename(preloadPath)}`);
      }
    }
  }

  logger.success('Linux window appearance patch applied successfully');
}

if (require.main === module) {
  main();
}

module.exports = { main };
