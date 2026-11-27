const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const { APP_MAIN_DIST } = require('../utils/paths');

const PATCH_START = '// CLIPBOARD IMAGE PASTE FIX';
const PATCH_END = '// END CLIPBOARD IMAGE PASTE FIX';
const BUNDLE_MARKER = '__ZaBUNDLENAME__="preload-render"';
const MAIN_PATCH_MARKER = '/*zalo-linux-clipboard*/';

// Paste handler prepended to preload-render.js (before `__ZaBUNDLENAME__="preload-render"`).
// Uses Electron 43's native clipboard APIs (readImage, readBuffer, availableFormats)
// and dispatches a synthetic ClipboardEvent('paste') directly to Zalo's own paste listener
function buildPasteHandler() {
  return `${PATCH_START}
window.addEventListener('DOMContentLoaded', () => {
    const { clipboard, ipcRenderer } = require('electron');
    const fs = require('fs');
    const path = require('path');
    const { fileURLToPath } = require('url');

    const IMAGE_MIME = {
        png: 'image/png',
        jpg: 'image/jpeg',
        jpeg: 'image/jpeg',
        gif: 'image/gif',
        webp: 'image/webp',
        bmp: 'image/bmp',
        tiff: 'image/tiff',
        tif: 'image/tiff',
        avif: 'image/avif',
        jxl: 'image/jxl',
    };
    const DIRECT_IMAGE_MIMES = [
        ['image/png', 'png'],
        ['image/jpeg', 'jpg'],
        ['image/webp', 'webp'],
        ['image/gif', 'gif'],
    ];
    const syntheticEvents = new WeakSet();

    function hasNativeClipboardFile(data) {
        if (!data || !data.items) return false;
        for (let i = 0; i < data.items.length; i++) {
            const item = data.items[i];
            if (item.kind === 'file' && item.getAsFile()) {
                return true;
            }
        }
        return false;
    }

    function clipboardContainsOnlyText(data) {
        if (!data) return false;
        const types = Array.from(data.types || []);
        if (types.some(t => t.startsWith('image/') || t === 'text/uri-list')) {
            return false;
        }
        if (clipboard.has && (clipboard.has('text/uri-list') || clipboard.has('x-special/gnome-copied-files'))) {
            return false;
        }
        return Boolean(data.getData('text/plain'));
    }

    function filesFromUriLines(lines) {
        const files = [];
        for (const raw of lines) {
            const line = raw.trim();
            if (!line || line.startsWith('#') || !line.startsWith('file://')) continue;
            try {
                const filePath = fileURLToPath(line);
                const ext = path.extname(filePath).slice(1).toLowerCase();
                const mime = IMAGE_MIME[ext];
                if (!mime) continue;
                if (!fs.statSync(filePath).isFile()) continue;
                const buf = fs.readFileSync(filePath);
                files.push(new File([buf], path.basename(filePath), { type: mime }));
            } catch (_) {}
        }
        return files;
    }

    function readClipboardFiles() {
        try {
            const uriBuf = clipboard.readBuffer('text/uri-list');
            if (uriBuf && uriBuf.length > 0) {
                const files = filesFromUriLines(uriBuf.toString('utf8').split(/\\r?\\n/));
                if (files.length > 0) return files;
            }

            if (clipboard.has && clipboard.has('x-special/gnome-copied-files')) {
                const gnomeBuf = clipboard.readBuffer('x-special/gnome-copied-files');
                if (gnomeBuf && gnomeBuf.length > 0) {
                    const lines = gnomeBuf.toString('utf8').split(/\\r?\\n/).slice(1);
                    const files = filesFromUriLines(lines);
                    if (files.length > 0) return files;
                }
            }

            const formats = clipboard.availableFormats ? clipboard.availableFormats() : [];
            for (const [mime, ext] of DIRECT_IMAGE_MIMES) {
                if (formats.includes(mime)) {
                    const buf = clipboard.readBuffer(mime);
                    if (buf && buf.length > 0) {
                        return [new File([buf], 'clipboard.' + ext, { type: mime })];
                    }
                }
            }

            const img = clipboard.readImage();
            if (img && !img.isEmpty()) {
                const png = img.toPNG();
                if (png && png.length > 0) {
                    return [new File([png], 'clipboard.png', { type: 'image/png' })];
                }
            }
        } catch (_) {}
        return [];
    }

    function dispatchPasteFiles(files) {
        if (!files || files.length === 0) return false;
        const dt = new DataTransfer();
        for (const file of files) {
            dt.items.add(file);
        }
        const pasteEvent = new ClipboardEvent('paste', {
            clipboardData: dt,
            bubbles: true,
            cancelable: true,
        });
        syntheticEvents.add(pasteEvent);
        const target = document.activeElement || document.body || document;
        target.dispatchEvent(pasteEvent);
        return true;
    }

    let _lastPaste = 0;
    document.addEventListener('paste', (e) => {
        if (syntheticEvents.has(e)) return;
        const data = e.clipboardData;
        if (hasNativeClipboardFile(data)) return;
        if (clipboardContainsOnlyText(data)) return;

        const now = Date.now();
        if (now - _lastPaste < 200) return;

        const files = readClipboardFiles();
        if (files.length > 0) {
            _lastPaste = now;
            e.preventDefault();
            e.stopImmediatePropagation();
            dispatchPasteFiles(files);
        }
    }, true);

    // Auto-paste captured screenshot from screenshot plugin
    try {
        ipcRenderer.on('zalo-linux-auto-paste-screenshot', () => {
            const files = readClipboardFiles();
            if (files.length > 0) {
                _lastPaste = Date.now();
                dispatchPasteFiles(files);
            }
        });
    } catch (_) {}
});
${PATCH_END}
`;
}

// Replace a `key:function(arg){...}` method body using balanced brace scanning
// so nested objects and minifier-renamed identifiers never break matching.
function replaceMethodBody(content, methodPrefix, replacement) {
  const start = content.indexOf(methodPrefix);
  if (start === -1) return null;
  const openBrace = content.indexOf('{', start + methodPrefix.length);
  if (openBrace === -1) return null;

  let depth = 0;
  let inStr = null;
  let escaped = false;
  for (let i = openBrace; i < content.length; i++) {
    const ch = content[i];
    if (inStr) {
      if (escaped) {
        escaped = false;
      } else if (ch === '\\') {
        escaped = true;
      } else if (ch === inStr) {
        inStr = null;
      }
      continue;
    }
    if (ch === '"' || ch === "'" || ch === '`') {
      inStr = ch;
    } else if (ch === '{') {
      depth++;
    } else if (ch === '}') {
      depth--;
      if (depth === 0) {
        return content.slice(0, start) + replacement + content.slice(i + 1);
      }
    }
  }
  return null;
}

// Zalo's renderer paste handler (_e -> Ee) always calls $zscreencap.getClipboard(userId)
// before calling uploadPhoto(). On Windows/macOS, ZaloCap responds with { requestId, content }.
// On Linux without ZaloCap, main.js rejected with "clipboard native not ready" and returned null,
// causing Zalo's renderer to silently abort every image paste!
function patchMainContent(content) {
  if (content.includes(MAIN_PATCH_MARKER)) {
    return { content, status: 'already' };
  }

  const getClipReplacement =
    `startGetClipboard:function(e){${MAIN_PATCH_MARKER}try{const{clipboard:c}=require("electron"),f=require("fs"),{fileURLToPath:u}=require("url");let r="";const b=c.readBuffer("text/uri-list");if(b&&b.length>0)r=b.toString("utf8");else if(c.has&&c.has("x-special/gnome-copied-files")){const g=c.readBuffer("x-special/gnome-copied-files");g&&g.length>0&&(r=g.toString("utf8").split(/\\r?\\n/).slice(1).join("\\n"))}const p=[];if(r)for(const l of r.split(/\\r?\\n/)){const s=l.trim();if(s&&!s.startsWith("#")&&s.startsWith("file://"))try{const d=u(s);f.statSync(d).isFile()&&p.push(d)}catch(_){}}return Promise.resolve({requestId:e,content:p.length>0?p.join("|"):null})}catch(_){return Promise.resolve({requestId:e,content:null})}}`;

  const rewriteClipReplacement =
    `startRewriteClipboard:function(e){${MAIN_PATCH_MARKER}return Promise.resolve({requestId:e})}`;

  let updated = replaceMethodBody(content, 'startGetClipboard:function(', getClipReplacement);
  if (updated === null) {
    throw new Error('Clipboard image paste patch not applied, Zalo changed: main.js: startGetClipboard');
  }

  const updatedRewrite = replaceMethodBody(updated, 'startRewriteClipboard:function(', rewriteClipReplacement);
  if (updatedRewrite === null) {
    throw new Error('Clipboard image paste patch not applied, Zalo changed: main.js: startRewriteClipboard');
  }

  return { content: updatedRewrite, status: 'patched' };
}

function patchContent(content) {
  const pasteHandler = buildPasteHandler();
  const handlerStart = content.indexOf(PATCH_START);
  const bundleStart = content.indexOf(BUNDLE_MARKER);

  if (bundleStart === -1) {
    throw new Error('Clipboard image paste patch not applied, Zalo changed: preload-render.js: bundle marker');
  }

  if (handlerStart !== -1 && bundleStart > handlerStart) {
    const updated = content.slice(0, handlerStart) + pasteHandler + content.slice(bundleStart);
    return { content: updated, status: updated === content ? 'already' : 'updated' };
  }

  if (handlerStart === -1) {
    const updated = content.slice(0, bundleStart) + pasteHandler + content.slice(bundleStart);
    return { content: updated, status: 'patched' };
  }

  throw new Error('Clipboard image paste patch not applied, Zalo changed: preload-render.js: bundle marker');
}

async function main(mainDistDir = APP_MAIN_DIST) {
  const mainJsPath = path.join(mainDistDir, 'main.js');
  if (!fs.existsSync(mainJsPath)) {
    throw new Error('Clipboard image paste patch not applied, Zalo changed: main.js: not found');
  }
  const mainContent = fs.readFileSync(mainJsPath, 'utf8');
  const mainRes = patchMainContent(mainContent);
  if (mainRes.status === 'already') {
    logger.dim('Main-process clipboard IPC handler already up to date in main.js');
  } else {
    fs.writeFileSync(mainJsPath, mainRes.content, 'utf8');
    logger.dim('Patched main-process clipboard IPC handler in main.js');
  }

  const preloadPath = path.join(mainDistDir, 'preload-render.js');
  if (!fs.existsSync(preloadPath)) {
    throw new Error('Clipboard image paste patch not applied, Zalo changed: preload-render.js: not found');
  }

  const content = fs.readFileSync(preloadPath, 'utf8');
  const { content: updated, status } = patchContent(content);

  if (status === 'already') {
    logger.dim('Clipboard image paste handler already up to date in preload-render.js');
    return;
  }

  fs.writeFileSync(preloadPath, updated, 'utf8');
  logger.dim(`${status === 'updated' ? 'Updated' : 'Patched'} clipboard image paste handler in preload-render.js`);
}

if (require.main === module) {
  main();
}

module.exports = {
  PATCH_START,
  PATCH_END,
  BUNDLE_MARKER,
  MAIN_PATCH_MARKER,
  buildPasteHandler,
  patchMainContent,
  patchContent,
  main,
};
