'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const { patchMainContent, PLATFORM_GUARD } = require('../scripts/patches/patch-linux-guards');

describe('Linux platform guards', () => {
  it('skips the macOS codesign executable check outside macOS', () => {
    const source = 'class SecureCipher { async checkAppSigned(){return this.isAppSigned??(await this.spawn("codesign"),this.isAppSigned=true)} }';
    const patched = patchMainContent(source);

    assert.ok(patched.includes(PLATFORM_GUARD));
    assert.ok(patched.indexOf(PLATFORM_GUARD) < patched.indexOf('spawn("codesign")'));
    assert.doesNotThrow(() => new vm.Script(patched));
  });

  it('does not spawn codesign on Linux but preserves the macOS check', async () => {
    const source = 'class SecureCipher { async checkAppSigned(){return this.isAppSigned??(await this.spawn("codesign"),this.isAppSigned=true)} }';
    const patched = patchMainContent(source);
    const LinuxCipher = vm.runInNewContext(patched + '; SecureCipher;', { process: { platform: 'linux' } });
    let linuxSpawnCalls = 0;
    const linuxCipher = new LinuxCipher();
    linuxCipher.spawn = () => { linuxSpawnCalls++; throw new Error('codesign must not run'); };
    assert.equal(await linuxCipher.checkAppSigned(), false);
    assert.equal(linuxSpawnCalls, 0);

    const MacCipher = vm.runInNewContext(patched + '; SecureCipher;', { process: { platform: 'darwin' } });
    let macSpawnCalls = 0;
    const macCipher = new MacCipher();
    macCipher.spawn = async command => { assert.equal(command, 'codesign'); macSpawnCalls++; };
    assert.equal(await macCipher.checkAppSigned(), true);
    assert.equal(macSpawnCalls, 1);
  });

  it('is idempotent', () => {
    const source = 'class SecureCipher { async checkAppSigned(){return this.isAppSigned??(await this.spawn("codesign"),this.isAppSigned=true)} }';
    const once = patchMainContent(source);
    assert.equal(patchMainContent(once), once);
  });

  it('fails closed when the upstream codesign method is missing or ambiguous', () => {
    assert.throws(() => patchMainContent('class SecureCipher { async checkAppSigned(){return false} }'), /expected one codesign checkAppSigned method, found 0/);
    assert.throws(() => patchMainContent('class A { async checkAppSigned(){await this.spawn("codesign");this.isAppSigned=true} } class B { async checkAppSigned(){await this.spawn("codesign");this.isAppSigned=true} }'), /found 2/);
  });
});
