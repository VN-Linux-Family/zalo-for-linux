const { test } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { packagePlan, profiles } = require('./build-linux-packages');
const config = require('../package.json').build;

const base = { artifactStem: 'Zalo-26.1.0-Original-abc1234-x86_64', version: '26.1.0', release: '123', arch: 'x64', config, unpackedDir: '/workspace with spaces/unpacked' };

test('all distro profiles have valid electron-builder configurations for both architectures', async () => {
  const { validateConfiguration } = require('app-builder-lib/out/util/config/config');
  for (const profile of Object.keys(profiles)) {
    for (const variant of ['original', 'zadark']) for (const arch of ['x64', 'arm64']) {
      const plan = packagePlan({ ...base, profile, arch, variant });
      await validateConfiguration(plan.config, { isEnabled: false });
      assert.equal(plan.args[plan.args.indexOf('--prepackaged') + 1], base.unpackedDir);
      assert.equal(plan.config.publish, null);
      assert.ok(plan.artifactName.endsWith(profiles[profile].extension));
    }
  }
  assert.equal(config.extraMetadata, undefined); // Planning must not modify the shared config.
});

test('reject unsupported package profiles, architectures and release numbers', () => {
  assert.throws(() => packagePlan({ ...base, profile: 'alpine' }), /Unknown/);
  assert.throws(() => packagePlan({ ...base, profile: 'deb', arch: 'ia32' }), /Unsupported/);
  assert.throws(() => packagePlan({ ...base, profile: 'deb', release: 'abc-123' }), /Invalid/);
});

test('isolate target metadata and clean temporary copies on success and failure', (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-package-test-'));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const unpackedDir = path.join(root, 'original');
  fs.mkdirSync(path.join(unpackedDir, 'resources'), { recursive: true });
  fs.writeFileSync(path.join(unpackedDir, 'resources', 'marker'), 'original');
  const options = { ...base, baseDir: root, unpackedDir, profile: 'deb' };
  const output = path.join(root, 'dist', packagePlan(options).artifactName);
  fs.mkdirSync(path.dirname(output));
  const cp = require('child_process');
  const stub = t.mock.method(cp, 'execFileSync', () => {});
  delete require.cache[require.resolve('./build-linux-packages')];
  const { buildLinuxPackage } = require('./build-linux-packages');
  fs.writeFileSync(output, 'stale artifact');
  assert.throws(() => buildLinuxPackage(options), /Package was not produced/);
  assert.equal(fs.existsSync(output), false);
  stub.mock.mockImplementation(() => { throw new Error('builder failed'); });
  assert.throws(() => buildLinuxPackage(options), /builder failed/);
  assert.deepEqual(fs.readdirSync(path.join(root, 'dist')), []);
  stub.mock.mockImplementation((command, args) => {
    const copy = args[args.indexOf('--prepackaged') + 1];
    assert.notEqual(copy, unpackedDir);
    fs.writeFileSync(path.join(copy, 'resources', 'marker'), 'target metadata');
    fs.writeFileSync(output, 'new artifact');
  });
  assert.equal(buildLinuxPackage(options), output);
  assert.equal(fs.readFileSync(path.join(unpackedDir, 'resources', 'marker'), 'utf8'), 'original');
  assert.deepEqual(fs.readdirSync(path.join(root, 'dist')), [path.basename(output)]);
});


test('variants keep distinct package identities and conflict over the shared executable', () => {
  for (const profile of ['deb', 'fedora', 'opensuse', 'arch']) {
    const target = profiles[profile].target;
    for (const variant of ['original', 'zadark']) {
      const plan = packagePlan({ ...base, profile, variant });
      const own = variant === 'zadark' ? 'zalo-for-linux-zadark' : 'zalo-for-linux';
      const other = variant === 'zadark' ? 'zalo-for-linux' : 'zalo-for-linux-zadark';
      assert.equal(plan.config[target].packageName, own);
      const fpm = plan.config[target].fpm;
      assert.equal(fpm[fpm.indexOf('--conflicts') + 1], other);
    }
  }
});
