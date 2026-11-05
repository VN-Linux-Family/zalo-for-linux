/**
 * Build helper for nativelibs (for Rust-based addons).
 *
 * Usage:
 *   node nativelibs/builder-rust.js <addon-path>
 *
 * Examples:
 *   node nativelibs/builder-rust.js nativelibs/db-cross-v4
 *   node nativelibs/builder-rust.js ./path/to/addon
 */

const { execSync } = require('child_process');
const fs = require('fs-extra');
const path = require('path');
const logger = require('../scripts/utils/logger');

const PACKAGE_JSON = require(path.join(__dirname, '..', 'package.json'));
const ELECTRON_VERSION = PACKAGE_JSON.devDependencies.electron.replace(/^\^/, '');

const repoRoot = path.resolve(__dirname, '..');
const libDir = path.resolve(repoRoot, process.argv[2] || '');
if (libDir !== repoRoot && !libDir.startsWith(repoRoot + path.sep)) {
 throw new Error(`Lib dir must be inside the repository: ${libDir}`);
}

const cargoTomlPath = path.join(libDir, 'Cargo.toml');
if (!fs.existsSync(cargoTomlPath)) {
 throw new Error(`Cargo.toml not found in ${libDir}`);
}
const cargoToml = fs.readFileSync(cargoTomlPath, 'utf8');
const crateNameMatch = cargoToml.match(/name\s*=\s*"([^"]+)"/);
const crateName = crateNameMatch ? crateNameMatch[1] : path.basename(libDir);

const workspaceRelease = path.join(repoRoot, 'target', 'release');
const localRelease = path.join(libDir, 'target', 'release');

logger.dim(`Lib dir: ${libDir} (package: ${crateName})`);
logger.dim(`Electron: ${ELECTRON_VERSION}`);

// `openh264-sys2` compiles OpenH264's C++ through the `cc` crate, which
// inherits CFLAGS/CXXFLAGS. A distro enabling LTO turns those objects into
// slim-LTO members; cargo's final cdylib link does not pass `-flto`, so the
// linker never materialises them and the addon keeps undefined Wels* symbols
// (the module then fails to dlopen). Drop LTO flags for this build.
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

// Ensure local target/release also has the .node binary for backwards compatibility with any legacy paths
fs.ensureDirSync(localRelease);
fs.copyFileSync(workspaceNode, path.join(localRelease, nodeName));

logger.dim(`Binary: ${nodeName} (${(fs.statSync(workspaceNode).size / 1024).toFixed(1)} KB)`);