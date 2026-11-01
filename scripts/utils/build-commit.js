const { execSync } = require('child_process');
const path = require('path');
const BASE_DIR = path.resolve(__dirname, '../..');

// Return the commit based on the last one that changed anything but nix/sources.json.
// Needs the history (actions/checkout with fetch-depth: 0).
function buildCommit() {
  return execSync("git log -1 --format=%h -- . ':(exclude)nix/sources.json'", {
    cwd: BASE_DIR,
    encoding: 'utf8'
  }).trim();
}

if (require.main === module) {
  console.log(buildCommit());
}

module.exports = { buildCommit };
