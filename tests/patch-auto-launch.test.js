'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const vm = require('node:vm');

const { patchAutoLaunch } = require('../scripts/patches/patch-auto-launch');

function makeSampleBundle(varName = 'd', initName = 'u') {
  return [
    'const o=require("path"),i={getPath:()=>"/opt/Zalo/zalo"};',
    'const c={zsymb:()=>{}},We={zsymb:(...args)=>{globalThis.__zsymbCalls&&globalThis.__zsymbCalls.push(args)}};',
    `let ${varName},l={get:()=>undefined,set:()=>{}};`,
    `function ${initName}(e){const r=globalThis.__MockLauncher;` +
    'if("win32"===process.platform){}else if("win32"===process.platform){let t=o.join(o.dirname(i.getPath("exe")),"..","Zalo.exe");e.path=t}' +
    `${varName}=new r(e);` +
    'const t=!1!==e.get("autolaunch");' +
    `${varName}.isEnabled().then((e=>{c.zsymb(4,"Gyqxit",["autolaunch isEnabled {} shouldEnable {}","4rLnzJ"],e,t),e!==t?t?(c.zsymb(4,"klmK4I",["autolaunch: syncing to enabled","tMlCcA"]),${varName}.enable()):(c.zsymb(4,"UhB-8k",["autolaunch: syncing to disabled","p3kCkQ"]),${varName}.disable()):c.zsymb(4,"WRkP8g",["autolaunch: already in sync, no write needed","CsVL-B"])})).catch((e=>{c.zsymb(22,"RnsE2Z",["autolaunch: Error checking isEnabled {}","d8CJZq"],e)}))}`,
    `const mod={configureLauncher:()=>{${initName}(l)},getZaloLauncher:()=>${varName}};`,
    'const Ke={checkAutoLaunchEnable:e=>{const{getZaloLauncher:t}=n("Wsuc");return t().isEnabled()},' +
    'toggleAutoLaunch:(e,t)=>{const{appConfig:r}=n("ZQzv"),{getZaloLauncher:i}=n("Wsuc"),o=i();' +
    't?(We.zsymb(4,"0Rv3yj",["autolaunch to enable","PBDWOh"]),o.enable(),r.set("autolaunch",!0)):(We.zsymb(4,"V2zVE1",["autolaunch to disable","gFpcFf"]),o.disable(),r.set("autolaunch",!1))},};',
    'globalThis.__testExports = { mod, Ke, init: ' + initName + ' };',
  ].join('\n');
}

function runPatchedSnippet(patchedCode, { isFlatpak = false, appImage = '', xdgConfigHome = '', mockCalls = {} } = {}) {
  const gdbusCalls = [];
  const launcherCalls = [];
  const launcherOptions = [];
  const unlinkedPaths = [];
  const zsymbCalls = [];

  class MockLauncher {
    constructor(opts) {
      launcherOptions.push({ ...opts });
      this.opts = opts;
    }
    async isEnabled() {
      launcherCalls.push('isEnabled');
      return Boolean(mockCalls.isEnabledResult);
    }
    async enable() {
      launcherCalls.push('enable');
    }
    async disable() {
      launcherCalls.push('disable');
    }
  }

  const fakeFs = {
    existsSync(p) {
      if (p === '/.flatpak-info') return isFlatpak;
      return fs.existsSync(p);
    },
    unlinkSync(p) {
      if (mockCalls.unlinkError) {
        const err = new Error('ENOENT');
        err.code = 'ENOENT';
        throw err;
      }
      unlinkedPaths.push(p);
    },
  };

  const fakeChildProcess = {
    execFile(cmd, args, opts, cb) {
      gdbusCalls.push({ cmd, args });
      if (mockCalls.gdbusError) {
        cb(new Error(mockCalls.gdbusError), '', mockCalls.gdbusError);
        return;
      }
      cb(null, mockCalls.gdbusStdout || '()', '');
    },
  };

  const fakeProcess = {
    platform: 'linux',
    execPath: '/opt/Zalo/zalo',
    env: {
      ...(isFlatpak ? { FLATPAK_ID: 'net.vietnamlinuxfamily.zalo_for_linux' } : {}),
      ...(appImage ? { APPIMAGE: appImage } : {}),
      ...(xdgConfigHome ? { XDG_CONFIG_HOME: xdgConfigHome } : {}),
    },
  };

  const customRequire = (id) => {
    if (id === 'child_process') return fakeChildProcess;
    if (id === 'fs') return fakeFs;
    if (id === 'path') return path;
    if (id === 'os') return os;
    return require(id);
  };

  const sandbox = {
    require: customRequire,
    process: fakeProcess,
    console,
    Math,
    Promise,
    Error,
    Set,
    globalThis: { __MockLauncher: MockLauncher, __zsymbCalls: zsymbCalls },
    n: (modName) => {
      if (modName === 'Wsuc') return sandbox.globalThis.__testExports.mod;
      if (modName === 'ZQzv') return { appConfig: mockCalls.appConfig || { get: () => undefined, set: () => {} } };
      return {};
    },
  };
  sandbox.globalThis.require = customRequire;

  vm.runInNewContext(patchedCode, sandbox);
  return {
    exports: sandbox.globalThis.__testExports,
    gdbusCalls,
    launcherCalls,
    launcherOptions,
    unlinkedPaths,
    zsymbCalls,
  };
}

describe('patch-auto-launch', () => {
  it('INV-1: startup does not call enable() and syncs appConfig to false when OS entry is absent', async () => {
    const patched = patchAutoLaunch(makeSampleBundle('d', 'u'));
    const setCalls = [];
    const env = runPatchedSnippet(patched, {
      isFlatpak: false,
      appImage: '/home/user/Applications/Zalo.AppImage',
      mockCalls: { isEnabledResult: false },
    });

    env.exports.init({ get: () => undefined, set: (...args) => setCalls.push(['unset', ...args]) });
    await new Promise((r) => setImmediate(r));
    assert.ok(!env.launcherCalls.includes('enable'), 'startup must not call enable() when autolaunch is unset');
    assert.strictEqual(setCalls.length, 0, 'unset autolaunch should not trigger set()');

    env.exports.init({ get: () => true, set: (...args) => setCalls.push(['true', ...args]) });
    await new Promise((r) => setImmediate(r));
    assert.ok(!env.launcherCalls.includes('enable'), 'startup must not call enable() when OS entry is absent');
    assert.deepStrictEqual(setCalls, [['true', 'autolaunch', false]], 'external deletion must sync appConfig.autolaunch to false');
  });

  it('INV-2: Flatpak uses Portal exclusively, never calls r.enable()/r.disable(), and propagates D-Bus errors via IPC', async () => {
    const patched = patchAutoLaunch(makeSampleBundle('d', 'u'));
    const envOk = runPatchedSnippet(patched, {
      isFlatpak: true,
      mockCalls: { gdbusStdout: "('yes',)" },
    });

    assert.strictEqual(await envOk.exports.Ke.checkAutoLaunchEnable(), true);
    assert.strictEqual(await envOk.exports.Ke.toggleAutoLaunch(null, true), true);
    assert.strictEqual(await envOk.exports.Ke.toggleAutoLaunch(null, false), true);
    assert.ok(envOk.gdbusCalls.length >= 3, 'Expected gdbus calls for Flatpak portal');
    assert.ok(!envOk.launcherCalls.includes('enable'), 'Flatpak must not call r.enable()');
    assert.ok(!envOk.launcherCalls.includes('disable'), 'Flatpak must not call r.disable()');

    const envErr = runPatchedSnippet(patched, {
      isFlatpak: true,
      mockCalls: { gdbusError: 'GDBus.Error:org.freedesktop.DBus.Error.UnknownMethod' },
    });
    const toggleResult = await envErr.exports.Ke.toggleAutoLaunch(null, true);
    assert.strictEqual(toggleResult, false, 'toggleAutoLaunch must return false when Portal RequestBackground throws');
    assert.ok(
      envErr.zsymbCalls.some((c) => c[1] === 'linux_auto_launch_error'),
      'Expected linux_auto_launch_error telemetry on Flatpak portal failure'
    );
  });

  it('INV-3: Host uses XDG auto-launch exclusively with e.name="zalo", respects XDG_CONFIG_HOME, and cleans legacy AppImage entries', async () => {
    const patched = patchAutoLaunch(makeSampleBundle('d', 'u'));
    const env = runPatchedSnippet(patched, {
      isFlatpak: false,
      appImage: '/home/user/Applications/Zalo-26.08.20.AppImage',
      xdgConfigHome: '/tmp/custom-xdg-config',
      mockCalls: { isEnabledResult: false },
    });

    assert.strictEqual(await env.exports.Ke.checkAutoLaunchEnable(), false);
    assert.strictEqual(env.launcherOptions[0].name, 'zalo');
    assert.strictEqual(env.launcherOptions[0].path, '/home/user/Applications/Zalo-26.08.20.AppImage');

    assert.strictEqual(await env.exports.Ke.toggleAutoLaunch(null, true), true);
    assert.strictEqual(await env.exports.Ke.toggleAutoLaunch(null, false), true);

    assert.strictEqual(env.gdbusCalls.length, 0, 'Host must not invoke gdbus RequestBackground');
    assert.deepStrictEqual(env.launcherCalls, ['isEnabled', 'isEnabled', 'enable', 'disable']);
    assert.ok(
      env.unlinkedPaths.includes('/tmp/custom-xdg-config/autostart/Zalo.AppImage.desktop'),
      'Host disable() should clean up legacy Zalo.AppImage.desktop under XDG_CONFIG_HOME'
    );
    assert.ok(
      env.unlinkedPaths.includes('/tmp/custom-xdg-config/autostart/Zalo-26.08.20.AppImage.desktop'),
      'Host disable() should clean up versioned AppImage desktop entry under XDG_CONFIG_HOME'
    );

    const envNoAppImage = runPatchedSnippet(patched, {
      isFlatpak: false,
      mockCalls: { isEnabledResult: false, unlinkError: true },
    });
    assert.strictEqual(await envNoAppImage.exports.Ke.toggleAutoLaunch(null, false), true);
  });

  it('INV-4: patchAutoLaunch is idempotent and upgrades legacy v1-patched bundles cleanly', () => {
    for (const [varName, initName] of [['d', 'u'], ['u', 'd']]) {
      const sample = makeSampleBundle(varName, initName);
      const once = patchAutoLaunch(sample);
      const twice = patchAutoLaunch(once);
      assert.strictEqual(twice, once, `patchAutoLaunch should be idempotent for var=${varName}`);

      // Simulate v1-patched bundle (lacking e.name="zalo" and linux_autostart_ssot)
      const legacyV1 = once
        .replace('e.name="zalo",', '')
        .replace(/const ([A-Za-z_$][A-Za-z0-9_$]*)=!0===([A-Za-z_$][A-Za-z0-9_$]*)\.get\("autolaunch"\),__zaloAutoCfg=\2;([A-Za-z_$][A-Za-z0-9_$]*)\.isEnabled\(\)\.then\(\(e=>\{[^}]+\}\)\)/,
          `const t=!1!==e.get("autolaunch");${varName}.isEnabled().then((e=>{c.zsymb(4,"Gyqxit",["autolaunch isEnabled {} shouldEnable {}","4rLnzJ"],e,t),e!==t?t?(c.zsymb(4,"klmK4I",["autolaunch: syncing to enabled","tMlCcA"]),${varName}.enable()):(c.zsymb(4,"UhB-8k",["autolaunch: syncing to disabled","p3kCkQ"]),${varName}.disable()):c.zsymb(4,"WRkP8g",["autolaunch: already in sync, no write needed","CsVL-B"])}))`
        );
      const upgraded = patchAutoLaunch(legacyV1);
      assert.ok(upgraded.includes('e.name="zalo"'), 'Legacy v1 upgrade must inject e.name="zalo"');
      assert.ok(upgraded.includes('linux_autostart_ssot'), 'Legacy v1 upgrade must apply linux_autostart_ssot');
      assert.strictEqual(patchAutoLaunch(upgraded), upgraded, 'Upgraded v1 bundle must be idempotent');
    }
  });
});
