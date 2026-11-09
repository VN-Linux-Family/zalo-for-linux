'use strict';

const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('./utils/logger');
const { APP_DIR, TEMP_DIR } = require('./utils/paths');

async function main() {
  if (!fs.existsSync(TEMP_DIR)) {
    fs.mkdirSync(TEMP_DIR, { recursive: true });
  }

  // Clean up any existing extracted Zalo folders
  try {
    const zaloFolders = execSync(`find "${TEMP_DIR}" -name "Zalo*" -type d 2>/dev/null || true`, {
      cwd: TEMP_DIR,
      encoding: 'utf8',
      stdio: 'pipe'
    }).trim().split('\n').filter(Boolean);

    zaloFolders.forEach(folder => {
      if (fs.existsSync(folder)) {
        fs.rmSync(folder, { recursive: true, force: true });
        logger.dim(`Cleaned up folder: ${folder}`);
      }
    });
  } catch (_) { }

  if (fs.existsSync(APP_DIR)) {
    fs.rmSync(APP_DIR, { recursive: true, force: true });
  }

  await extractDMG();
  await extractWindows();
  await extractAppAsar();
}

async function extractDMG() {
  try {
    if (!fs.existsSync(TEMP_DIR)) {
      throw new Error(`Temp directory not found: ${TEMP_DIR}. Please run "npm run download" first.`);
    }

    let selectedFile;
    const explicitDmg = process.env.ZALO_DMG_PATH ? path.resolve(process.env.ZALO_DMG_PATH) : null;

    if (explicitDmg && fs.existsSync(explicitDmg)) {
      const stats = fs.statSync(explicitDmg);
      const name = path.basename(explicitDmg);
      selectedFile = {
        name,
        path: explicitDmg,
        version: parseVersion(name),
        versionStr: parseVersion(name)?.raw || 'custom',
        size: (stats.size / 1024 / 1024).toFixed(2),
        mtime: stats.mtime
      };
      logger.info(`Using explicit DMG from ZALO_DMG_PATH: ${selectedFile.name}`);
    } else {
      const files = fs.readdirSync(TEMP_DIR);
      const dmgFiles = files.filter(file => file.toLowerCase().endsWith('.dmg'));

      if (dmgFiles.length === 0) {
        logger.error('No DMG files found in:', TEMP_DIR);
        logger.info('Please run "npm run download" first or set ZALO_DMG_PATH.');
        throw new Error('No DMG files found');
      }

      const allFiles = dmgFiles.map(file => {
        const filePath = path.join(TEMP_DIR, file);
        const stats = fs.statSync(filePath);
        const version = parseVersion(file);

        return {
          name: file,
          path: filePath,
          version: version,
          versionStr: version ? version.raw : 'unknown',
          size: (stats.size / 1024 / 1024).toFixed(2),
          mtime: stats.mtime
        };
      });

      const sortedFiles = allFiles.sort((a, b) => {
        if (a.version && b.version) return compareVersions(b.version, a.version);
        if (a.version && !b.version) return -1;
        if (!a.version && b.version) return 1;
        return 0;
      });

      if (sortedFiles.length === 1) {
        selectedFile = sortedFiles[0];
        logger.info(`Auto-selecting DMG: ${selectedFile.name}`);
      } else if (process.env.ZALO_VERSION) {
        const requestedVersion = process.env.ZALO_VERSION.trim();
        const matchingFile = sortedFiles.find(file => file.version && file.version.raw === requestedVersion);

        if (matchingFile) {
          selectedFile = matchingFile;
          logger.info(`Auto-selecting version ${requestedVersion}: ${selectedFile.name}`);
        } else if (!process.stdin.isTTY) {
          selectedFile = sortedFiles[0];
          logger.warn(`Requested version ${requestedVersion} not found; falling back to newest: ${selectedFile.name}`);
        } else {
          selectedFile = await showInteractiveMenu(sortedFiles, 'DMG files');
        }
      } else if (!process.stdin.isTTY) {
        selectedFile = sortedFiles[0];
        logger.info(`Non-interactive environment detected; auto-selecting newest DMG: ${selectedFile.name}`);
      } else {
        selectedFile = await showInteractiveMenu(sortedFiles, 'DMG files');
      }
    }

    const dmgPath = selectedFile.path;

    if (!commandExists('7z')) {
      logger.error('Dependency missing: 7z is not installed. Run: sudo apt-get install p7zip-full');
      throw new Error('7z is required for DMG extraction.');
    }

    logger.info(`Extracting app.asar and the call helper from ${selectedFile.name}...`);
    const extractCommand = `7z x "${dmgPath}" "Zalo*/Zalo.app/Contents/Resources/app.asar*" "Zalo*/Zalo.app/Contents/ZaloHelper.app/Contents/MacOS/ZaloCall"`;

    try {
      execSync(extractCommand, { cwd: TEMP_DIR, stdio: 'pipe' });
    } catch (_) {
      logger.dim('Note: 7z reported warnings/errors (normal for DMG files)');
    }

    logger.success('DMG extraction completed successfully');
  } catch (error) {
    logger.error('DMG extraction failed:', error.message);
    throw error;
  }
}

async function extractWindows() {
  try {
    if (!fs.existsSync(TEMP_DIR)) {
      throw new Error(`Temp directory not found: ${TEMP_DIR}. Please run "npm run download" first.`);
    }

    let selectedFile;
    const explicitExe = process.env.ZALO_EXE_PATH ? path.resolve(process.env.ZALO_EXE_PATH) : null;

    if (explicitExe && fs.existsSync(explicitExe)) {
      const stats = fs.statSync(explicitExe);
      const name = path.basename(explicitExe);
      selectedFile = {
        name,
        path: explicitExe,
        version: parseVersion(name),
        versionStr: parseVersion(name)?.raw || 'custom',
        size: (stats.size / 1024 / 1024).toFixed(2),
        mtime: stats.mtime
      };
      logger.info(`Using explicit Windows installer from ZALO_EXE_PATH: ${selectedFile.name}`);
    } else {
      const files = fs.readdirSync(TEMP_DIR);
      const exeFiles = files.filter(file => /^ZaloSetup-[0-9.]+\.exe$/i.test(file));

      if (exeFiles.length === 0) {
        logger.warn('No Windows installers found in: ' + TEMP_DIR);
        logger.info('OCR functionality will be unavailable until a Windows installer is provided.');
        return;
      }

      const allFiles = exeFiles.map(file => {
        const filePath = path.join(TEMP_DIR, file);
        const stats = fs.statSync(filePath);
        const version = parseVersion(file);

        return {
          name: file,
          path: filePath,
          version: version,
          versionStr: version ? version.raw : 'unknown',
          size: (stats.size / 1024 / 1024).toFixed(2),
          mtime: stats.mtime
        };
      });

      const sortedFiles = allFiles.sort((a, b) => {
        if (a.version && b.version) return compareVersions(b.version, a.version);
        if (a.version && !b.version) return -1;
        if (!a.version && b.version) return 1;
        return 0;
      });

      if (sortedFiles.length === 1) {
        selectedFile = sortedFiles[0];
        logger.info(`Auto-selecting Windows installer: ${selectedFile.name}`);
      } else if (process.env.ZALO_WIN_VERSION) {
        const requestedVersion = process.env.ZALO_WIN_VERSION.trim();
        const matchingFile = sortedFiles.find(file => file.version && file.version.raw === requestedVersion);

        if (matchingFile) {
          selectedFile = matchingFile;
          logger.info(`Auto-selecting version ${requestedVersion}: ${selectedFile.name}`);
        } else if (!process.stdin.isTTY) {
          selectedFile = sortedFiles[0];
          logger.warn(`Requested version ${requestedVersion} not found; falling back to newest: ${selectedFile.name}`);
        } else {
          selectedFile = await showInteractiveMenu(sortedFiles, 'Windows installers');
        }
      } else if (!process.stdin.isTTY) {
        selectedFile = sortedFiles[0];
        logger.info(`Non-interactive environment detected; auto-selecting newest installer: ${selectedFile.name}`);
      } else {
        selectedFile = await showInteractiveMenu(sortedFiles, 'Windows installers');
      }
    }

    const version = selectedFile.versionStr;
    if (!selectedFile.version) {
      throw new Error('Could not parse Windows installer version: ' + selectedFile.name);
    }

    if (!commandExists('7z')) {
      logger.error('Dependency missing: 7z is not installed. Run: sudo apt-get install p7zip-full');
      throw new Error('7z is required for Windows installer extraction.');
    }

    const inner7z = path.join(TEMP_DIR, `Zalo-Win-${version}.7z`);
    if (!fs.existsSync(inner7z)) {
      logger.info(`Extracting installer payload from ${selectedFile.name}...`);
      execSync(`7z e -y "${selectedFile.path}" '$PLUGINSDIR/app-32.7z' -oZalo-Win-extract`, {
        cwd: TEMP_DIR,
        stdio: 'pipe'
      });
      fs.copyFileSync(path.join(TEMP_DIR, 'Zalo-Win-extract', 'app-32.7z'), inner7z);
    }

    const extractOut = path.join(TEMP_DIR, `Zalo-Win-${version}`);
    logger.info(`Extracting plugins from ${selectedFile.name}...`);
    execSync(`7z x -y "${inner7z}" 'Zalo-${version}/plugins/*' -o"${extractOut}"`, {
      cwd: TEMP_DIR,
      stdio: 'pipe'
    });
    process.env.ZALO_WIN_VERSION = version;
    logger.success('Windows plugins extracted successfully');
  } catch (error) {
    logger.warn('Windows extraction failed, OCR unavailable: ' + error.message);
  }
}

async function extractAppAsar() {
  const findResourcesCommand = `find "${TEMP_DIR}" -path "*/Zalo.app/Contents/Resources" -type d`;
  let resourcesPaths;

  try {
    const result = execSync(findResourcesCommand, { cwd: TEMP_DIR, encoding: 'utf8', stdio: 'pipe' });
    resourcesPaths = result.trim().split('\n').filter(Boolean);
  } catch (_) {
    resourcesPaths = [];
  }

  const resourcesPath = resourcesPaths[0];
  if (!resourcesPath || !fs.existsSync(resourcesPath)) {
    throw new Error('Could not locate extracted Zalo.app/Contents/Resources in temp directory');
  }

  logger.info(`Extracting app.asar to app directory: ${APP_DIR}...`);
  const asarModule = await import('@electron/asar');

  const originalCwd = process.cwd();
  try {
    process.chdir(resourcesPath);
    await asarModule.extractAll('app.asar', APP_DIR);
  } finally {
    process.chdir(originalCwd);
  }

  logger.success('App directory extracted');

  // Rename package.json to package.json.bak
  const packageJsonPath = path.join(APP_DIR, 'package.json');
  const packageJsonBakPath = path.join(APP_DIR, 'package.json.bak');
  if (fs.existsSync(packageJsonPath)) {
    fs.renameSync(packageJsonPath, packageJsonBakPath);
  }
}

function commandExists(command) {
  try {
    execSync(`command -v ${command}`, { stdio: 'ignore' });
    return true;
  } catch (_) {
    return false;
  }
}

function parseVersion(filename) {
  const versionMatch = filename.match(/(\d+)\.(\d+)\.(\d+)/);
  if (versionMatch) {
    return {
      major: parseInt(versionMatch[1]),
      minor: parseInt(versionMatch[2]),
      patch: parseInt(versionMatch[3]),
      raw: `${versionMatch[1]}.${versionMatch[2]}.${versionMatch[3]}`
    };
  }
  return null;
}

function compareVersions(v1, v2) {
  if (v1.major !== v2.major) return v1.major - v2.major;
  if (v1.minor !== v2.minor) return v1.minor - v2.minor;
  return v1.patch - v2.patch;
}

async function showInteractiveMenu(files, label = 'DMG files') {
  return new Promise((resolve, reject) => {
    let selectedIndex = 0;
    const maxIndex = files.length - 1;

    process.stdin.setRawMode(true);
    process.stdin.resume();
    process.stdin.setEncoding('utf8');

    function renderMenu() {
      process.stdout.write('\x1B[2J\x1B[0f');
      console.log(`📋 Available ${label}:`);
      console.log('   Use ↑↓ arrow keys to navigate, Enter to select, Esc to cancel\n');
      files.forEach((file, index) => {
        const isSelected = index === selectedIndex;
        const color = isSelected ? '\x1b[36m' : '\x1b[37m';
        console.log(`${color}  ${isSelected ? '●' : '○'} ${file.name}\x1b[0m`);
      });
    }

    function cleanup() {
      process.stdin.setRawMode(false);
      process.stdin.pause();
      process.stdin.removeAllListeners('data');
    }

    function handleKeypress(key) {
      if (key === '\u001b[A') selectedIndex = selectedIndex > 0 ? selectedIndex - 1 : maxIndex;
      else if (key === '\u001b[B') selectedIndex = selectedIndex < maxIndex ? selectedIndex + 1 : 0;
      else if (key === '\r' || key === '\n') { cleanup(); resolve(files[selectedIndex]); }
      else if (key === '\u001b' || key === '\u0003') { cleanup(); reject(new Error('Cancelled')); }
      renderMenu();
    }
    renderMenu();
    process.stdin.on('data', handleKeypress);
  });
}

if (require.main === module) {
  main().catch((err) => {
    logger.error('Extract failed:', err.message);
    process.exit(1);
  });
}

module.exports = {
  main,
  extractDMG,
  extractWindows,
  extractAppAsar,
  parseVersion,
  compareVersions,
};
