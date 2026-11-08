const { execSync, execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");
const logger = require("./utils/logger");
const { buildCommit } = require("./utils/build-commit");

const { ROOT: BASE_DIR, APP_DIR, DIST_DIR, ZADARK_VENDOR_DIR } = require('./utils/paths');
// AppDir left behind by stage 2 of the Original build (`--appimage-extract`).
// Phase 2 mutates it in place and repacks, instead of running electron-builder
// a second time.
const ORIGINAL_APPDIR = path.join(DIST_DIR, "squashfs-root");

// Upstream repository that hosts the release assets. GitHub Actions sets
// GITHUB_REPOSITORY; the hardcoded value keeps local builds identical.
const RELEASE_REPO =
  process.env.GITHUB_REPOSITORY || "VN-Linux-Family/zalo-for-linux";

let ZALO_VERSION = null;
const builtFiles = [];

// Name the AppImage toolchain expects from `uname -m`, not Node's process.arch.
function toolchainArch() {
  switch (process.arch) {
    case "x64":
      return "x86_64";
    case "arm64":
      return "aarch64";
    case "ia32":
      return "i686";
    default:
      return process.arch;
  }
}

// zsync "update information" embedded in the AppImage. The glob must match the
// artifact names we produce, so it is built here (single source of truth)
// instead of being re-derived by build-stage2.sh.
function zsyncUpdateInfo(variant, archSuffix, commitHash) {
  const [owner, repo] = RELEASE_REPO.split("/");
  const variantSuffix =
    variant === "zadark" ? "+ZaDark-[0-9]*[0-9]" : "-Original";
  const commitGlob = "?".repeat(commitHash.length);
  return `gh-releases-zsync|${owner}|${repo}|latest|Zalo-[0-9]*[0-9]${variantSuffix}-${commitGlob}${archSuffix}.AppImage.zsync`;
}

async function main() {
  try {
    // Read version from package.json.bak
    const packageJsonBakPath = path.join(APP_DIR, "package.json.bak");
    if (fs.existsSync(packageJsonBakPath)) {
      const packageJson = JSON.parse(
        fs.readFileSync(packageJsonBakPath, "utf8"),
      );
      ZALO_VERSION = packageJson.version;
      logger.info("Zalo version from package.json.bak:", ZALO_VERSION);

      // Export global outputs for workflow
      if (process.env.GITHUB_OUTPUT) {
        fs.appendFileSync(
          process.env.GITHUB_OUTPUT,
          `zalo_version=${ZALO_VERSION}\n`,
        );
      }
    } else {
      logger.warn("package.json.bak not found, version will be unknown");
    }

    // Calls run on the native engine (zcall-native/, shipped by extraFiles) in
    // both variants: no Wine runtime, no Full variants.

    // Phase 1: Build original Zalo. Stage 2 keeps dist/squashfs-root behind so
    // Phase 2 can derive the ZaDark variant from it.
    logger.step("PHASE 1: Building Zalo (Original)");
    await build("(Original)", "", { keepAppDir: true });

    // Phase 2: Derive the ZaDark variant from the Original AppDir. No second
    // electron-builder run: only app/ is modified and the same AppDir repacked.
    logger.step("PHASE 2: Building Zalo (with ZaDark)");

    await integrateZaDark(ORIGINAL_APPDIR);
    await build("(with ZaDark)", "-ZaDark", { reuseAppDir: ORIGINAL_APPDIR });

    // Final summary
    logger.step("BUILD SUMMARY");
    if (builtFiles.length > 0) {
      builtFiles.forEach(({ type, name, sizeStr }) => {
        logger.info(`${type} • ${name} (${sizeStr})`);
      });
    } else {
      logger.warn("No AppImage files were built in this run");
    }
  } catch (error) {
    logger.error("Main workflow failed:", error.message);
    process.exit(1);
  }
}

function jsFiles(dir) {
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const p = path.join(dir, e.name);
    return e.isDirectory() ? jsFiles(p) : p.endsWith(".js") ? [p] : [];
  });
}

// electron-builder's extraFiles filter decides what zcall-native/ ships. The
// engine dies at startup if a file it requires was filtered out, so check the
// packaged copy: every relative require resolves, and audio-io.py's opus_play.
function checkEnginePackaged(dir) {
  const missing = [];
  if (!fs.existsSync(dir))
    throw new Error("call engine missing from the package: " + dir);
  for (const file of jsFiles(dir)) {
    const src = fs.readFileSync(file, "utf8");
    for (const m of src.matchAll(/require\((['"])(\.{1,2}\/[^'"]+)\1\)/g)) {
      try {
        require.resolve(path.resolve(path.dirname(file), m[2]));
      } catch (_) {
        missing.push(`${path.relative(dir, file)} requires ${m[2]}`);
      }
    }
  }
  if (!fs.existsSync(path.join(dir, "tools", "opus_play.py")))
    missing.push("audio-io.py imports tools/opus_play.py");
  if (missing.length)
    throw new Error(
      "call engine incomplete in the package: " + missing.join("; "),
    );
}

// `zaloRoot` is the directory that contains Zalo's `app/` tree: the extracted
// AppDir in the build-once flow (dist/squashfs-root), or BASE_DIR for the
// legacy source-tree flow. ZaDark's helpers all take this root and operate on
// `<root>/app`, so no submodule change is needed.
async function integrateZaDark(zaloRoot = BASE_DIR) {
  logger.info("Applying ZaDark patches...");
  const targetAppDir = path.join(zaloRoot, "app");

  try {
    // Verify ZaDark module is available
    const zadarkModulePath = path.join(
      ZADARK_VENDOR_DIR,
      "build",
      "pc",
      "zadark-pc.js",
    );
    if (!fs.existsSync(zadarkModulePath)) {
      throw new Error(
        'ZaDark PC module not found - run "npm run prepare-zadark" first',
      );
    }

    const zadarkPC = require(zadarkModulePath);
    zadarkPC.copyZaDarkAssets(zaloRoot);
    zadarkPC.writeIndexFile(zaloRoot);
    zadarkPC.writeBootstrapFile(zaloRoot);
    zadarkPC.writePopupViewerFile(zaloRoot);

    // Route ZaDark translate requests to Google Translate (api.zadark.com is dead).
    await require("./patches/patch-zadark-translate").main(targetAppDir);


    // Zalo's dark theme strips ZaDark's CSS/scripts from the page (#66).
    await require("./patches/patch-zadark-keep").main(targetAppDir);
    logger.success("ZaDark patches applied successfully");
    return true;
  } catch (error) {
    logger.error("ZaDark integration failed:", error.message);
    logger.info(
      "Continuing with the current AppDir; the ZaDark artifact may be incomplete.",
    );
    return false;
  }
}

function writeBuildInfo(appDir, buildInfo) {
  const buildInfoPath = path.join(appDir, "pc-dist", "build-info.json");
  if (fs.existsSync(path.join(appDir, "pc-dist"))) {
    fs.writeFileSync(buildInfoPath, JSON.stringify(buildInfo, null, 2), "utf8");
    logger.dim(`Wrote metadata: ${buildInfoPath}`);
  } else {
    logger.warn("pc-dist directory not found, skipping build-info.json");
  }
}

async function build(buildName = "", outputSuffix = "", options = {}) {
  // Approach C: when `reuseAppDir` is set, electron-builder is skipped entirely
  // and this variant is packed from an AppDir that already exists (the one the
  // Original stage 2 left behind, mutated in place).
  const reuseAppDir = options.reuseAppDir || null;
  const keepAppDir = Boolean(options.keepAppDir);

  try {
    // Get git commit hash for filename
    const commitHash = buildCommit();
    const St2script = path.join(BASE_DIR, "scripts", "build-stage2.sh");
    const variant = outputSuffix === "-ZaDark" ? "zadark" : "original";

    // Add the arch suffix for builds
    const arch = toolchainArch();
    const archSuffix = arch === "aarch64" ? "-aarch64" : "-x86_64";

    // Set artifact name and build command based on build type
    let artifactName;
    let buildCommand;
    let zadarkVersion = null;
    let versionTag;

    if (outputSuffix === "-ZaDark") {
      // Read ZaDark version for custom naming
      const zadarkPackagePath = path.join(
        ZADARK_VENDOR_DIR,
        "package.json",
      );
      zadarkVersion = "unknown";

      if (fs.existsSync(zadarkPackagePath)) {
        try {
          const zadarkPackage = JSON.parse(
            fs.readFileSync(zadarkPackagePath, "utf8"),
          );
          zadarkVersion = zadarkPackage.version;
        } catch (error) {
          logger.warn('Could not read ZaDark version, using "unknown"');
        }
      }
      versionTag = `${ZALO_VERSION}+zadark${zadarkVersion}-${commitHash}`;

      artifactName = `Zalo-${ZALO_VERSION}+ZaDark-${zadarkVersion}-${commitHash}${archSuffix}.AppImage`;
      buildCommand = `npx electron-builder --linux --config.linux.artifactName="${artifactName}" -c.extraMetadata.version=${ZALO_VERSION} --publish=never`;

      logger.info(
        `Building ${buildName} with Zalo: ${ZALO_VERSION}, ZaDark: ${zadarkVersion}, Commit: ${commitHash}`,
      );
    } else {
      versionTag = `${ZALO_VERSION}-${commitHash}`;
      artifactName = `Zalo-${ZALO_VERSION}-Original-${commitHash}${archSuffix}.AppImage`;
      buildCommand = `npx electron-builder --linux --config.linux.artifactName="${artifactName}" -c.extraMetadata.version=${ZALO_VERSION} --publish=never`;

      logger.info(
        `Building ${buildName} with Zalo: ${ZALO_VERSION}, Commit: ${commitHash}`,
      );
    }

    const stage2Args = [versionTag, artifactName, DIST_DIR];
    const upinfo = zsyncUpdateInfo(variant, archSuffix, commitHash);

    // Write build-info.json into whichever app/ tree will be packaged: the
    // source tree for a normal build, or the reused AppDir for the ZaDark one.
    const appDirToPack = reuseAppDir ? path.join(reuseAppDir, "app") : APP_DIR;
    const buildInfo = {
      version: ZALO_VERSION,
      zadarkVersion: outputSuffix === "-ZaDark" ? zadarkVersion : null,
      commit: commitHash,
      buildDate: new Date().toISOString(),
    };

    if (reuseAppDir) {
      if (!fs.existsSync(reuseAppDir) || !fs.existsSync(appDirToPack)) {
        throw new Error(
          `AppDir to reuse not found: ${reuseAppDir} (Original stage 2 must keep it)`,
        );
      }
      logger.dim(`Reusing AppDir: ${reuseAppDir} (no electron-builder run)`);
      writeBuildInfo(appDirToPack, buildInfo);
      checkEnginePackaged(path.join(reuseAppDir, "zcall-native"));
      logger.success("call engine files are all in the package");
    } else {
      writeBuildInfo(appDirToPack, buildInfo);

      logger.dim(`Command: ${buildCommand}`);

      // Capture build output to get file information. Between the two stages,
      // check that the unpacked app carries the whole call engine.
      execSync(buildCommand, {
        stdio: "pipe",
        cwd: path.join(BASE_DIR),
        encoding: "utf8",
      });

      const unpacked = fs
        .readdirSync(DIST_DIR)
        .filter((d) => /^linux(-arm64)?-unpacked$/.test(d));
      if (!unpacked.length)
        throw new Error(
          "electron-builder output (linux-unpacked) not found in dist/",
        );
      for (const d of unpacked)
        checkEnginePackaged(path.join(DIST_DIR, d, "zcall-native"));
      logger.success("call engine files are all in the package");
    }

    logger.dim(`Command (Stage 2): ${St2script} ${stage2Args.join(" ")}`);
    logger.dim(
      `Stage 2 metadata: variant=${variant} arch=${arch} archSuffix=${archSuffix}`,
    );
    logger.dim(`Stage 2 UPINFO: ${upinfo}`);

    fs.chmodSync(St2script, 0o755);
    execFileSync(St2script, stage2Args, {
      stdio: "pipe",
      cwd: path.join(BASE_DIR),
      encoding: "utf8",
      env: {
        ...process.env,
        VARIANT: variant,
        ARCH: arch,
        ARCH_SUFFIX: archSuffix,
        UPINFO: upinfo,
        REUSE_APPDIR: reuseAppDir ? "1" : "0",
        KEEP_APPDIR: keepAppDir ? "1" : "0",
      },
    });

    const appImageFile = path.relative(
      BASE_DIR,
      path.join(DIST_DIR, artifactName),
    );
    const appImageName = artifactName;

    if (fs.existsSync(path.join(BASE_DIR, appImageFile))) {
      const fullPath = path.join(BASE_DIR, appImageFile);
      const size = fs.statSync(fullPath).size;
      const sizeStr =
        size > 1024 * 1024
          ? `${Math.round(size / 1024 / 1024)}MB`
          : `${Math.round(size / 1024)}KB`;

      // Calculate SHA256 for logging
      let fileSha256 = "unknown";
      try {
        const sha256Output = execSync(`sha256sum "${fullPath}"`, {
          encoding: "utf8",
        });
        fileSha256 = sha256Output.split(" ")[0];
      } catch (error) {
        logger.warn("Could not calculate SHA256");
      }

      logger.success(`Built ${appImageName} (${sizeStr})`);
      logger.dim(`SHA256: ${fileSha256}`);

      builtFiles.push({
        type: outputSuffix === "-ZaDark" ? "🎨 ZaDark" : "📦 Original",
        name: appImageName,
        sizeStr,
      });
    } else {
      logger.warn(`AppImage file not found: ${appImageFile}`);
    }

    // Export build info to GitHub Actions
    if (process.env.GITHUB_OUTPUT) {
      const prefix = outputSuffix === "-ZaDark" ? "zadark_" : "original_";

      // Export build-specific info
      const specificOutputs = [
        `${prefix}appimage_file=${appImageFile || ""}`,
        `${prefix}appimage_name=${appImageName || ""}`,
      ];

      specificOutputs.forEach((output) => {
        fs.appendFileSync(process.env.GITHUB_OUTPUT, output + "\n");
      });

      logger.dim(
        `Exported ${prefix.replace("_", "")} build info to GitHub Actions`,
      );
    }
  } catch (error) {
    logger.error("Build failed:", error.message);
    if (error.stdout) logger.dim("STDOUT:", error.stdout.toString());
    if (error.stderr) logger.dim("STDERR:", error.stderr.toString());
    process.exit(1);
  }
}

if (require.main === module) {
  main();
}

module.exports = { main };
