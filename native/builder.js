/**
 * Unified build helper for native addons (C++ node-gyp & Rust NAPI-RS).
 *
 * Usage:
 *   node native/builder.js <addon-path>
 *
 * Examples:
 *   node native/builder.js native/addons/db-cross-v4
 *   node native/builder.js native/addons/zwalker
 */

'use strict';

const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../scripts/utils/logger');
const { ROOT: repoRoot } = require('../scripts/utils/paths');

const PACKAGE_JSON = require(path.join(repoRoot, 'package.json'));
const ELECTRON_VERSION = PACKAGE_JSON.devDependencies.electron.replace(/^\^/, '');

const libDir = path.resolve(repoRoot, process.argv[2] || '');
if (libDir !== repoRoot && !libDir.startsWith(repoRoot + path.sep)) {
 throw new Error(`Lib dir must be inside the repository: ${libDir}`);
}

const cargoTomlPath = path.join(libDir, 'Cargo.toml');
const bindingGypPath = path.join(libDir, 'binding.gyp');

const isRust = fs.existsSync(cargoTomlPath);
const isGyp = fs.existsSync(bindingGypPath);

if (!isRust && !isGyp) {
 throw new Error(`Neither Cargo.toml nor binding.gyp found in ${libDir}`);
}

if (isGyp) {
 buildGypAddon();
} else if (isRust) {
 buildRustAddon();
}

function buildGypAddon() {
 const arch = process.arch === 'arm64' ? 'arm64' : 'x64';
 const buildDir = path.join(libDir, 'build');
 const releaseBinary = path.join(buildDir, 'Release');

 logger.dim(`Lib dir: ${libDir} (toolchain: C++ / node-gyp)`);
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
}

function buildRustAddon() {
 const cargoToml = fs.readFileSync(cargoTomlPath, 'utf8');
 const crateNameMatch = cargoToml.match(/name\s*=\s*"([^"]+)"/);
 const crateName = crateNameMatch ? crateNameMatch[1] : path.basename(libDir);

 const workspaceRelease = path.join(repoRoot, 'target', 'release');
 const localRelease = path.join(libDir, 'target', 'release');

 logger.dim(`Lib dir: ${libDir} (package: ${crateName}, toolchain: Rust / cargo)`);
 logger.dim(`Electron: ${ELECTRON_VERSION}`);

 // openh264-sys2 compiles OpenH264's C++ through the cc crate, which inherits
 // CFLAGS/CXXFLAGS. A distro enabling LTO turns those objects into slim-LTO members;
 // cargo's final cdylib link does not pass -flto, so the linker never materialises
 // them and the addon keeps undefined Wels* symbols. Drop LTO flags for this build.
 const env = { ...process.env };
 for (const key of ['CFLAGS', 'CXXFLAGS', 'LDFLAGS']) {
  if (env[key]) env[key] = env[key].replace(/\s*-flto(=\S+)?/g, '');
 }

 execSync(
  `cargo build --release -p ${crateName}`,
  { cwd: repoRoot, stdio: 'inherit', env }
 );

 const soFile = `lib${crateName.replace(/-/g, '_')}.so`;
 const nodeName = `${crateName}.node`;
 const workspaceSo = path.join(workspaceRelease, soFile);
 const localSo = path.join(localRelease, soFile);

 const sourceSo = fs.existsSync(workspaceSo) ? workspaceSo : (fs.existsSync(localSo) ? localSo : null);
 if (!sourceSo) {
  throw new Error(`Build succeeded but binary ${soFile} not found in ${workspaceRelease} or ${localRelease}`);
 }

 const workspaceNode = path.join(workspaceRelease, nodeName);
 fs.copyFileSync(sourceSo, workspaceNode);

 // Ensure local target/release also has the .node binary for backwards compatibility
 fs.ensureDirSync(localRelease);
 fs.copyFileSync(workspaceNode, path.join(localRelease, nodeName));

 logger.dim(`Binary: ${nodeName} (${(fs.statSync(workspaceNode).size / 1024).toFixed(1)} KB)`);
}
