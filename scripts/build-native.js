/**
 * scripts/build-native.js
 *
 * Standalone build orchestrator for all native modules:
 * - 11 N-API addons in native/addons/ (C++ node-gyp & Rust NAPI-RS)
 * - Standalone engines in native/engines/ (zcall/audio, zocr)
 *
 * Usage:
 *   node scripts/build-native.js           # Build all native modules
 *   node scripts/build-native.js <target>  # Build a specific addon or engine (e.g. zfile, zocr)
 */

'use strict';

const fs = require('fs');
const path = require('path');
const logger = require('./utils/logger');
const { NATIVE_ADDONS_DIR, NATIVE_ENGINES_DIR } = require('./utils/paths');
const { build } = require('../native/builder');

function getAvailableTargets() {
 const targets = [];

 // Addons in native/addons/
 if (fs.existsSync(NATIVE_ADDONS_DIR)) {
  const addonDirs = fs.readdirSync(NATIVE_ADDONS_DIR, { withFileTypes: true })
   .filter(dirent => dirent.isDirectory())
   .map(dirent => path.join(NATIVE_ADDONS_DIR, dirent.name))
   .filter(dir => fs.existsSync(path.join(dir, 'Cargo.toml')) || fs.existsSync(path.join(dir, 'binding.gyp')));

  for (const dir of addonDirs) {
   targets.push({
    name: path.basename(dir),
    category: 'addon',
    dir,
   });
  }
 }

 // Engines in native/engines/
 if (fs.existsSync(NATIVE_ENGINES_DIR)) {
  // zocr
  const zocrDir = path.join(NATIVE_ENGINES_DIR, 'zocr');
  if (fs.existsSync(path.join(zocrDir, 'Cargo.toml'))) {
   targets.push({
    name: 'zocr',
    category: 'engine',
    dir: zocrDir,
   });
  }

  // zcall audio engine
  const zcallAudioDir = path.join(NATIVE_ENGINES_DIR, 'zcall', 'audio');
  if (fs.existsSync(path.join(zcallAudioDir, 'Cargo.toml'))) {
   targets.push({
    name: 'zcall-audio',
    category: 'engine',
    dir: zcallAudioDir,
   });
  }
 }

 return targets;
}

async function main(filterArg = process.argv[2]) {
 if (filterArg === '--help' || filterArg === '-h') {
  console.log('Usage: node scripts/build-native.js [target-name]');
  console.log('\nAvailable targets:');
  for (const t of getAvailableTargets()) {
   console.log(`  - ${t.name} (${t.category}: ${path.relative(process.cwd(), t.dir)})`);
  }
  return;
 }

 const allTargets = getAvailableTargets();
 const targets = filterArg
  ? allTargets.filter(t => t.name.toLowerCase() === filterArg.toLowerCase() || t.name.toLowerCase().includes(filterArg.toLowerCase()))
  : allTargets;

 if (targets.length === 0) {
  logger.error(`No matching native target found for "${filterArg}".`);
  console.log('Available targets: ' + allTargets.map(t => t.name).join(', '));
  process.exit(1);
 }

 logger.step(`Building ${targets.length} native module(s)...`);
 const results = [];
 const startTime = Date.now();

 for (const target of targets) {
  const relPath = path.relative(process.cwd(), target.dir);
  logger.info(`[${target.category.toUpperCase()}] Compiling ${target.name} (${relPath})...`);
  const targetStart = Date.now();
  try {
   const outputPath = build(target.dir);
   const elapsed = ((Date.now() - targetStart) / 1000).toFixed(1);
   results.push({ name: target.name, success: true, elapsed, outputPath });
   logger.success(`Finished ${target.name} in ${elapsed}s`);
  } catch (error) {
   const elapsed = ((Date.now() - targetStart) / 1000).toFixed(1);
   results.push({ name: target.name, success: false, elapsed, error: error.message });
   logger.error(`Failed building ${target.name}: ${error.message}`);
  }
 }

 const totalElapsed = ((Date.now() - startTime) / 1000).toFixed(1);
 logger.step(`NATIVE BUILD SUMMARY (${totalElapsed}s)`);

 let hasError = false;
 for (const r of results) {
  if (r.success) {
   logger.success(`✓ ${r.name} (${r.elapsed}s)`);
  } else {
   hasError = true;
   logger.error(`✗ ${r.name} (${r.elapsed}s): ${r.error}`);
  }
 }

 if (hasError) {
  logger.error('One or more native builds failed.');
  process.exit(1);
 } else {
  logger.success(`All ${results.length} native target(s) built successfully.`);
 }
}

if (require.main === module) {
 main().catch(err => {
  logger.error('build-native failed:', err.message);
  process.exit(1);
 });
}

module.exports = { main, getAvailableTargets };
