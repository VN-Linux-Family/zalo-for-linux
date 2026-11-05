const { execSync } = require('child_process');
const path = require('path');
const BASE_DIR = path.resolve(__dirname, '../..');

// Return the commit based on the last one that changed anything but exclude files in workflow.
// Needs the history (actions/checkout with fetch-depth: 0).
function buildCommit() {
  const excludePatterns = [
    '*.md',
    '**/*.md',
    '.gitignore',
    '**/LICENSE',
    '**/LICENSE*',
    'docs/**',
    '.github/**',
    'flake.nix',
    'flake.lock',
    'nix/**',
    '.all-contributorsrc',
    'scripts/utils/build-commit.js'
  ];

  const excludeArgs = excludePatterns
    .map((pattern) => `':(exclude)${pattern}'`)
    .join(' ');

  return execSync(`git log -1 --format=%h -- . ${excludeArgs}`, {
    cwd: BASE_DIR,
    encoding: 'utf8'
  }).trim();
}

if (require.main === module) {
  console.log(buildCommit());
}

module.exports = { buildCommit };
