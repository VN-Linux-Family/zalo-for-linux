/**
 * setup-zcall-bridge.js
 *
 * Prepares the call-v2 Wine runtime:
 *
 *   1. Downloads the official Windows Zalo installer and extracts
 *      plugins/capture/ (ZaloCall.exe + Qt DLLs + plugins) into
 *      app/native/qt-call-and-cap/, then trims unneeded files
 *   2. Compiles pipebridge.c (252KB named-pipe <-> TCP pump, no runtime
 *      needed) with mingw, falling back to the committed prebuilt exe
 *   3. Compiles streamproxy.c (LD_PRELOAD shim that redirects ZaloCall's
 *      screen-capture reads to the Wayland bridge display) with gcc
 *
 * No proprietary binaries are committed to this repository — everything is
 * fetched from official sources at setup time (same policy as the macOS DMG).
 */

const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const https = require('https');
const logger = require('./utils/logger');

const ROOT = path.join(__dirname, '..');
const TARGET = path.join(ROOT, 'app', 'native', 'qt-call-and-cap');
const TEMP_DIR = path.join(ROOT, 'temp');

const ZALO_WIN_PATTERN = 'https://res-download-pc.zadn.vn/win/ZaloSetup-VERSION.exe';

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    https.get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        file.close();
        return download(res.headers.location, dest).then(resolve, reject);
      }
      if (res.statusCode !== 200) {
        file.close();
        fs.unlinkSync(dest);
        reject(new Error('HTTP ' + res.statusCode + ' for ' + url));
        return;
      }
      res.pipe(file);
      file.on('finish', () => { file.close(); resolve(); });
    }).on('error', (e) => {
      file.close();
      try { fs.unlinkSync(dest); } catch (_) { /* ignore */ }
      reject(e);
    });
  });
}

async function getWindowsVersion() {
  return new Promise((resolve, reject) => {
    https.get('https://zalo.me/download/zalo-pc?utm=90000', {
      headers: {
        'User-Agent': 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36'
      }
    }, (res) => {
      if (res.statusCode === 302 && res.headers.location) {
        const m = res.headers.location.match(/ZaloSetup-([0-9.]+)\.exe/);
        if (m) return resolve(m[1]);
      }
      reject(new Error('Could not resolve Windows Zalo version'));
    }).on('error', reject);
  });
}

function sevenz(args) {
  execSync(`7z ${args}`, { cwd: TEMP_DIR, stdio: 'pipe' });
}

async function main() {
  fs.ensureDirSync(TEMP_DIR);

  // -------------------------------------------------------------------------
  // 1. plugins/capture from the Windows installer
  //    NOTE: the Windows version is resolved independently of ZALO_VERSION
  //    (which refers to the macOS DMG) — the two version families can diverge.
  // -------------------------------------------------------------------------
  const version = process.env.ZALO_WIN_VERSION || await getWindowsVersion();
  logger.info(`Setting up call-v2 runtime from Zalo Windows v${version}...`);

  const exeName = `ZaloSetup-${version}.exe`;
  const exePath = path.join(TEMP_DIR, exeName);
  if (!fs.existsSync(exePath)) {
    logger.dim('Downloading Windows installer...');
    await download(ZALO_WIN_PATTERN.replace('VERSION', version), exePath);
  }

  const inner7z = path.join(TEMP_DIR, `zcall-bridge-${version}.7z`);
  if (!fs.existsSync(inner7z)) {
    logger.dim('Extracting installer payload...');
    sevenz(`e -y "${exeName}" '$PLUGINSDIR/app-32.7z' -ozcall-bridge-extract`);
    fs.copyFileSync(path.join(TEMP_DIR, 'zcall-bridge-extract', 'app-32.7z'), inner7z);
  }

  // version-tagged so CI caching never serves a stale engine for a new version
  const captureOut = path.join(TEMP_DIR, `capture-extract-${version}`);
  if (!fs.existsSync(path.join(captureOut, 'ZaloCall.exe'))) {
    sevenz(`x -y "${path.basename(inner7z)}" 'Zalo-${version}/plugins/capture/*' -o${captureOut}`);
  }

  const captureSrc = path.join(captureOut, `Zalo-${version}`, 'plugins', 'capture');
  fs.ensureDirSync(TARGET);
  fs.copySync(captureSrc, TARGET, { overwrite: true });
  logger.success('ZaloCall.exe + Qt runtime installed');

  // Trim the capture folder: keep only what ZaloCall.exe needs.
  // Verified by replay tests: 1-1 calls work without these.
  for (const junk of ['pdbs', 'translations', 'bearer', 'iconengines',
                      'playlistformats', 'sqldrivers', 'styles']) {
    fs.removeSync(path.join(TARGET, junk));
  }
  for (const junkFile of ['opengl32sw.dll',  // Qt software-GL fallback (unused under Wine)
                          'Qt5Sql.dll', 'Qt5Xml.dll']) {
    fs.removeSync(path.join(TARGET, junkFile));
  }
  // Note: ZaviMeet.exe (19MB) is KEPT — required for group video calls.
  logger.dim('Trimmed unneeded Qt plugins / DLLs');

  // -------------------------------------------------------------------------
  // 2. pipebridge.exe (tiny C named-pipe <-> TCP pump, no runtime needed).
  //    Compiled from source — requires mingw on the build machine
  //    (i686-w64-mingw32-gcc). Binaries are not committed (*.exe is
  //    gitignored per repo policy).
  // -------------------------------------------------------------------------
  const srcC = path.join(ROOT, 'zcall-bridge', 'pipebridge.c');
  const builtExe = path.join(ROOT, 'zcall-bridge', 'pipebridge.exe');
  if (fs.existsSync(srcC)) {
    try {
      execSync(`i686-w64-mingw32-gcc "${srcC}" -lws2_32 -O2 -o "${builtExe}"`, {
        cwd: ROOT, stdio: 'pipe'
      });
      logger.dim('pipebridge.exe compiled from source');
    } catch (e) {
      throw new Error(
        'mingw (i686-w64-mingw32-gcc) is required to build pipebridge.exe — ' +
        'install it with: sudo apt install gcc-mingw-w64-i686'
      );
    }
  }
  if (fs.existsSync(builtExe)) {
    fs.copyFileSync(builtExe, path.join(TARGET, 'pipebridge.exe'));
    logger.success('pipebridge.exe installed');
  } else {
    logger.warn('pipebridge.exe missing — build it with: i686-w64-mingw32-gcc zcall-bridge/pipebridge.c -lws2_32 -O2 -o zcall-bridge/pipebridge.exe');
  }

  // -------------------------------------------------------------------------
  // 3. streamproxy.so (LD_PRELOAD shim that redirects ZaloCall's
  //    screen-capture reads to the bridge display). Two builds of the same
  //    source, chosen at runtime by the wine flavor (see selectProxySo in
  //    plugins/zcall-bridge/index.js):
  //    - 32-bit: classic wine builds keep 32-bit unixlibs (lib/wine/i386-unix),
  //      so ZaloCall runs in a 32-bit process whose winex11 driver binds the
  //      32-bit libX11 — only a 32-bit preload can intercept.
  //    - 64-bit: pure-wow64 wine builds (Full variant's bundled runtime) host
  //      the 32-bit Windows code in ONE 64-bit process with 64-bit unixlibs —
  //      only a 64-bit preload can intercept there.
  // -------------------------------------------------------------------------
  const proxySrc = path.join(ROOT, 'zcall-bridge', 'streamproxy.c');
  const proxySo = path.join(ROOT, 'zcall-bridge', 'streamproxy.so');
  if (fs.existsSync(proxySrc)) {
    try {
      execSync(`gcc -m32 -shared -fPIC -O2 "${proxySrc}" -ldl -lX11 -lxcb -lpthread -o "${proxySo}"`, {
        cwd: ROOT, stdio: 'pipe'
      });
      logger.dim('streamproxy.so (32-bit) compiled from source');
    } catch (e) {
      throw new Error(
        '32-bit build toolchain is required for streamproxy.so — ' +
        'install with: sudo apt install gcc-multilib libc6-dev-i386 libx11-dev:i386 libxcb1-dev:i386 libxext-dev:i386' +
        ' (gcc said: ' + String(e.stderr || e.message).trim().slice(-300) + ')'
      );
    }

    // 3b. 64-bit variant — used when the selected wine is a pure-wow64 build
    //     (Full variant's bundled wine). Non-fatal: the 32-bit shim above is
    //     the hard requirement; this one only serves the wow64 runtime.
    const proxySo64 = path.join(ROOT, 'zcall-bridge', 'streamproxy-x86_64.so');
    try {
      execSync(`gcc -shared -fPIC -O2 "${proxySrc}" -ldl -lX11 -lxcb -lpthread -o "${proxySo64}"`, {
        cwd: ROOT, stdio: 'pipe'
      });
      logger.dim('streamproxy-x86_64.so (64-bit) compiled from source');
    } catch (e) {
      logger.warn('64-bit shim build failed — Full variants lose Wayland screen-share proxying: ' +
        String(e.stderr || e.message).trim().slice(-200));
    }
  } else {
    logger.warn('streamproxy.c missing — share screen will not work on Wayland');
  }

  // -------------------------------------------------------------------------
  // 4. camtest.exe — the per-machine camera probe (wine DirectShow capture)
  //    that decides whether the v4l2loopback bridge is needed. Same mingw
  //    toolchain as pipebridge.exe above, so it is required too.
  // -------------------------------------------------------------------------
  const camtestSrc = path.join(ROOT, 'zcall-bridge', 'camtest.c');
  const camtestExe = path.join(ROOT, 'zcall-bridge', 'camtest.exe');
  try {
    execSync(`i686-w64-mingw32-gcc "${camtestSrc}" -lstrmiids -lole32 -loleaut32 -o "${camtestExe}"`, {
      cwd: ROOT, stdio: 'pipe'
    });
    logger.dim('camtest.exe compiled from source');
  } catch (e) {
    throw new Error(
      'mingw (i686-w64-mingw32-gcc) is required to build camtest.exe — ' +
      'install it with: sudo apt install gcc-mingw-w64-i686' +
      ' (gcc said: ' + String(e.stderr || e.message).trim().slice(-300) + ')'
    );
  }

  // -------------------------------------------------------------------------
  // 5. Bundled 32-bit GStreamer stack (gst 1.28 + libv4l 1.32, Ubuntu 26.04
  //    i386 packages). Ubuntu 24.04's i386 gst 1.24 + libv4l 1.26 stalls
  //    UVC cameras (DQBUF EPIPE) in wine's capture path — classic wine
  //    always uses this bundled stack (see applyWineEnv in the plugin).
  //    The extracted tree IS committed (electron-builder honors .gitignore
  //    when packaging extraFiles, so zcall-bridge/gst-i386 must stay
  //    tracked); this step just regenerates it from the .debs.
  // -------------------------------------------------------------------------
  const gstDebDir = path.join(ROOT, 'scripts', 'gst-i386-debs');
  const gstOutDir = path.join(ROOT, 'zcall-bridge', 'gst-i386');
  if (fs.existsSync(gstDebDir)) {
    try {
      for (const deb of fs.readdirSync(gstDebDir)) {
        if (!deb.endsWith('.deb')) continue;
        execSync(`dpkg-deb -x "${path.join(gstDebDir, deb)}" "${gstOutDir}"`, {
          cwd: ROOT, stdio: 'pipe'
        });
      }
      logger.dim('gst-i386 bundle extracted from ' +
        fs.readdirSync(gstDebDir).filter((n) => n.endsWith('.deb')).length + ' debs');
    } catch (e) {
      logger.warn('gst-i386 extraction failed: ' + String(e.stderr || e.message).trim().slice(-200));
    }
  }

  logger.success('call-v2 runtime ready: ' + TARGET);
}

if (require.main === module) {
  main().catch((e) => {
    logger.error('Setup failed:', e.message);
    process.exit(1);
  });
}

module.exports = { main };
