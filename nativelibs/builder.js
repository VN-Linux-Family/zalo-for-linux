/**
 * Build helper for nativelibs.
 *
 * Usage:
 *   node nativelibs/builder.js <addon-path>
 *
 * Examples:
 *   node nativelibs/builder.js nativelibs/db-cross-v4
 *   node nativelibs/builder.js ./path/to/addon
 */

const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../scripts/utils/logger');
const arch = process.arch === 'arm64' ? 'arm64' : 'x64';

const { ROOT: repoRoot } = require('../scripts/utils/paths');
const PACKAGE_JSON = require(path.join(repoRoot, 'package.json'));
const ELECTRON_VERSION = PACKAGE_JSON.devDependencies.electron.replace(/^\^/, '');
const libDir = path.resolve(repoRoot, process.argv[2] || '');
if (libDir !== repoRoot && !libDir.startsWith(repoRoot + path.sep)) {
 throw new Error(`Lib dir must be inside the repository: ${libDir}`);
}
const buildDir = path.join(libDir, 'build');
const releaseBinary = path.join(buildDir, 'Release');

logger.dim(`Lib dir: ${libDir}`);
logger.dim(`Electron: ${ELECTRON_VERSION}`);


execSync(
 `npx node-gyp configure --target=${ELECTRON_VERSION} --arch=${arch} --dist-url=https://www.electronjs.org/headers build`,
 { cwd: libDir, stdio: 'ignore' }
);

const files = fs.readdirSync(releaseBinary).filter(f => f.endsWith('.node'));
if (files.length === 0) {
 throw new Error(`Build succeeded but binary not found in ${releaseBinary}`);
}

const binaryPath = path.join(releaseBinary, files[0]);
logger.dim(`Binary: ${files[0]} (${(fs.statSync(binaryPath).size / 1024).toFixed(1)} KB)`);