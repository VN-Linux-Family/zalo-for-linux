#!/usr/bin/env node
'use strict';

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const BASE_DIR = path.resolve(__dirname, '../..');
const TEMPLATE_PATH = path.join(BASE_DIR, '.github/release-template.md');
const DEFAULT_OUTPUT = path.join(BASE_DIR, 'dist/release-body.md');

function run(cmd, opts = {}) {
  try {
    return execSync(cmd, {
      cwd: BASE_DIR,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      ...opts,
    }).trim();
  } catch {
    return '';
  }
}

function resolveRepo() {
  if (process.env.GITHUB_REPOSITORY) {
    return process.env.GITHUB_REPOSITORY;
  }
  const remoteUrl = run('git config --get remote.upstream.url') || run('git config --get remote.origin.url');
  const match = remoteUrl.match(/github\.com[/:]([^/]+\/[^/.]+)/);
  return match ? match[1] : 'VN-Linux-Family/zalo-for-linux';
}

function findPreviousTag(tags, currentTag) {
  if (!Array.isArray(tags) || tags.length === 0) return '';
  const idx = tags.indexOf(currentTag);
  if (idx !== -1 && idx + 1 < tags.length) {
    return tags[idx + 1];
  }
  return tags.find((t) => t !== currentTag) || '';
}

function getPreviousTag(currentTag) {
  let rawTags = run('git tag --sort=-creatordate');
  if (!rawTags) {
    // If shallow clone, attempt fetch tags once
    run('git fetch --tags --depth=50 origin 2>/dev/null || git fetch --tags --depth=50 upstream 2>/dev/null || true');
    rawTags = run('git tag --sort=-creatordate');
  }
  if (!rawTags) return '';
  const tags = rawTags
    .split('\n')
    .map((t) => t.trim())
    .filter(Boolean);

  return findPreviousTag(tags, currentTag);
}

function getPriorAuthors(previousTag) {
  const priorSet = new Set();
  if (!previousTag) return priorSet;

  const raw = run(`git log ${previousTag} --pretty=format:"%an|%ae"`);
  for (const line of raw.split('\n')) {
    const [name, email] = line.split('|');
    if (name) priorSet.add(name.toLowerCase().trim());
    if (email) priorSet.add(email.toLowerCase().trim());
  }
  return priorSet;
}

function parseGitLog(lines, priorAuthors = new Set(), repo = 'VN-Linux-Family/zalo-for-linux', currentTag = 'latest', previousTag = '') {
  const categories = [
    { key: 'feat', title: 'Features', items: [], match: (s) => /^feat(\(.*\))?!?:/i.test(s) || /feature/i.test(s) },
    { key: 'fix', title: 'Fixes', items: [], match: (s) => /^fix(\(.*\))?!?:/i.test(s) || /bugfix/i.test(s) },
    { key: 'perf', title: 'Performance', items: [], match: (s) => /^perf(\(.*\))?!?:/i.test(s) },
    { key: 'refactor', title: 'Refactor', items: [], match: (s) => /^refactor(\(.*\))?!?:/i.test(s) },
    { key: 'docs', title: 'Documentation', items: [], match: (s) => /^docs(\(.*\))?!?:/i.test(s) || /readme/i.test(s) },
    { key: 'ci', title: 'CI', items: [], match: (s) => /^(ci|build)(\(.*\))?!?:/i.test(s) },
    { key: 'other', title: 'Maintenance', items: [], match: () => true },
  ];

  const allContributors = new Map(); // authorLogin -> { isNew, firstPr }

  for (const line of lines) {
    const parts = line.split('|');
    if (parts.length < 2) continue;
    const subject = parts[0];
    const hash = parts[1] || '';
    const mergerName = parts[2] || '';
    const bodyText = parts[3] || '';

    // Ignore bot automated maintenance commits
    if (subject.startsWith('chore(nix):')) continue;

    let prNumber = null;
    let prAuthor = mergerName;
    let title = subject;

    // 1. Detect merge commits from PRs: e.g. "Merge pull request #184 from user/branch"
    const mergePrMatch = subject.match(/^Merge pull request #(\d+)(?:\s+from\s+([^/\s]+)\/[^\s]+)?/i);
    if (mergePrMatch) {
      prNumber = mergePrMatch[1];
      prAuthor = mergePrMatch[2] || mergerName;

      // Extract PR title from commit body if available
      const bodyCandidate = bodyText
        ? bodyText.split('\\n').map((l) => l.trim()).find((l) => l.length > 0 && !l.startsWith('Merge pull request') && !l.startsWith('Co-authored-by:'))
        : null;

      if (bodyCandidate) {
        title = bodyCandidate;
      } else if (hash) {
        const bodyFromGit = run(`git log -1 --format=%b ${hash}`)
          .split('\n')
          .map((l) => l.trim())
          .find((l) => l.length > 0 && !l.startsWith('Merge pull request') && !l.startsWith('Co-authored-by:'));
        if (bodyFromGit) title = bodyFromGit;
      }
    } else {
      // 2. Detect squash commits with PR trailer: "feat: something (#184)"
      const squashMatch = subject.match(/^(.*?)\s*\(#(\d+)\)$/);
      if (squashMatch) {
        title = squashMatch[1].trim();
        prNumber = squashMatch[2];
      }
    }

    if (!prNumber) continue;

    const entry = `* ${title} by @${prAuthor} in https://github.com/${repo}/pull/${prNumber}`;
    const targetCategory = categories.find((c) => c.match(title)) || categories[categories.length - 1];
    targetCategory.items.push(entry);

    // Track all contributors (both new and recurring)
    if (prAuthor && prAuthor !== 'github-actions[bot]' && prAuthor !== 'VN-Linux-Family') {
      const isNew = !priorAuthors.has(prAuthor.toLowerCase().trim());
      if (!allContributors.has(prAuthor)) {
        allContributors.set(prAuthor, { isNew, prNumber });
      }
    }
  }

  let changelog = "### What's Changed\n\n";

  for (const cat of categories) {
    if (cat.items.length > 0) {
      changelog += `#### ${cat.title}\n\n${cat.items.reverse().join('\n')}\n\n`;
    }
  }

  if (allContributors.size > 0) {
    changelog += '#### Contributors\n\n';
    const sortedAuthors = Array.from(allContributors.keys()).sort((a, b) =>
      a.localeCompare(b, undefined, { sensitivity: 'base' })
    );
    for (const author of sortedAuthors) {
      const info = allContributors.get(author);
      const note = info.isNew ? ' (first contribution)' : '';
      changelog += `* @${author}${note}\n`;
    }
    changelog += '\n';
  }

  if (previousTag) {
    changelog += `**Full Changelog**: https://github.com/${repo}/compare/${previousTag}...${currentTag}`;
  }

  return changelog.trim();
}

function generateChangelog(repo, currentTag, previousTag) {
  const range = previousTag ? `${previousTag}..HEAD` : 'HEAD';
  const rawLog = run(`git log --reverse ${range} --pretty=format:"%s|%h|%an|%ae"`);
  if (!rawLog) {
    return previousTag
      ? `### What's Changed\n\n* Maintenance and upstream updates.\n\n**Full Changelog**: https://github.com/${repo}/compare/${previousTag}...${currentTag}`
      : `### What's Changed\n\n* Initial release.\n`;
  }

  const priorAuthors = getPriorAuthors(previousTag);
  const lines = rawLog.split('\n').filter(Boolean);
  return parseGitLog(lines, priorAuthors, repo, currentTag, previousTag);
}

function generateReleaseBody(currentTag, outputPath = DEFAULT_OUTPUT) {
  const repo = resolveRepo();
  const previousTag = getPreviousTag(currentTag);

  let template = '';
  if (fs.existsSync(TEMPLATE_PATH)) {
    template = fs.readFileSync(TEMPLATE_PATH, 'utf8').trim();
  }

  const changelog = generateChangelog(repo, currentTag, previousTag);
  const finalBody = template ? `${template}\n\n---\n\n${changelog}\n` : `${changelog}\n`;

  const outDir = path.dirname(outputPath);
  if (!fs.existsSync(outDir)) {
    fs.mkdirSync(outDir, { recursive: true });
  }

  fs.writeFileSync(outputPath, finalBody, 'utf8');
  console.log(`Generated release notes for ${currentTag} (diff against ${previousTag || 'initial'}) -> ${outputPath}`);
  return finalBody;
}

if (require.main === module) {
  const tag = process.argv[2] || process.env.RELEASE_TAG || '26.11.10';
  const out = process.argv[3] || DEFAULT_OUTPUT;
  generateReleaseBody(tag, out);
}

module.exports = {
  findPreviousTag,
  getPreviousTag,
  getPriorAuthors,
  parseGitLog,
  generateChangelog,
  generateReleaseBody,
};
