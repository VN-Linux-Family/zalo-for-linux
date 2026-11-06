'use strict';

const { execFile, execSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');

/**
 * Capture full screen to a local PNG file.
 * Supports Wayland (via grim or xdg-desktop-portal silent screenshot)
 * and X11 (via xdg-desktop-portal, desktopCapturer, or ImageMagick import).
 *
 * @param {Object} options
 * @param {import('electron').DesktopCapturer} [options.desktopCapturer]
 * @param {import('electron').Screen} [options.screen]
 * @param {string} [options.targetDir]
 * @returns {Promise<string|null>} Path to saved PNG file or null on failure
 */
async function captureScreen({ desktopCapturer, screen, targetDir } = {}) {
  const dir = targetDir || path.join(os.tmpdir(), 'zalo-screencap');
  if (!fs.existsSync(dir)) {
    try {
      fs.mkdirSync(dir, { recursive: true });
    } catch (err) {
      console.error('[Screenshot Plugin] Failed to create target directory:', dir, err.message);
      return null;
    }
  }
  const outFile = path.join(dir, `capture_${Date.now()}_${Math.random().toString(36).slice(2, 8)}.png`);

  const isWayland = process.env.XDG_SESSION_TYPE === 'wayland' || Boolean(process.env.WAYLAND_DISPLAY);
  const failures = [];

  // 1. Try silent xdg-desktop-portal screenshot (works across GNOME, KDE, Cosmic, XFCE on both Wayland & X11)
  const portalResult = await tryPortal(outFile);
  if (portalResult.success) {
    return outFile;
  }
  failures.push({ method: 'xdg-desktop-portal', reason: portalResult.error });

  // 2. If Wayland, try `grim` (for wlroots/Sway/Hyprland)
  if (isWayland) {
    const grimResult = await tryGrim(outFile);
    if (grimResult.success) {
      return outFile;
    }
    failures.push({ method: 'grim', reason: grimResult.error });
  }

  // 3. If on X11 and desktopCapturer is provided, try desktopCapturer
  if (!isWayland && desktopCapturer && screen) {
    const capturerResult = await tryDesktopCapturer(desktopCapturer, screen, outFile);
    if (capturerResult.success) {
      return outFile;
    }
    failures.push({ method: 'desktopCapturer', reason: capturerResult.error });
  }

  // 4. Try ImageMagick import on X11
  if (!isWayland) {
    const importResult = await tryImport(outFile);
    if (importResult.success) {
      return outFile;
    }
    failures.push({ method: 'import', reason: importResult.error });
  }

  // If all methods failed, print detailed diagnostics
  const envInfo = {
    XDG_SESSION_TYPE: process.env.XDG_SESSION_TYPE || '(unset)',
    XDG_CURRENT_DESKTOP: process.env.XDG_CURRENT_DESKTOP || '(unset)',
    DESKTOP_SESSION: process.env.DESKTOP_SESSION || '(unset)',
    WAYLAND_DISPLAY: process.env.WAYLAND_DISPLAY || '(unset)',
    DISPLAY: process.env.DISPLAY || '(unset)',
    targetPath: outFile
  };

  console.error('[Screenshot Plugin] Screen capture failed! All available capture methods were exhausted:');
  console.error('[Screenshot Plugin] Environment:', JSON.stringify(envInfo, null, 2));
  console.error('[Screenshot Plugin] Method failure details:');
  failures.forEach((f, idx) => {
    console.error(`  ${idx + 1}. [${f.method}]: ${f.reason}`);
  });

  return null;
}

function tryGrim(outFile) {
  return new Promise((resolve) => {
    try {
      execSync('which grim', { stdio: 'ignore' });
    } catch (_) {
      return resolve({ success: false, error: 'Command "grim" not found in PATH' });
    }
    execFile('grim', [outFile], { timeout: 4000 }, (err, _stdout, stderr) => {
      if (!err && fs.existsSync(outFile) && fs.statSync(outFile).size > 1000) {
        resolve({ success: true });
      } else {
        const errDetail = (stderr && stderr.trim()) || (err && err.message) || 'Generated file is empty or missing';
        if (fs.existsSync(outFile)) {
          try { fs.unlinkSync(outFile); } catch (_) {}
        }
        resolve({ success: false, error: errDetail });
      }
    });
  });
}

/**
 * Resolves an executable path on the real filesystem for portal.py.
 * External binaries like python3 cannot execute files packaged inside app.asar.
 *
 * @param {string} [baseDir]
 * @returns {string} Path to portal.py accessible on the filesystem
 */
function getPortalScriptPath(baseDir = __dirname) {
  const defaultPath = path.join(baseDir, 'portal.py');
  if (!defaultPath.includes('app.asar')) {
    return defaultPath;
  }

  // 1. Check if electron-builder unpacked it into app.asar.unpacked
  const unpackedPath = defaultPath.replace('app.asar', 'app.asar.unpacked');
  if (fs.existsSync(unpackedPath)) {
    return unpackedPath;
  }

  // 2. Fallback: extract the script to temp directory so external python3 can execute it
  try {
    const tempDir = path.join(os.tmpdir(), 'zalo-screencap');
    if (!fs.existsSync(tempDir)) {
      fs.mkdirSync(tempDir, { recursive: true });
    }
    const extractedPath = path.join(tempDir, 'portal.py');
    const content = fs.readFileSync(defaultPath);
    if (!fs.existsSync(extractedPath) || fs.statSync(extractedPath).size !== content.length) {
      fs.writeFileSync(extractedPath, content, { mode: 0o755 });
    }
    return extractedPath;
  } catch (err) {
    console.error('[Screenshot Plugin] Failed to extract portal.py from asar:', err.message);
    return defaultPath;
  }
}

function tryPortal(outFile) {
  return new Promise((resolve) => {
    const scriptPath = getPortalScriptPath();
    execFile('python3', [scriptPath, '--output', outFile], { timeout: 12000 }, (err, _stdout, stderr) => {
      if (!err && fs.existsSync(outFile) && fs.statSync(outFile).size > 1000) {
        resolve({ success: true });
      } else {
        const errDetail = (stderr && stderr.trim()) || (err && err.message) || 'Portal finished but output image file is missing or empty';
        if (fs.existsSync(outFile)) {
          try { fs.unlinkSync(outFile); } catch (_) {}
        }
        resolve({ success: false, error: errDetail });
      }
    });
  });
}

async function tryDesktopCapturer(desktopCapturer, screen, outFile) {
  try {
    const display = screen.getPrimaryDisplay();
    if (!display || !display.size) {
      return { success: false, error: 'Cannot retrieve primary display geometry' };
    }
    const { width, height } = display.size;
    const scale = display.scaleFactor || 1;
    const sources = await desktopCapturer.getSources({
      types: ['screen'],
      thumbnailSize: {
        width: Math.round(width * scale),
        height: Math.round(height * scale)
      }
    });
    if (!sources || sources.length === 0) {
      return { success: false, error: 'desktopCapturer returned no screen sources' };
    }
    const img = sources[0].thumbnail;
    if (img.isEmpty()) {
      return { success: false, error: 'desktopCapturer returned empty thumbnail' };
    }
    const pngBuf = img.toPNG();
    if (pngBuf.length <= 5000) {
      return { success: false, error: `Captured image buffer is suspiciously small (${pngBuf.length} bytes, likely black buffer on Wayland/XWayland)` };
    }
    fs.writeFileSync(outFile, pngBuf);
    return { success: true };
  } catch (err) {
    return { success: false, error: err.message };
  }
}

function tryImport(outFile) {
  return new Promise((resolve) => {
    try {
      execSync('which import', { stdio: 'ignore' });
    } catch (_) {
      return resolve({ success: false, error: 'Command "import" (ImageMagick) not found in PATH' });
    }
    execFile('import', ['-silent', '-window', 'root', outFile], { timeout: 5000 }, (err, _stdout, stderr) => {
      if (!err && fs.existsSync(outFile) && fs.statSync(outFile).size > 1000) {
        resolve({ success: true });
      } else {
        const errDetail = (stderr && stderr.trim()) || (err && err.message) || 'ImageMagick import command failed';
        if (fs.existsSync(outFile)) {
          try { fs.unlinkSync(outFile); } catch (_) {}
        }
        resolve({ success: false, error: errDetail });
      }
    });
  });
}

module.exports = { captureScreen, getPortalScriptPath };
