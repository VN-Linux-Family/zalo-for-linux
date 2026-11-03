const { execFileSync } = require('child_process');
const fs = require('fs');
const path = require('path');
const profiles = require('../packaging/linux-profiles.json');

function packagePlan({ profile, artifactStem, version, release, arch, config, unpackedDir, variant = 'original' }) {
  if (!['original', 'zadark'].includes(variant)) throw new Error(`Unknown variant: ${variant}`);
  const packageName = variant === 'zadark' ? 'zalo-for-linux-zadark' : 'zalo-for-linux';
  const otherPackage = variant === 'zadark' ? 'zalo-for-linux' : 'zalo-for-linux-zadark';
  const selected = profiles[profile];
  if (!selected) throw new Error(`Unknown Linux package profile: ${profile}`);
  if (!['x64', 'arm64'].includes(arch)) throw new Error(`Unsupported architecture: ${arch}`);
  if (!/^\d+$/.test(release)) throw new Error(`Invalid package release: ${release}`);
  const artifactName = `${artifactStem}-${profile}-${release}.${selected.extension}`;
  const buildConfig = structuredClone(config);
  buildConfig.publish = null; // Local packages do not configure an update repository.
  buildConfig.extraMetadata = { ...buildConfig.extraMetadata, version };
  buildConfig.buildNumber = release;
  buildConfig.linux.artifactName = artifactName;
  if (selected.depends) {
    buildConfig[selected.target] = {
      artifactName, depends: selected.depends,
      maintainer: 'VN Linux Family', packageName,
      afterRemove: 'packaging/after-remove.sh',
      fpm: ['--conflicts', otherPackage,
        ...(selected.target === 'rpm' ? ['--rpm-rpmbuild-define', '_build_id_links none'] : []),
        ...(selected.target === 'deb' ? ['--replaces', otherPackage] : [])],
      ...(selected.target === 'deb' ? { recommends: [] } : {}),
      ...(selected.target === 'pacman' ? { compression: 'xz' } : {})
    };
  }
  return {
    artifactName, config: buildConfig,
    args: ['electron-builder', '--linux', selected.target, `--${arch}`, '--prepackaged', unpackedDir, '--publish=never']
  };
}

function buildLinuxPackage(options) {
  const { baseDir } = options;
  const distDir = path.join(baseDir, 'dist');
  fs.mkdirSync(distDir, { recursive: true });
  const plan = packagePlan(options);
  const output = path.join(distDir, plan.artifactName);
  fs.rmSync(output, { force: true });
  const tempDir = fs.mkdtempSync(path.join(distDir, '.package-'));
  try {
    // FPM writes target-specific metadata into prepackaged/resources. Isolate
    // every target so RPM metadata cannot leak into DEB, Arch or the archive.
    const unpackedDir = path.join(tempDir, 'unpacked');
    fs.cpSync(options.unpackedDir, unpackedDir, { recursive: true, verbatimSymlinks: true, mode: fs.constants.COPYFILE_FICLONE });
    const isolated = packagePlan({ ...options, unpackedDir });
    const configPath = path.join(tempDir, 'electron-builder.json');
    fs.writeFileSync(configPath, JSON.stringify(isolated.config));
    execFileSync('npx', [...isolated.args, '--config', configPath], { cwd: baseDir, stdio: 'inherit' });
    if (!fs.existsSync(output)) throw new Error(`Package was not produced: ${output}`);
    return output;
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
}

module.exports = { buildLinuxPackage, packagePlan, profiles };
