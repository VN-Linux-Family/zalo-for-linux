'use strict';

const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');
const { APP_NATIVE_LIBS } = require('../utils/paths');

const V8_PROFILES_DIR = path.join(APP_NATIVE_LIBS, 'v8-profiles');
const INDEX_JS = path.join(V8_PROFILES_DIR, 'index.js');

const MOCK_INDEX_CONTENT = `/**
 * Mock v8-profiles implementation for Zalo on Linux.
 *
 * In official macOS/Windows releases, Zalo ships a legacy v8-profiler C++ addon
 * ('profiler_electron1.8_mac.node') used only for internal telemetry / debugging.
 * On Linux, this C++ addon is not used by Zalo business logic and is replaced
 * by this zero-overhead, memory-safe JavaScript mock with full API contract parity.
 */
'use strict';

function CpuProfile(title) {
  this.typeId = 'CPU';
  this.uid = 1;
  this.title = title || '';
  this.startTime = Date.now();
  this.endTime = Date.now();
  this.samples = [];
  this.timestamps = [];
  this.head = {
    functionName: '(root)',
    url: '',
    scriptId: 0,
    lineNumber: 0,
    callUID: 1,
    bailoutReason: 'no reason',
    hitCount: 0,
    children: []
  };
}

CpuProfile.prototype.getHeader = function() {
  return {
    typeId: this.typeId,
    uid: this.uid,
    title: this.title
  };
};

CpuProfile.prototype.delete = function() {};

const profiles = {};
const activeProfiles = [];

const profiler = {
  get profiles() {
    return profiles;
  },
  startProfiling: function(name, recsamples) {
    name = '' + (name || '');
    if (activeProfiles.indexOf(name) < 0) {
      activeProfiles.push(name);
    }
  },
  stopProfiling: function(name) {
    name = '' + (name || '');
    const idx = activeProfiles.indexOf(name);
    if (idx >= 0) {
      activeProfiles.splice(idx, 1);
    }
    const profile = new CpuProfile(name);
    profiles[name] = profile;
    return profile;
  },
  setSamplingInterval: function(num) {},
  deleteAllProfiles: function() {
    for (const key of Object.keys(profiles)) {
      delete profiles[key];
    }
  }
};

module.exports = profiler;
process.profiler = profiler;
`;

async function main() {
 logger.info('Installing v8-profiles JS mock stub...');

 fs.ensureDirSync(V8_PROFILES_DIR);

 // Clean any legacy .node binaries that might be left in v8-profiles directory
 if (fs.existsSync(V8_PROFILES_DIR)) {
  const files = fs.readdirSync(V8_PROFILES_DIR);
  for (const f of files) {
   if (f.endsWith('.node')) {
    fs.removeSync(path.join(V8_PROFILES_DIR, f));
   }
  }
 }

 fs.writeFileSync(INDEX_JS, MOCK_INDEX_CONTENT, 'utf8');

 // Verify stub loads under standard Node.js runtime without errors
 delete require.cache[require.resolve(INDEX_JS)];
 const stub = require(INDEX_JS);
 if (typeof stub.startProfiling !== 'function' || typeof stub.stopProfiling !== 'function') {
  throw new Error('v8-profiles stub failed to export expected profiling interface');
 }

 logger.success('v8-profiles JS mock stub installed');
}

if (require.main === module) {
 main().catch(e => {
  logger.error(e.message);
  process.exit(1);
 });
}

module.exports = { main };
