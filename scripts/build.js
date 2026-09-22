const { execSync, spawn } = require('child_process');
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const logger = require('./utils/logger');

const BASE_DIR = path.join(__dirname, '..');
const APP_DIR = path.join(BASE_DIR, 'app');

let ZALO_VERSION = null;
const builtFiles = [];

async function main() {
  try {
    // Read version from package.json.bak
    const packageJsonBakPath = path.join(APP_DIR, 'package.json.bak');
    if (fs.existsSync(packageJsonBakPath)) {
      const packageJson = JSON.parse(fs.readFileSync(packageJsonBakPath, 'utf8'));
      ZALO_VERSION = packageJson.version;
      logger.info('Zalo version from package.json.bak:', ZALO_VERSION);

      // Export global outputs for workflow
      if (process.env.GITHUB_OUTPUT) {
        fs.appendFileSync(process.env.GITHUB_OUTPUT, `zalo_version=${ZALO_VERSION}\n`);
      }
    } else {
      logger.warn('package.json.bak not found, version will be unknown');
    }

    // A leftover bundled runtime (e.g. from a crashed previous run) would
    // silently bloat the standard variants — start clean; Phase 3 re-bundles.
    fs.rmSync(path.join(APP_DIR, 'native', 'wine-runtime'), { recursive: true, force: true });
    fs.rmSync(path.join(APP_DIR, 'native', 'gst-runtime'), { recursive: true, force: true });
    fs.rmSync(path.join(BASE_DIR, 'temp', 'runtime-stash'), { recursive: true, force: true });

    // The capture shims are compiled build artifacts: packaging silently
    // drops them when missing (electron-builder honors .gitignore for
    // extraFiles — the exact bug that shipped share-screen-dead AppImages
    // before). Fail loudly instead.
    for (const shim of ['streamproxy.so', 'streamproxy-x86_64.so']) {
      if (!fs.existsSync(path.join(BASE_DIR, 'zcall-bridge', shim))) {
        throw new Error('missing zcall-bridge/' + shim +
          ' — run the SETUP phase first (npm run main / node scripts/setup-zcall-bridge.js)');
      }
    }

    // ZALO_ONLY_FULL=1: skip the two standard variants (dev loop — saves
    // two electron-builder passes). CI always builds all four.
    const onlyFull = process.env.ZALO_ONLY_FULL === '1';

    if (!onlyFull) {
      // Phase 1: Build original Zalo
      logger.step('PHASE 1: Building Zalo (Original)');
      await build('(Original)', '');
    }

    // Phase 1.5: Full variant of the original (no ZaDark) — wine bundled.
    logger.step('PHASE 1.5: Building Zalo (Full — wine bundled, no ZaDark)');
    await bundleWineRuntime();
    await bundleGstRuntime();
    await build('(Full — wine bundled)', '-PlainFull');
    // STASH the runtimes instead of deleting: the standard variants must not
    // contain them (Phase 2 builds clean), but Phase 3 needs the same trees —
    // deleting here used to force a full re-download + re-bundle (94MB wine +
    // minutes of gst post-extract) every run.
    const stash = path.join(BASE_DIR, 'temp', 'runtime-stash');
    fs.mkdirSync(stash, { recursive: true });
    for (const name of ['wine-runtime', 'gst-runtime']) {
      const src = path.join(APP_DIR, 'native', name);
      if (fs.existsSync(src)) fs.renameSync(src, path.join(stash, name));
    }

    // Phase 2: Apply ZaDark integration and build final product
    // Patch ZaDark directly into APP_DIR (needed for the -Full variant too)
    await integrateZaDark();
    if (!onlyFull) {
      logger.step('PHASE 2: Building Zalo (with ZaDark)');
      await build('(with ZaDark)', '-ZaDark');
    }

    // Phase 3: Full variant of the ZaDark build — wine bundled, so the call
    // feature works out of the box with no first-run download.
    logger.step('PHASE 3: Building Zalo (Full — wine bundled, with ZaDark)');
    for (const name of ['wine-runtime', 'gst-runtime']) {
      const src = path.join(stash, name);
      if (fs.existsSync(src)) fs.renameSync(src, path.join(APP_DIR, 'native', name));
    }
    await bundleWineRuntime();
    await bundleGstRuntime();
    await build('(Full — wine bundled)', '-Full');
    // Release asset for the standard variants' first-run download — must run
    // BEFORE the rmSyncs below (the only moment the final tree exists).
    await packageGstAsset();
    fs.rmSync(path.join(APP_DIR, 'native', 'wine-runtime'), { recursive: true, force: true });
    fs.rmSync(path.join(APP_DIR, 'native', 'gst-runtime'), { recursive: true, force: true });
    fs.rmSync(path.join(BASE_DIR, 'temp', 'runtime-stash'), { recursive: true, force: true });

    // Final summary
    logger.step('BUILD SUMMARY');
    if (builtFiles.length > 0) {
      builtFiles.forEach(({ type, name, sizeStr }) => {
        logger.info(`${type} • ${name} (${sizeStr})`);
      });
    } else {
      logger.warn('No AppImage files were built in this run');
    }
  } catch (error) {
    logger.error('Main workflow failed:', error.message);
    process.exit(1);
  }
}

// Full variants bundle the CLASSIC wine build: the wow64 build crashes
// ZaloCall's DirectShow camera path on some hosts (wine qcap WoW64
// media-type marshaling bug — upstream MR10269/10377, unmerged as of
// 11.17). Classic has no WoW64 boundary and is immune; it needs the host's
// 32-bit libraries, which the app's settings dialog guides users through.
// WINE_DOWNLOAD_URL in plugins/zcall-bridge/index.js is the IDENTICAL URL —
// the standard-variant first-run download matches the bundled runtime;
// keep the two in sync.
const WINE_DOWNLOAD_URL_CLASSIC =
  'https://github.com/Kron4ek/Wine-Builds/releases/download/11.17/wine-11.17-amd64.tar.xz';

// GStreamer packages bundled for the Full variant (64-bit, Ubuntu jammy so
// the glibc floor is 2.35 — Ubuntu 22.04/Mint 21+). The 64-bit tree now
// serves ONLY the Wayland screen-share bridge
// (pipewiresrc ! videoconvert ! ximagesink) — call media runs on the
// bundled i386 stack (zcall-bridge/gst-i386) since only classic wine is
// supported. libav/plugins-good/blas/lapack were the old wow64 call path:
// dropped, they were the bulk of the tree.
const GST_PACKAGES = [
  'libgstreamer1.0-0',
  'libgstreamer-plugins-base1.0-0', // libgstvideo/libgstbase — the libs the plugins link
  'gstreamer1.0-plugins-base',      // the plugin .so files (videoconvert, …)
  // glib dlopens libpcre for regex — a Recommends, so --no-install-recommends
  // drops it; the gst-plugin-scanner links it directly.
  'libpcre3',
];

// Wayland screen-share bridge stack bundled for the Full variant (jammy
// 64-bit only). Xvfb links libGL/libGLX/libGLdispatch in its NEEDED set —
// the GL strip below stays mandatory, so those three resolve from the HOST
// mesa at runtime (always present in a graphical session); see the ldd gate
// allowlist below.
const BRIDGE_X_PACKAGES = [
  'xvfb', 'xserver-common', 'xkb-data', 'xauth', 'x11-xkb-utils', 'xdotool',
  'gstreamer1.0-tools', // gst-launch-1.0 — screenbridge.py launches the pipeline with it
  // Debian splits the X-dependent plugins (ximagesink/xvimagesink) out of
  // plugins-base into gstreamer1.0-x.
  'gstreamer1.0-x',
];

// python3 is NOT bundled anymore: screenbridge.py runs on the HOST python3
// (every desktop distro ships it; the app checks python3-dbus/python3-gi and
// shows an install dialog when missing). The bundled python closure was
// ~20-25MB of the tree — see bridgeTools() in the plugin for the host side.

// Pipewiresrc: Ubuntu ships it in a DEDICATED package (gstreamer1.0-pipewire,
// 150KB, contains exactly libgstpipewire.so) — NOT in plugins-bad, whose
// closure (blas3/LLVM) must never land in the tree. All four debs extract
// whole; no closure, no cherry-pick needed. libspa-0.2-bluetooth is a hard
// Depends of libpipewire-0.3-0 on jammy (harmless if not — the ldd gate is
// self-correcting).
const PW_PACKAGES = [
  'gstreamer1.0-pipewire', 'libpipewire-0.3-0', 'libspa-0.2-modules',
  'libspa-0.2-bluetooth',
];

/**
 * Trim a kron4ek wine tree to runtime-only content (~200MB of the ~850MB
 * extract): the tarball ships wine's full development kit — headers
 * (include/), import libs, .def/.c sources under lib/wine and the
 * winegcc/widl/winemaker toolchain in bin/. The call engine spawns only
 * bin/wine, bin/wineserver and bin/wineboot, and the wine loader never
 * reads the dev files. Mirrored in plugins/zcall-bridge (standard-variant
 * first-run install) so both paths get the same tree.
 */
function pruneWineTree(wineRoot) {
  fs.rmSync(path.join(wineRoot, 'include'), { recursive: true, force: true });
  for (const sub of ['lib', 'lib64']) {
    const dir = path.join(wineRoot, sub, 'wine');
    if (!fs.existsSync(dir)) continue;
    (function walk(d) {
      for (const e of fs.readdirSync(d, { withFileTypes: true })) {
        const p = path.join(d, e.name);
        if (e.isDirectory()) walk(p);
        else if (/\.(a|def|c)$/.test(e.name)) fs.rmSync(p, { force: true });
      }
    })(dir);
  }
  const keepBin = new Set(['wine', 'wineserver', 'wineboot']);
  const binDir = path.join(wineRoot, 'bin');
  for (const e of fs.readdirSync(binDir, { withFileTypes: true })) {
    if (!keepBin.has(e.name)) fs.rmSync(path.join(binDir, e.name), { recursive: true, force: true });
  }
}

async function bundleWineRuntime() {
  const target = path.join(APP_DIR, 'native', 'wine-runtime');
  if (fs.existsSync(path.join(target, 'bin', 'wine'))) {
    logger.dim('wine runtime already bundled, skipping download');
    return;
  }
  const tarball = path.join(APP_DIR, 'native', 'wine-bundle.tar.xz');
  logger.info('Downloading portable wine (classic) for the Full variant...');
  try {
    execSync(`curl -L --fail -o "${tarball}" "${WINE_DOWNLOAD_URL_CLASSIC}"`, {
      cwd: BASE_DIR, stdio: 'inherit'
    });
    fs.mkdirSync(target, { recursive: true });
    execSync(`tar -xf "${tarball}" -C "${target}" --strip-components=1`, {
      cwd: BASE_DIR, stdio: 'pipe'
    });
  } finally {
    try { fs.unlinkSync(tarball); } catch (e) { /* none */ }
  }
  if (!fs.existsSync(path.join(target, 'bin', 'wine'))) {
    throw new Error('wine binary not found after extract');
  }
  pruneWineTree(target);
  logger.success('wine runtime bundled into app/native/wine-runtime (pruned)');
}

/**
 * Bundle a self-contained 64-bit GStreamer tree for the Full variant.
 * Uses a throwaway ubuntu:22.04 container so apt resolves the exact jammy
 * dependency closure (glibc excluded — the container image already ships
 * libc, so the download cache never includes it). Missing docker is NOT
 * fatal: the Full AppImage still builds and falls back to the host's 64-bit
 * GStreamer at runtime.
 */
// Everything the runtime needs beyond the plain gst libs (paths relative to
// the tree root): keeps a pre-bridge-era bundle from silently satisfying the
// skip gates (the CI temp cache is keyed on package-lock.json, which does
// not change when this file does).
const RUNTIME_MARKERS = [
  'usr/lib/x86_64-linux-gnu/libgstreamer-1.0.so.0',
  'usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstpipewire.so',
  'usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstximagesink.so',
  'usr/bin/Xvfb',
];

async function bundleGstRuntime() {
  const target = path.join(APP_DIR, 'native', 'gst-runtime');
  const libDir = path.join(target, 'usr', 'lib', 'x86_64-linux-gnu');

  // Content stamp: hash of the exact fetch script + package set. The CI
  // temp cache restores whole trees built by OLDER versions of this file —
  // marker checks alone let a stale tree (e.g. one extracted before
  // libblas3/liblapack3 joined the set) silently satisfy the skip gates and
  // fail the ldd gate forever. The stamp is written into the tree by the
  // fetch script itself; anything else = stale = rebuild.
  const ALL_PACKAGES = GST_PACKAGES.concat(BRIDGE_X_PACKAGES, PW_PACKAGES, ['libpcre3']);
  const script = [
    'set -e',
    'export DEBIAN_FRONTEND=noninteractive',
    // GitHub runners are Azure VMs: the azure mirror is far faster than
    // archive.ubuntu.com, and ForceIPv4 sidesteps the broken-IPv6-route
    // crawling (68-300 kB/s instead of MB/s) that hits CI apt regularly.
    'sed -i "s|http://archive.ubuntu.com/ubuntu|http://azure.archive.ubuntu.com/ubuntu|; s|http://security.ubuntu.com/ubuntu|http://azure.archive.ubuntu.com/ubuntu|" /etc/apt/sources.list',
    'apt-get -o Acquire::ForceIPv4=true update -qq',
    // Fresh extract, but KEEP /out/cache: CI restores the deb cache there
    // and apt re-uses it (matching checksums skip the re-download).
    'rm -rf /out/root /out/cache-pw',
    'mkdir -p /out/cache/partial /out/cache-pw /out/root',
    // Silences "Download is performed unsandboxed as root" noise; IPv4-only
    // for the same CI speed reason as the update above.
    'APT() { apt-get -o APT::Sandbox::User=root -o Acquire::ForceIPv4=true "$@"; }',
    // ---- Phase 1: gst runtime + X bridge stack (not installed in the
    // ---- image, so the full closure lands: xserver-common, xkb-data,
    // ---- libxfont2, libpixman, libxdo3, ...).
    'APT -o Dir::Cache::archives=/out/cache install -y -qq ' +
      '--download-only --no-install-recommends ' +
      GST_PACKAGES.concat(BRIDGE_X_PACKAGES).join(' '),
    // libpcre3 ships in the base image, so `install --download-only` skips
    // it — pull it explicitly (download ignores Dir::Cache::archives, so
    // run it from inside the cache dir).
    'cd /out/cache && APT download libpcre3 && cd /out',
    // ---- Phase 2: pipewiresrc, NO closure — the explicit list below
    // ---- (gstreamer1.0-pipewire contains exactly the plugin .so).
    'cd /out/cache-pw && APT download ' + PW_PACKAGES.join(' ') + ' && cd /out',
    // ---- Deterministic extraction: only debs belonging to the CURRENT
    // ---- closure. Blindly extracting every cached deb lets stale debs
    // ---- from earlier package sets leak into the tree (bloat + a local
    // ---- tree that differs from a fresh-CI tree — the libblas/liblapack
    // ---- incident). apt-cache depends --recurse lists the FULL recursive
    // ---- closure BY NAME, INCLUDING packages already installed in the
    // ---- base image (zlib1g, liblzma5, libbz2... — the `-s install` plan
    // ---- silently omits them and their debs were never downloaded).
    'apt-cache depends --recurse --no-recommends --no-suggests ' +
      '--no-conflicts --no-breaks --no-replaces --no-enhances ' +
      ALL_PACKAGES.join(' ') + ' > /out/closure.txt',
    // Keep ONLY the first alternative of each Depends group — that is what
    // apt actually selects; expanding all alternatives drags in openblas/
    // blis/etc (~200MB of never-used providers).
    'sed -E \'s/( \\| [^ (]+( \\([^)]*\\))?)+//g\' /out/closure.txt | ' +
    'awk \'/^[^ ]/{if($1 !~ /^</) print $1} /^ *Depends:/{for(i=2;i<=NF;i++){sub(/[(:<].*/,"",$i); if($i!="|" && $i!="") print $i}}\' | grep -v "^<" | sort -u > /out/keep.txt',
    // Union with the explicit request list: even if the plan output format
    // ever changed and dropped a name, an explicitly-requested package must
    // always be extracted.
    'printf "%s\\n" ' + ALL_PACKAGES.join(' ') + ' >> /out/keep.txt',
    'sort -u /out/keep.txt -o /out/keep.txt',
    // Extraction blacklist: packages the recursive closure drags in that the
    // runtime never loads — the openblas/blis/atlas BLAS provider families
    // (libsphinxbase only needs libblas.so.3 + liblapack.so.3, which the
    // reference libblas3/liblapack3 debs provide), the perl toolchain
    // (nothing in the tree NEEDs libperl), OCR (tesseract/lept) and the
    // alternate avfilter build. The post-extraction prune below is the belt
    // for the same families in case a future package set reintroduces them
    // via other names/paths.
    'grep -vE "^(libatlas3-base|libblis3-.*|libopenblas0-.*|libavfilter-extra7|perl|perl-base|perl-modules-5.34|libperl5.34|liblept5|libtesseract4)$" /out/keep.txt > /out/keep.txt.tmp && mv /out/keep.txt.tmp /out/keep.txt',
    // Belt-and-braces: every package in the plan MUST have its deb in the
    // cache before extraction. `install --download-only` can silently skip
    // packages that apt deems satisfied (installed in the image, alternative
    // providers, restored-cache quirks) — fetch any missing one explicitly.
    // BATCHED (2 apt invocations total): per-package calls made this step
    // exceed the CI step timeout on cold caches.
    'missing=""',
    'for p in $(cat /out/keep.txt); do',
    '  ls /out/cache/${p}_*.deb /out/cache-pw/${p}_*.deb >/dev/null 2>&1 || missing="$missing $p";',
    'done',
    // One availability check for the whole missing set (filters virtuals),
    // then ONE download invocation for all real packages.
    'avail=$(apt-cache show $missing 2>/dev/null | grep "^Package: " | awk \'{print $2}\' | sort -u)',
    'if [ -n "$avail" ]; then (cd /out/cache && APT download $avail); fi',
    'for f in /out/cache/*.deb /out/cache-pw/*.deb; do',
    '  n=$(dpkg-deb -f "$f" Package 2>/dev/null || true)',
    '  if grep -qxF "$n" /out/keep.txt; then dpkg-deb -x "$f" /out/root; fi',
    'done',
    // Size prune (belt to the keep.txt blacklist, see above): BLAS/LAPACK
    // provider families beyond the reference blas3/lapack3; perl/tesseract
    // stacks nothing in the runtime NEEDs; intel mfx HW plugins (dlopen-
    // probed only — QSV never runs under wine, libx264 is the encoder).
    'cd /out/root/usr/lib/x86_64-linux-gnu 2>/dev/null && {',
    '  rm -rf openblas-* blis-* atlas mfx perl perl-base perl5.34*',
    '  rm -f libatlas* libcblas* libf77blas* liblapack_atlas* libmfxhw64* libperl* libtesseract* liblept*',
    '} || true',
    'rm -f /out/root/usr/bin/perl /out/root/usr/bin/perl5.34.0 2>/dev/null || true',
    'echo "$BUNDLE_STAMP" > /out/root/.bundle-stamp',
    // root-owned inside the container — give everything back to the
    // invoking user (NOT just root/: leftover root-owned dirs like
    // cache/partial break `find` runs elsewhere in the build with
    // "Permission denied" exit codes).
    'chown -R "$HOST_UID:$HOST_GID" /out',
  ].join('\n');
  const BUNDLE_STAMP = crypto.createHash('sha1').update(script).digest('hex').slice(0, 16);
  const stampOk = (root) => {
    try { return fs.readFileSync(path.join(root, '.bundle-stamp'), 'utf8').trim() === BUNDLE_STAMP; } catch (e) { return false; }
  };

  if (RUNTIME_MARKERS.every((m) => fs.existsSync(path.join(target, m))) && stampOk(target)) {
    logger.dim('gst runtime already bundled, skipping');
    return;
  }

  // Staged under temp/ (gitignored + CI-cached) so both Full phases and
  // re-runs share one docker pass.
  const stage = path.join(BASE_DIR, 'temp', 'gst-debs');
  const stageRoot = path.join(stage, 'root');
  if (!(RUNTIME_MARKERS.every((m) => fs.existsSync(path.join(stageRoot, m))) && stampOk(stageRoot))) {
    logger.info('Bundling 64-bit GStreamer + Wayland bridge stack (ubuntu:22.04 debs) for the Full variant...');
    fs.mkdirSync(stage, { recursive: true });
    // Script via a mounted file — nested shell quoting would eat `$f`.
    fs.writeFileSync(path.join(stage, 'fetch-gst.sh'), script);
    try {
      execSync(
        `docker run --rm -e HOST_UID=${process.getuid()} -e HOST_GID=${process.getgid()} ` +
        `-e BUNDLE_STAMP=${BUNDLE_STAMP} ` +
        `-v "${stage}:/out" ubuntu:22.04 bash /out/fetch-gst.sh`, {
          cwd: BASE_DIR, stdio: 'inherit', timeout: 1800000
        });
    } catch (e) {
      logger.warn('docker unavailable or failed — Full variant built WITHOUT bundled GStreamer ' +
        '(call video will need 64-bit gst on the host): ' + String(e.message).slice(-200));
      return;
    }
  }

  // Prune docs/systemd junk and make absolute symlinks relative so the tree
  // is relocatable. Also merge lib/ into usr/lib/ — a few debs still ship
  // files under /lib (pre-usr-merge layout), which the runtime
  // LD_LIBRARY_PATH entry would never see.
  for (const p of ['usr/share/doc', 'usr/share/man', 'etc', 'bin', 'sbin', 'var',
    // Dead weight the recursive closure drags in — nothing in the camera/
    // share pipelines renders text, so fonts/locale/TeX/Perl never load.
    'usr/share/texmf', 'usr/share/perl', 'usr/share/locale', 'usr/share/fonts']) {
    fs.rmSync(path.join(stageRoot, p), { recursive: true, force: true });
  }
  const legacyLib = path.join(stageRoot, 'lib', 'x86_64-linux-gnu');
  if (fs.existsSync(legacyLib)) {
    fs.cpSync(legacyLib, path.join(stageRoot, 'usr', 'lib', 'x86_64-linux-gnu'), {
      recursive: true, force: true, verbatimSymlinks: true
    });
    fs.rmSync(path.join(stageRoot, 'lib'), { recursive: true, force: true });
  }
  // jammy's libblas3/liblapack3 ship their .so in blas/ + lapack/ SUBDIRS,
  // which a real system reaches via /etc/ld.so.conf.d/blas-*.conf. Our
  // runtime LD_LIBRARY_PATH points only at the flat lib dir, so the loader
  // would silently fall through to the HOST's blas (if installed) or fail.
  // Flatten the subdirs into the lib dir.
  for (const sub of ['blas', 'lapack']) {
    const src = path.join(stageRoot, 'usr', 'lib', 'x86_64-linux-gnu', sub);
    if (fs.existsSync(src)) {
      fs.cpSync(src, path.join(stageRoot, 'usr', 'lib', 'x86_64-linux-gnu'), {
        recursive: true, force: true, verbatimSymlinks: true
      });
      fs.rmSync(src, { recursive: true, force: true });
    }
  }

  // Strip the GL/mesa chain. apt forces it in via libgstreamer-gl (hard dep
  // of plugins-base) but it must NOT ship: our LD_LIBRARY_PATH is prepended
  // to the whole wine process, so a bundled libGL would shadow the host's
  // mesa and break ZaloCall rendering on GPUs newer than jammy's drivers.
  // The capture path (v4l2src, videoconvert, libav) never touches GL; gl
  // plugins simply fail to load and gst blacklists them silently.
  const gstLibDir = path.join(stageRoot, 'usr', 'lib', 'x86_64-linux-gnu');
  const GL_STRIP = [
    'dri', 'libLLVM-*.so.*', 'libGLX*.so.*', 'libGL.so.*', 'libEGL*.so.*',
    'libgbm.so.*', 'libglapi.so.*', 'libxatracker.so.*', 'libGLES*.so.*',
    'libOpenGL.so.*', 'libgallium*.so.*', 'libOSMesa.so.*', 'libglx-*.so.*',
    'gstreamer-1.0/libgstgl.so', 'gstreamer-1.0/libgstopengl*.so',
  ];
  // Same class of bug one layer down: the host GL stack (mesa dri drivers,
  // AMDGPU-PRO's /opt/amdgpu libgallium) links these, and a bundled jammy
  // copy shadows the host's newer one under the prepended LD_LIBRARY_PATH.
  // On AMD-PRO machines libgallium's dlopen dies with "undefined symbol:
  // amdgpu_va_get_start_addr" (resolved against jammy libdrm_amdgpu 1.0.0)
  // and ZaloCall.exe never starts; the same shadowing killed Xvfb on that
  // machine (gate (d)). Every host GL stack ships these support libs, so
  // they always resolve from the host — the ldd gate allowlist below.
  const DRM_STRIP = [
    'libdrm.so.*', 'libdrm_amdgpu.so.*', 'libdrm_intel.so.*',
    'libdrm_nouveau.so.*', 'libdrm_radeon.so.*',
    'libzstd.so.*', 'libxshmfence.so.*', 'libpciaccess.so.*',
  ];
  // The screen bridge must use the HOST pipewire stack. jammy's 0.3.48
  // client cannot export stream nodes to modern daemons (1.x raised the
  // protocol floor: pw_core_export dies with "Protocol error"), and its
  // mandatory-module load resolves against the host moduledir regardless —
  // a failed pw_context_new() then segfaults gstpipewire via
  // pw_context_connect(NULL). The bridge only ever runs inside a Wayland
  // session, where host pipewire is the session's own infrastructure —
  // always present and always matching the daemon. libgstpipewire.so
  // stays: the client ABI is stable, it links host libpipewire cleanly.
  const PW_STRIP = [
    'libpipewire-0.3.so.*',
    // libspa base lib: host libpipewire may NEED it (distro-dependent) and
    // would otherwise resolve the jammy copy under the prepended
    // LD_LIBRARY_PATH — same shadowing class as DRM_STRIP.
    'libspa-0.2.so.*',
  ];
  // glibc core must NEVER ship either: the recursive apt closure
  // re-introduces libc6 (installed in the base image), and a bundled
  // libc.so.6/ld-linux under LD_LIBRARY_PATH breaks the HOST shell itself
  // (glibc version mismatch) — these always resolve from the host.
  const GLIBC_STRIP = [
    'libc.so.6*', 'ld-linux*.so.2*', 'libm.so.6*', 'libpthread.so.0*',
    'librt.so.1*', 'libdl.so.2*', 'libutil.so.1*', 'libgcc_s.so.1*',
    'libstdc++.so.6*', 'libresolv.so.2*', 'libnss_*.so.2*', 'libanl.so.1*',
    'libcrypt.so.1*',
  ];
  for (const pattern of GL_STRIP.concat(DRM_STRIP, PW_STRIP, GLIBC_STRIP)) {
    const dir = path.dirname(pattern);
    const base = path.basename(pattern);
    let files = [];
    try { files = fs.readdirSync(path.join(gstLibDir, dir)); } catch (e) { continue; }
    for (const f of files) {
      if (f === base || (base.includes('*') && f.startsWith(base.split('*')[0]) && f.endsWith(base.split('*').pop()))) {
        fs.rmSync(path.join(gstLibDir, dir, f), { recursive: true, force: true });
      }
    }
  }
  // spa plugins + pw modules belong to the stripped libpipewire — the host
  // stack loads its own copies from its own compiled-in dirs.
  for (const dir of ['spa-0.2', 'pipewire-0.3']) {
    fs.rmSync(path.join(gstLibDir, dir), { recursive: true, force: true });
  }
  const absLinks = [];
  (function walk(dir) {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const p = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(p);
      else if (entry.isSymbolicLink()) {
        const t = fs.readlinkSync(p);
        if (path.isAbsolute(t)) absLinks.push({ p, t });
      }
    }
  })(stageRoot);
  for (const { p, t } of absLinks) {
    const rel = path.relative(path.dirname(p), t);
    fs.unlinkSync(p);
    fs.symlinkSync(rel, p);
  }
  // Empty dir GST_PLUGIN_SYSTEM_PATH points at — isolates the wine process
  // from the host's (possibly different-version) system plugins.
  fs.mkdirSync(path.join(stageRoot, 'system'), { recursive: true });

  fs.rmSync(target, { recursive: true, force: true });
  fs.cpSync(stageRoot, target, { recursive: true, verbatimSymlinks: true });

  const EXISTENCE_GATE = [
    ['usr/lib/x86_64-linux-gnu', 'libgstreamer-1.0.so.0'],
    ['usr/lib/x86_64-linux-gnu', 'gstreamer-1.0/libgstpipewire.so'],  // cherry-picked
    // videoconvert guards the plugins-base PLUGIN package specifically — a
    // package-set regression (libs present, plugin .so files absent) fails
    // HERE instead of deep in gate (c).
    ['usr/lib/x86_64-linux-gnu', 'gstreamer-1.0/libgstvideoconvert.so'],
    ['usr/bin', 'Xvfb'], ['usr/bin', 'gst-launch-1.0'], ['usr/bin', 'gst-inspect-1.0'],
    ['usr/bin', 'xdotool'],
  ];
  for (const [base, f] of EXISTENCE_GATE) {
    if (!fs.existsSync(path.join(target, base, f))) {
      throw new Error('gst bundle incomplete after extraction: ' + base + '/' + f);
    }
  }

  // Every NEEDED entry of the key libs must resolve inside the bundle (the
  // runtime LD_LIBRARY_PATH contains ONLY this dir — a missing dep would be
  // a hard dlopen failure at call time, so fail the build now).
  // Three intentional exceptions:
  //  - Xvfb links libGL/libGLX/libGLdispatch DIRECTLY (checked: host ldd
  //    shows them) and must resolve them from the HOST mesa — bundling GL
  //    is forbidden (it would shadow the host mesa for the wine process
  //    too). Every graphical session ships mesa, so this is always
  //    satisfiable where Wayland sharing can exist.
  //  - The DRM_STRIP family (HOST_GPU set): GPU-driver support libs must
  //    resolve from the HOST so the host GL/dri stack never loads jammy
  //    copies (a shadowed libdrm kills ZaloCall.exe on AMDGPU-PRO hosts).
  //  - The PW_STRIP family (HOST_PW set): the bridge uses the host
  //    pipewire stack (see the PW_STRIP comment).
  const LDD_GATE = [
    'libgstreamer-1.0.so.0', 'gstreamer-1.0/libgstpipewire.so',
    'gstreamer-1.0/libgstvideoconvert.so',
    '../../bin/Xvfb', '../../bin/gst-launch-1.0',
    '../../bin/gst-inspect-1.0', '../../bin/xdotool',
  ];
  try {
    const lddFiles = LDD_GATE.map((f) => path.join(libDir, f));
    // Resolutions allowed to come from the HOST: glibc core (never bundled —
    // the container already ships libc) and Xvfb's GL trio. EVERYTHING else
    // must resolve INSIDE libDir — a host-ld.so.cache fallback (e.g. host
    // libblas under /usr/lib/x86_64-linux-gnu/blas) would make the bundle
    // pass locally yet fail on machines without that package. This gate must
    // catch exactly that.
    const HOST_CORE = new Set([
      'linux-vdso.so.1', 'ld-linux-x86-64.so.2', 'libc.so.6', 'libm.so.6',
      'libpthread.so.0', 'libdl.so.2', 'librt.so.1', 'libgcc_s.so.1',
      'libstdc++.so.6', 'libresolv.so.2', 'libnss_dns.so.2', 'libnss_files.so.2',
      'libnss_compat.so.2', 'libutil.so.1', 'libatomic.so.1',
    ]);
    // GPU-driver support libs (stripped by DRM_STRIP): they must resolve
    // from the HOST so the host GL/dri stack never loads a jammy copy
    // (see the DRM_STRIP comment — a shadowed libdrm kills ZaloCall.exe
    // on AMDGPU-PRO machines).
    const HOST_GPU = new Set([
      'libdrm.so.2', 'libdrm_amdgpu.so.1', 'libdrm_intel.so.1',
      'libdrm_nouveau.so.2', 'libdrm_radeon.so.1', 'libzstd.so.1',
      'libxshmfence.so.1', 'libpciaccess.so.0',
    ]);
    // Stripped by PW_STRIP: libgstpipewire.so resolves libpipewire from
    // the HOST (host spa/pw-module dirs follow — the bundle ships none).
    // Machines without host pipewire have no Wayland session, so the
    // bridge never runs there.
    const HOST_PW = new Set(['libpipewire-0.3.so.0', 'libspa-0.2.so.0']);
    const missing = [];
    let lddToolMissing = false;
    for (const f of lddFiles) {
      // lddFiles entries are already absolute (joined with libDir above);
      // path.join does NOT reset on absolute segments — a naive join would
      // DOUBLE the prefix.
      const full = path.isAbsolute(f) ? f : path.join(libDir, f);
      if (!fs.existsSync(full)) {
        // Hard defect: a gate target is absent even though the existence
        // gate passed moments ago — dump everything to pinpoint it.
        let listing = '';
        try { listing = fs.readdirSync(libDir).slice(0, 40).join('\n'); } catch (e) { listing = String(e.message); }
        let lstatInfo = '';
        try { lstatInfo = JSON.stringify(fs.lstatSync(full)); } catch (e) { lstatInfo = String(e.message); }
        throw new Error('ldd gate target missing: ' + full + '\nlstat: ' + lstatInfo + '\nlibDir listing (first 40):\n' + listing);
      }
      let out;
      try {
        out = execSync(`LD_LIBRARY_PATH="${libDir}" ldd "${full}"`, {
          encoding: 'utf8', stdio: 'pipe'
        });
      } catch (e) {
        const msg = String(e.stderr || e.message || '');
        // Only when the ldd TOOL itself is absent do we degrade to a warn —
        // any other failure (missing file, loader errors) is a hard defect.
        if (/ldd[^:]*:\s*not found/i.test(msg)) { lddToolMissing = true; break; }
        throw new Error('ldd failed on ' + full + ': ' + msg.slice(-400));
      }
      for (const line of out.split('\n')) {
        if (!line.trim()) continue;
        const m = line.match(/^\s*(\S+)\s*=>\s*(\S+)/);
        if (!m) continue; // vdso / direct-loader lines without a path
        const soname = m[1];
        const resolved = m[2];
        const isXvfbGl = f.includes('Xvfb') && /libGL|libGLdispatch|libGLX/.test(soname);
        if (resolved.startsWith(libDir)) continue;
        if (HOST_CORE.has(soname) || isXvfbGl || HOST_GPU.has(soname) || HOST_PW.has(soname)) continue;
        missing.push(path.basename(f) + ' -> ' + line.trim() + ' [resolves OUTSIDE bundle]');
      }
    }
    if (lddToolMissing) {
      logger.warn('ldd unavailable on this machine — self-containment check skipped');
      missing.length = 0;
    }
    if (missing.length) {
      // Diagnostics: which expected libs exist in the extracted tree / cache?
      const diag = [];
      try {
        const stageCache = path.join(stage, 'cache');
        const want = ['libblas', 'liblapack'];
        for (const w of want) {
          const inTree = (function walk(dir) {
            let hit = false;
            try {
              for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
                const p = path.join(dir, e.name);
                if (e.isDirectory()) hit = walk(p) || hit;
                else if (e.name.startsWith(w)) hit = true;
              }
            } catch (_) { /* none */ }
            return hit;
          })(libDir);
          let inCache = '';
          try { inCache = fs.readdirSync(stageCache).filter((x) => x.startsWith(w)).join(', '); } catch (_) { /* none */ }
          diag.push(`${w}: in-tree=${inTree} in-cache=[${inCache}]`);
        }
        let keepLine = '';
        try { keepLine = fs.readFileSync(path.join(stage, 'keep.txt'), 'utf8').split('\n').filter((l) => /blas|lapack/.test(l)).join(', '); } catch (_) { /* none */ }
        diag.push('keep.txt blas/lapack: [' + keepLine + ']');
      } catch (e) { diag.push('diag error: ' + e.message); }
      throw new Error('gst bundle has unresolved deps:\n' + missing.join('\n') + '\n' + diag.join('\n'));
    }
    logger.dim('gst bundle deps self-contained (' + LDD_GATE.length + ' files checked)');
  } catch (e) {
    if (/unresolved deps|ldd gate target missing|ldd failed on/.test(String(e.message || ''))) throw e;
    logger.warn('ldd dep check skipped (ldd unavailable): ' + String(e.message).slice(-120));
  }

  // Smoke-verify plugin loadability with RTLD_NOW (stricter than a lazy
  // load; catches symbol-level breakage the ldd check cannot). gst's own
  // scanner is unsuitable here — it speaks a pipe protocol when invoked
  // directly.
  try {
    const probe = path.join(stage, 'dlopen-probe.c');
    fs.writeFileSync(probe, [
      '#include <dlfcn.h>',
      '#include <stdio.h>',
      'int main(int c,char**v){for(int i=1;i<c;i++){void*h=dlopen(v[i],RTLD_NOW);',
      'if(!h){fprintf(stderr,"dlopen FAIL %s: %s\\n",v[i],dlerror());return 1;}}return 0;}',
    ].join('\n'));
    execSync(`gcc "${probe}" -ldl -o "${stage}/dlopen-probe" && ` +
      `LD_LIBRARY_PATH="${libDir}" "${stage}/dlopen-probe" ` +
      `"${libDir}/gstreamer-1.0/libgstpipewire.so" ` +
      `"${libDir}/gstreamer-1.0/libgstvideoconvert.so"`, {
      cwd: BASE_DIR, stdio: 'pipe', timeout: 60000
    });
    logger.dim('gst bundle verified (pipewire + videoconvert dlopen with RTLD_NOW)');
  } catch (e) {
    logger.warn('gst plugin dlopen check skipped/failed: ' + String(e.stderr || e.message).trim().slice(-300));
  }

  // ---- Gate (c): bundled gst-inspect must register the bridge elements
  // ---- (HARD). Inspect exits 1 when the element did not register — the
  // ---- plugin-dlopen/registration failure signal we want.
  try {
    const gstEnv = {
      PATH: process.env.PATH || '/usr/bin:/bin',
      LD_LIBRARY_PATH: libDir,
      GST_PLUGIN_PATH: libDir + '/gstreamer-1.0',
      GST_PLUGIN_SYSTEM_PATH: target + '/system',
      GST_PLUGIN_SCANNER: libDir + '/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner',
      GST_REGISTRY: stage + '/registry-bridge-test.bin',
    };
    for (const el of ['pipewiresrc', 'ximagesink', 'videoconvert']) {
      execSync(`"${target}/usr/bin/gst-inspect-1.0" ${el}`, {
        cwd: BASE_DIR, stdio: 'pipe', timeout: 60000, env: gstEnv
      });
    }
    logger.dim('gate (c): bundled gst-inspect registers pipewiresrc + ximagesink + videoconvert');
  } catch (e) {
    throw new Error('gate (c) FAILED — bundled gst cannot register bridge elements:\n' +
      String(e.stderr || e.message).trim().slice(-500));
  }

  // ---- Gate (d): bundled Xvfb smoke. WARN-grade on purpose: Xvfb has a
  // ---- HARD NEEDED on host libGL (we never bundle GL), so whether it
  // ---- starts depends on the BUILD machine's GL stack — healthy on any
  // ---- real desktop session, but broken/absent on some headless build
  // ---- boxes and CI runners. A failure here does NOT mean the bundle is
  // ---- bad; it means the bridge needs a desktop session with mesa, which
  // ---- is exactly the runtime environment it targets.
  try {
    let display = null;
    for (let n = 90; n <= 99 && !display; n++) {
      if (!fs.existsSync(`/tmp/.X${n}-lock`) && !fs.existsSync(`/tmp/.X11-unix/X${n}`)) display = ':' + n;
    }
    if (display) {
      const xvfb = spawn(target + '/usr/bin/Xvfb', [display, '-screen', '0', '320x240x24'], {
        stdio: ['ignore', 'ignore', 'pipe'],
        env: { PATH: process.env.PATH || '/usr/bin:/bin', LD_LIBRARY_PATH: libDir },
      });
      let stderrBuf = '';
      xvfb.stderr.on('data', (d) => { stderrBuf += d; });
      await new Promise((resolve) => setTimeout(resolve, 3500));
      if (xvfb.exitCode !== null) {
        logger.warn('gate (d): Xvfb died on this build machine (broken/absent host libGL — ' +
          'expected on headless builders; fine on desktop sessions):\n' + stderrBuf.slice(-300).split('\n').slice(-4).join('\n'));
      } else {
        xvfb.kill('SIGTERM');
        const fatal = stderrBuf.split('\n').filter((l) =>
          /Fatal server error|Server is already active|\(EE\)/.test(l) &&
          !/glx|GLX|Failed to load module|keymap|xkb|font|XKB/i.test(l));
        if (fatal.length) {
          logger.warn('gate (d): Xvfb fatal markers (build machine GL env):\n' + fatal.join('\n').slice(-400));
        } else {
          logger.dim(`gate (d): bundled Xvfb smoke OK on ${display} (no GLX module)`);
        }
      }
    } else {
      logger.warn('gate (d) skipped: no free X display in :90-:99');
    }
  } catch (e) {
    logger.warn('gate (d) skipped (host GL environment): ' + String(e.message).slice(-300));
  }

  logger.success('gst runtime + Wayland bridge stack bundled into app/native/gst-runtime');
}

/**
 * Package the bundled GStreamer tree as the per-version release asset the
 * standard variants download at first-run setup (gst-runtime-<ver>.tar.xz).
 * Must be called while app/native/gst-runtime still exists (main() does this
 * right after the Phase 3 Full build, before the rmSync cleanup).
 */
async function packageGstAsset() {
  const target = path.join(APP_DIR, 'native', 'gst-runtime');
  const marker = path.join(target, 'usr', 'lib', 'x86_64-linux-gnu', 'libgstreamer-1.0.so.0');
  if (!fs.existsSync(marker)) {
    logger.warn('no gst runtime to package (docker bundle failed?) — standard variants will fall back to host 64-bit gst');
    if (process.env.GITHUB_OUTPUT) {
      fs.appendFileSync(process.env.GITHUB_OUTPUT, 'gst_runtime_file=\ngst_runtime_name=\n');
    }
    return;
  }
  const name = `gst-runtime-${ZALO_VERSION || 'unknown'}.tar.xz`;
  const file = path.join(BASE_DIR, 'dist', name);
  logger.info('Packaging GStreamer runtime release asset: dist/' + name + ' ...');
  // xz level 2: level 6 would take 3-5 min on the ~640MB tree for only a
  // few percent smaller asset — build speed wins, size delta is negligible.
  fs.mkdirSync(path.join(BASE_DIR, 'dist'), { recursive: true });
  execSync(`tar -cJf "${file}" -C "${target}" .`, {
    cwd: BASE_DIR, stdio: 'pipe', env: Object.assign({}, process.env, { XZ_OPT: '-2' })
  });
  logger.success('gst release asset: ' + name + ' (' + Math.round(fs.statSync(file).size / 1024 / 1024) + 'MB)');
  if (process.env.GITHUB_OUTPUT) {
    fs.appendFileSync(process.env.GITHUB_OUTPUT, `gst_runtime_file=${'dist/' + name}\ngst_runtime_name=${name}\n`);
  }
}

async function integrateZaDark() {
  logger.info('Applying ZaDark patches...');

  try {
    // Verify ZaDark module is available
    const zadarkModulePath = path.join(BASE_DIR, 'plugins', 'zadark', 'build', 'pc', 'zadark-pc.js');
    if (!fs.existsSync(zadarkModulePath)) {
      throw new Error('ZaDark PC module not found - run "npm run prepare-zadark" first');
    }

    const zadarkPC = require(zadarkModulePath);
    zadarkPC.copyZaDarkAssets(BASE_DIR);
    zadarkPC.writeIndexFile(BASE_DIR);
    zadarkPC.writeBootstrapFile(BASE_DIR);
    zadarkPC.writePopupViewerFile(BASE_DIR);
    logger.success('ZaDark patches applied successfully');

  } catch (error) {
    logger.error('ZaDark integration failed:', error.message);
    logger.info('Continuing with original app directory...');
  }
}

async function build(buildName = '', outputSuffix = '') {
  try {
    // Get git commit hash for filename
    const commitHash = execSync('git rev-parse --short HEAD', { encoding: 'utf8' }).trim();

    // Set artifact name and build command based on build type
    let artifactName;
    let buildCommand;
    let buildTargets;
    let zadarkVersion = null;

    if (outputSuffix === '-ZaDark' || outputSuffix === '-Full') {
      // Read ZaDark version for custom naming (the Full variant also builds
      // on the ZaDark-integrated app directory)
      const zadarkPackagePath = path.join(BASE_DIR, 'plugins', 'zadark', 'package.json');
      zadarkVersion = 'unknown';

      if (fs.existsSync(zadarkPackagePath)) {
        try {
          const zadarkPackage = JSON.parse(fs.readFileSync(zadarkPackagePath, 'utf8'));
          zadarkVersion = zadarkPackage.version;
        } catch (error) {
          logger.warn('Could not read ZaDark version, using "unknown"');
        }
      }

      artifactName = `Zalo-${ZALO_VERSION}+ZaDark-${zadarkVersion}-${commitHash}${outputSuffix}.\${ext}`;
      // Native packages (deb/rpm/pacman) are STANDARD variants: the
      // package manager installs wine + GStreamer + the i386 libs from the
      // distro repos (declared in package.json), so the package itself
      // stays light (~200MB) and install-and-use — the same app as the
      // standard AppImage, with the components as system packages instead
      // of a first-run download. The Full variants (wine + GStreamer
      // bundled) remain AppImage-only.
      buildTargets = outputSuffix === '-Full' || outputSuffix === '-PlainFull'
        ? 'AppImage'
        : 'AppImage deb rpm pacman';
      buildCommand = `npx electron-builder --linux ${buildTargets} --config.linux.artifactName='${artifactName}' -c.extraMetadata.version=${ZALO_VERSION} --publish=never`;
      logger.info(`Building ${buildName} with Zalo: ${ZALO_VERSION}, ZaDark: ${zadarkVersion}, Commit: ${commitHash}`);
    } else if (outputSuffix === '-PlainFull') {
      artifactName = `Zalo-${ZALO_VERSION}-${commitHash}-Full.\${ext}`;
      buildTargets = 'AppImage';
      buildCommand = `npx electron-builder --linux ${buildTargets} --config.linux.artifactName='${artifactName}' -c.extraMetadata.version=${ZALO_VERSION} --publish=never`;
      logger.info(`Building ${buildName} with Zalo: ${ZALO_VERSION}, Commit: ${commitHash}`);
    } else {
      artifactName = `Zalo-${ZALO_VERSION}-${commitHash}.\${ext}`;
      buildTargets = 'AppImage deb rpm pacman';
      buildCommand = `npx electron-builder --linux ${buildTargets} --config.linux.artifactName='${artifactName}' -c.extraMetadata.version=${ZALO_VERSION} --publish=never`;
      logger.info(`Building ${buildName} with Zalo: ${ZALO_VERSION}, Commit: ${commitHash}`);
    }
    // Write build-info.json to the app directory so the AppImage will contain its metadata
    let repository = process.env.GITHUB_REPOSITORY || null;
    if (!repository) {
      try {
        const remote = execSync('git remote get-url origin', { encoding: 'utf8', stdio: 'pipe' }).trim();
        const m = remote.match(/github\.com[:/]([^/]+)\/([^/]+?)(?:\.git)?$/);
        if (m) repository = m[1] + '/' + m[2];
      } catch (e) { /* no git remote — asset downloads will fall back to host gst */ }
    }
    const buildInfo = {
      version: ZALO_VERSION,
      zadarkVersion: (outputSuffix === '-ZaDark' || outputSuffix === '-Full') ? zadarkVersion : null,
      commit: commitHash,
      buildDate: new Date().toISOString(),
      // Which GitHub repo hosts this build's release assets (gst-runtime
      // first-run download). CI: github.repository; local: derived from the
      // origin remote — so forks host their own assets automatically.
      repository,
    };
    
    const buildInfoPath = path.join(APP_DIR, 'pc-dist', 'build-info.json');
    if (fs.existsSync(path.join(APP_DIR, 'pc-dist'))) {
      fs.writeFileSync(buildInfoPath, JSON.stringify(buildInfo, null, 2), 'utf8');
      logger.dim(`Wrote metadata: ${buildInfoPath}`);
    } else {
      logger.warn('pc-dist directory not found, skipping build-info.json');
    }

    logger.dim(`Command: ${buildCommand}`);

    // Capture build output to get file information
    const buildOutput = execSync(buildCommand, {
      stdio: 'pipe',
      cwd: path.join(BASE_DIR),
      encoding: 'utf8'
    });

    // Parse build output to find every produced artifact (one `file=` line
    // per target: AppImage, deb, rpm, pacman).
    const artifactFiles = [...buildOutput.matchAll(/file=(dist\/[^\s]+)/g)].map((m) => m[1]);
    const variantLabel = outputSuffix === '-Full' ? '🍷 Full (ZaDark)'
      : outputSuffix === '-PlainFull' ? '🍷 Full'
      : outputSuffix === '-ZaDark' ? '🎨 ZaDark'
      : '📦 Original';
    const builtByTarget = {};

    if (!artifactFiles.length) {
      logger.warn('Could not find any artifact path in build output');
    }
    for (const file of artifactFiles) {
      const fullPath = path.join(BASE_DIR, file);
      if (!fs.existsSync(fullPath)) {
        logger.warn(`Artifact file not found: ${file}`);
        continue;
      }
      const name = path.basename(file);
      const target = path.extname(name).slice(1).toLowerCase(); // appimage / deb / rpm / pacman
      const size = fs.statSync(fullPath).size;
      const sizeStr = size > 1024 * 1024
        ? `${Math.round(size / 1024 / 1024)}MB`
        : `${Math.round(size / 1024)}KB`;

      let fileSha256 = 'unknown';
      try {
        const sha256Output = execSync(`sha256sum "${fullPath}"`, { encoding: 'utf8' });
        fileSha256 = sha256Output.split(' ')[0];
      } catch (error) {
        logger.warn('Could not calculate SHA256');
      }

      logger.success(`Built ${name} (${sizeStr})`);
      logger.dim(`SHA256: ${fileSha256}`);

      builtFiles.push({ type: variantLabel, name, sizeStr });
      builtByTarget[target] = { file, name };
    }

    // Export build info to GitHub Actions (one pair of outputs per target).
    if (process.env.GITHUB_OUTPUT) {
      const prefix = outputSuffix === '-PlainFull' ? 'plainfull_' : outputSuffix === '-Full' ? 'full_' : outputSuffix === '-ZaDark' ? 'zadark_' : 'original_';
      for (const [target, info] of Object.entries(builtByTarget)) {
        fs.appendFileSync(process.env.GITHUB_OUTPUT, `${prefix}${target}_file=${info.file}\n${prefix}${target}_name=${info.name}\n`);
      }
      logger.dim(`Exported ${prefix.replace('_', '')} build info to GitHub Actions (${Object.keys(builtByTarget).length} targets)`);
    }
  } catch (error) {
    logger.error('Build failed:', error.message);
    if (error.stdout) logger.dim('STDOUT:', error.stdout.toString());
    if (error.stderr) logger.dim('STDERR:', error.stderr.toString());
    process.exit(1);
  }
}

if (require.main === module) {
  main();
}

module.exports = { main, bundleWineRuntime, bundleGstRuntime, packageGstAsset };