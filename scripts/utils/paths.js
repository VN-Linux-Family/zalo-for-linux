'use strict';

const path = require('path');

const ROOT = path.resolve(__dirname, '..', '..');
const APP_DIR = path.join(ROOT, 'app');
const TEMP_DIR = path.join(ROOT, 'temp');
const DIST_DIR = path.join(ROOT, 'dist');
const PLUGINS_DIR = path.join(ROOT, 'plugins');
const NODE_MODULES_DIR = path.join(ROOT, 'node_modules');
const VENDOR_DIR = path.join(ROOT, 'vendor');
const ZADARK_VENDOR_DIR = path.join(VENDOR_DIR, 'zadark');

// ---------------------------------------------------------------------------
// Repository source locations (Phase 3 consolidated layout)
// ---------------------------------------------------------------------------
const NATIVE_DIR = path.join(ROOT, 'native');
const NATIVE_ADDONS_DIR = path.join(NATIVE_DIR, 'addons');
const NATIVE_ENGINES_DIR = path.join(NATIVE_DIR, 'engines');
const NATIVELIBS_DIR = NATIVE_ADDONS_DIR; // Backward-compatible alias
const ZOCR_DIR = path.join(NATIVE_ENGINES_DIR, 'zocr');
const ZCALL_DIR = path.join(NATIVE_ENGINES_DIR, 'zcall');
const SCRIPTS_DIR = path.join(ROOT, 'scripts');
const PATCHES_DIR = path.join(SCRIPTS_DIR, 'patches');

const BUILDER_SCRIPT = path.join(NATIVE_DIR, 'builder.js');
const BUILDER_RUST_SCRIPT = BUILDER_SCRIPT; // Unified builder handles both C++ and Rust
// ---------------------------------------------------------------------------
// Extracted Zalo application destinations (immutable runtime layout)
// ---------------------------------------------------------------------------
const APP_MAIN_DIST = path.join(APP_DIR, 'main-dist');
const APP_PC_DIST = path.join(APP_DIR, 'pc-dist');
const APP_NATIVE = path.join(APP_DIR, 'native');
const APP_NATIVE_LIBS = path.join(APP_NATIVE, 'nativelibs');
const APP_OCR_DIR = path.join(APP_NATIVE, 'ocr');
const APP_ZCALL_ASSETS = path.join(APP_DIR, 'zcall-assets');

module.exports = Object.freeze({
  ROOT,
  APP_DIR,
  TEMP_DIR,
  DIST_DIR,
  PLUGINS_DIR,
  NODE_MODULES_DIR,
  VENDOR_DIR,
  ZADARK_VENDOR_DIR,

  // Source paths
  NATIVE_DIR,
  NATIVE_ADDONS_DIR,
  NATIVE_ENGINES_DIR,
  NATIVELIBS_DIR,
  ZOCR_DIR,
  ZCALL_DIR,
  SCRIPTS_DIR,
  PATCHES_DIR,
  BUILDER_SCRIPT,
  BUILDER_RUST_SCRIPT,
  // Extracted app runtime layout
  APP_MAIN_DIST,
  APP_PC_DIST,
  APP_NATIVE,
  APP_NATIVE_LIBS,
  APP_OCR_DIR,
  APP_ZCALL_ASSETS,
});
