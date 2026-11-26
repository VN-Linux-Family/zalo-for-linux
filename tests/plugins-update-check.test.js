'use strict';

const { describe, it } = require('node:test');
const assert = require('node:assert/strict');
const crypto = require('crypto');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { EventEmitter } = require('events');
const updateCheck = require('../plugins/update-check');

const BUILD = {
  version: '26.11.10',
  zadarkVersion: null,
  commit: '1ba60fb',
  buildDate: '2026-11-19T09:40:00.000Z',
  releaseRepo: 'VN-Linux-Family/zalo-for-linux'
};

const NEW_CONTENT = Buffer.from('new appimage payload');
const NEW_DIGEST = 'sha256:' + crypto.createHash('sha256').update(NEW_CONTENT).digest('hex');
const ARCH = process.arch === 'arm64' ? 'aarch64' : 'x86_64';

function release(assets, extra = {}, assetTime = '2026-11-26T08:30:00Z') {
  return {
    html_url: 'https://github.com/VN-Linux-Family/zalo-for-linux/releases/tag/26.11.20',
    published_at: '2026-11-26T08:30:40Z',
    draft: false,
    prerelease: false,
    assets: assets.map((name) => ({
      name,
      created_at: assetTime,
      updated_at: assetTime,
      size: NEW_CONTENT.length,
      digest: NEW_DIGEST,
      browser_download_url: 'https://github.com/VN-Linux-Family/zalo-for-linux/releases/download/26.11.20/' + encodeURIComponent(name)
    })),
    ...extra
  };
}

const ASSETS_2620 = [
  'Zalo-26.11.20+ZaDark-26.3-925ff72-aarch64.AppImage',
  'Zalo-26.11.20+ZaDark-26.3-925ff72-aarch64.AppImage.zsync',
  'Zalo-26.11.20+ZaDark-26.3-925ff72-x86_64.AppImage',
  'Zalo-26.11.20+ZaDark-26.3-925ff72-x86_64.AppImage.zsync',
  'Zalo-26.11.20-Original-925ff72-aarch64.AppImage',
  'Zalo-26.11.20-Original-925ff72-x86_64.AppImage'
];

function makeAppDir(info) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-update-check-'));
  fs.mkdirSync(path.join(dir, 'app', 'pc-dist'), { recursive: true });
  fs.mkdirSync(path.join(dir, 'user'));
  fs.mkdirSync(path.join(dir, 'bin'));
  fs.writeFileSync(path.join(dir, 'bin', 'Zalo.AppImage'), 'old appimage', { mode: 0o755 });
  if (info) fs.writeFileSync(path.join(dir, 'app', 'pc-dist', 'build-info.json'), JSON.stringify(info));
  return dir;
}

function fakeWindow({ title = 'Zalo', visible = true } = {}) {
  return { isDestroyed: () => false, isVisible: () => visible, isMinimized: () => false, getTitle: () => title, setProgressBar: () => {} };
}

function fakeElectron(windows = []) {
  const shown = [];
  const opened = [];
  class Notification extends EventEmitter {
    constructor(options) { super(); this.options = options; }
    static isSupported() { return true; }
    show() { shown.push(this); }
  }
  const BrowserWindow = { getAllWindows: () => windows, getFocusedWindow: () => null };
  return { shown, opened, electron: { Notification, BrowserWindow, shell: { openExternal: (url) => opened.push(url) } } };
}

// Answers dialogs in order; records what was asked.
function fakeDialog(responses) {
  const asked = [];
  return {
    asked,
    showMessageBox: async (parentOrOptions, maybeOptions) => {
      const options = maybeOptions || parentOrOptions;
      options.parent = maybeOptions ? parentOrOptions : null;
      asked.push(options);
      return { response: responses.length ? responses.shift() : options.cancelId };
    }
  };
}

function registerWith(dir, {
  fetchRelease,
  env,
  onTrayMenuChanged,
  dialog = fakeDialog([]),
  windows = [],
  quitGuardMs = 60 * 1000,
  argv = ['/tmp/.mount_x/zalo'],
  download = async (url, dest) => fs.writeFileSync(dest, NEW_CONTENT)
} = {}) {
  const relaunched = [];
  const uis = [];
  let quit = 0;
  const app = new EventEmitter();
  app.getPath = () => path.join(dir, 'user');
  app.quit = () => { quit++; };
  const spawn = (command, args, options) => {
    const child = { pid: 2147483000, killed: [], unref() { this.unrefed = true; }, kill(signal) { this.killed.push(signal); } };
    relaunched.push({ command, args, options, child });
    return child;
  };
  const createProgressUi = ({ title, onCancel }) => {
    const ui = { title, onCancel, states: [], closed: false, update(state) { this.states.push(state); }, close() { this.closed = true; } };
    uis.push(ui);
    return ui;
  };
  const fake = fakeElectron(windows);
  const plugin = updateCheck.register({
    app,
    appDir: path.join(dir, 'app'),
    dialog,
    env: env || { APPIMAGE: path.join(dir, 'bin', 'Zalo.AppImage') },
    electron: fake.electron,
    fetchRelease,
    download,
    spawn,
    createProgressUi,
    pid: 4242,
    argv,
    quitGuardMs,
    onTrayMenuChanged,
    setTimer: () => null,
    setRepeat: () => null
  });
  return { plugin, app, dialog, relaunched, uis, getQuit: () => quit, ...fake };
}

describe('update-check plugin', () => {
  it('picks the asset of the same variant and architecture with its download metadata', () => {
    const original = updateCheck.findUpdate(release(ASSETS_2620), BUILD, 'x86_64');
    assert.equal(original.assetName, 'Zalo-26.11.20-Original-925ff72-x86_64.AppImage');
    assert.equal(original.version, '26.11.20');
    assert.equal(original.commit, '925ff72');
    assert.equal(original.size, NEW_CONTENT.length);
    assert.equal(original.sha256, NEW_DIGEST.slice(7));
    assert.match(original.downloadUrl, /Original-925ff72-x86_64\.AppImage$/);

    const zadark = updateCheck.findUpdate(release(ASSETS_2620), { ...BUILD, zadarkVersion: '26.3' }, 'aarch64');
    assert.equal(zadark.assetName, 'Zalo-26.11.20+ZaDark-26.3-925ff72-aarch64.AppImage');
  });

  it('reports nothing for the running build, an older Zalo, drafts or a missing arch', () => {
    assert.equal(updateCheck.findUpdate(release(ASSETS_2620), { ...BUILD, version: '26.11.20', commit: '925ff72' }, 'x86_64'), null);
    assert.equal(updateCheck.findUpdate(release(ASSETS_2620), { ...BUILD, version: '26.12.1' }, 'x86_64'), null);
    assert.equal(updateCheck.findUpdate(release(ASSETS_2620, { draft: true }), BUILD, 'x86_64'), null);
    assert.equal(updateCheck.findUpdate(release(ASSETS_2620, { prerelease: true }), BUILD, 'x86_64'), null);
    assert.equal(updateCheck.findUpdate(release(['Zalo-26.11.20-Original-925ff72-x86_64.AppImage']), BUILD, 'aarch64'), null);
  });

  it('treats a rebuild of the same Zalo version as newer only when its asset was uploaded after this build', () => {
    const same = { ...BUILD, version: '26.11.20' };
    assert.ok(updateCheck.findUpdate(release(ASSETS_2620), same, 'x86_64'));
    assert.equal(updateCheck.findUpdate(release(ASSETS_2620, {}, '2026-11-01T00:00:00Z'), same, 'x86_64'), null);

    // The workflow reuses the tag: published_at stays at the first build while
    // the assets are replaced by a later one.
    const built0900 = { ...same, buildDate: '2026-11-26T09:00:00Z' };
    const reused = release(ASSETS_2620, { published_at: '2026-11-26T08:30:40Z' }, '2026-11-26T09:40:00Z');
    assert.ok(updateCheck.findUpdate(reused, built0900, 'x86_64'));
    const older = release(ASSETS_2620, { published_at: '2026-11-26T09:30:00Z' }, '2026-11-26T08:50:00Z');
    assert.equal(updateCheck.findUpdate(older, built0900, 'x86_64'), null);
  });

  it('compares versions numerically', () => {
    assert.equal(updateCheck.compareVersions('26.11.20', '26.11.9'), 1);
    assert.equal(updateCheck.compareVersions('26.11', '26.11.0'), 0);
    assert.equal(updateCheck.compareVersions('25.12.1', '26.1.1'), -1);
  });

  it('stays off outside an AppImage, when disabled, or without release metadata', async () => {
    const dir = makeAppDir(BUILD);
    const noInfo = makeAppDir(null);
    const oldInfo = makeAppDir({ ...BUILD, releaseRepo: undefined });
    try {
      let calls = 0;
      const fetchRelease = async () => { calls++; return release(ASSETS_2620); };
      for (const [d, env] of [[dir, {}], [dir, { APPIMAGE: '/x', ZALO_UPDATE_CHECK: '0' }], [noInfo, { APPIMAGE: '/x' }], [oldInfo, { APPIMAGE: '/x' }]]) {
        const { plugin } = registerWith(d, { fetchRelease, env });
        assert.equal(plugin.isEnabled(), false);
        assert.equal(await plugin.check(), null);
      }
      assert.equal(calls, 0);
    } finally {
      for (const d of [dir, noInfo, oldInfo]) fs.rmSync(d, { recursive: true, force: true });
    }
  });

  it('notifies once per release and keeps the tray item', async () => {
    const dir = makeAppDir(BUILD);
    try {
      let trayChanges = 0;
      const { plugin, shown } = registerWith(dir, {
        fetchRelease: async (repo) => { assert.equal(repo, 'VN-Linux-Family/zalo-for-linux'); return release(ASSETS_2620); },
        onTrayMenuChanged: () => trayChanges++
      });
      assert.deepEqual(plugin.getTrayMenuItems(), []);

      const update = await plugin.check();
      assert.equal(update.assetName, `Zalo-26.11.20-Original-925ff72-${ARCH}.AppImage`);
      assert.equal(trayChanges, 1);
      assert.equal(shown.length, 1);
      assert.match(shown[0].options.body, /26\.11\.20 \(925ff72\)/);
      assert.equal(plugin.getTrayMenuItems()[0].label, 'Có bản mới: 26.11.20 (925ff72)');

      await plugin.check();
      assert.equal(shown.length, 1, 'same release must not notify twice');
      assert.equal(trayChanges, 1);

      const again = registerWith(dir, { fetchRelease: async () => release(ASSETS_2620) });
      await again.plugin.check();
      assert.equal(again.shown.length, 0, 'remembered across restarts');
      assert.equal(again.plugin.getTrayMenuItems().length, 1, 'tray item still shown');

      plugin.setLocale('en');
      assert.equal(plugin.getTrayMenuItems()[0].label, 'Update available: 26.11.20 (925ff72)');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('asks right away in the visible Zalo window instead of a desktop notification', async () => {
    const dir = makeAppDir(BUILD);
    try {
      const background = fakeWindow({ title: 'Shared Worker' });
      const main = fakeWindow();
      const dialog = fakeDialog([2]);
      const ctx = registerWith(dir, { fetchRelease: async () => release(ASSETS_2620), dialog, windows: [background, main] });
      await ctx.plugin.check();
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(ctx.shown.length, 0, 'no desktop notification while the window is on screen');
      assert.equal(dialog.asked.length, 1);
      assert.strictEqual(dialog.asked[0].parent, main, 'attached to the Zalo window, not a background one');
      assert.match(dialog.asked[0].message, /26\.11\.20 \(925ff72\)/);

      // Once per release, also for the popup.
      await ctx.plugin.check();
      const again = registerWith(dir, { fetchRelease: async () => release(ASSETS_2620), dialog, windows: [main] });
      await again.plugin.check();
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(dialog.asked.length, 1);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('uses a desktop notification when Zalo is hidden in the tray', async () => {
    const dir = makeAppDir(BUILD);
    try {
      const dialog = fakeDialog([2]);
      const ctx = registerWith(dir, {
        fetchRelease: async () => release(ASSETS_2620),
        dialog,
        windows: [fakeWindow({ visible: false }), fakeWindow({ title: 'SQLite' })]
      });
      await ctx.plugin.check();
      assert.equal(ctx.shown.length, 1);
      assert.equal(dialog.asked.length, 0);
      ctx.shown[0].emit('click');
      await new Promise((resolve) => setImmediate(resolve));
      assert.equal(dialog.asked.length, 1, 'clicking the notification opens the prompt');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('asks first: "Later" and "Open release page" leave the AppImage alone', async () => {
    const dir = makeAppDir(BUILD);
    try {
      let downloads = 0;
      const dialog = fakeDialog([2, 1]);
      const { plugin, opened } = registerWith(dir, {
        fetchRelease: async () => release(ASSETS_2620),
        dialog,
        download: async () => { downloads++; }
      });
      await plugin.check();
      assert.equal(await plugin.promptUpdate(), 'later');
      assert.equal(await plugin.promptUpdate(), 'page');
      assert.equal(downloads, 0);
      assert.deepEqual(opened, [release([]).html_url]);
      assert.equal(dialog.asked[0].buttons[0], 'Cập nhật');
      assert.equal(fs.readFileSync(path.join(dir, 'bin', 'Zalo.AppImage'), 'utf8'), 'old appimage');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('downloads with a progress window, verifies, swaps the AppImage keeping .old, and relaunches on confirm', async () => {
    const dir = makeAppDir(BUILD);
    const appImage = path.join(dir, 'bin', 'Zalo.AppImage');
    try {
      const trayDuring = [];
      const dialog = fakeDialog([0, 0]);
      let plugin;
      const ctx = registerWith(dir, {
        fetchRelease: async () => release(ASSETS_2620),
        dialog,
        argv: ['/tmp/.mount_x/zalo', '--hidden', '--foo'],
        download: async (url, dest, onProgress, signal) => {
          assert.match(url, new RegExp(`Original-925ff72-${ARCH}\\.AppImage$`));
          assert.equal(path.dirname(dest), path.dirname(appImage), 'temp file must sit next to the AppImage');
          assert.ok(signal && typeof signal.aborted === 'boolean');
          fs.writeFileSync(dest, NEW_CONTENT);
          onProgress(NEW_CONTENT.length / 2, NEW_CONTENT.length);
          onProgress(NEW_CONTENT.length, NEW_CONTENT.length);
          trayDuring.push(plugin.getTrayMenuItems()[0]);
        }
      });
      plugin = ctx.plugin;
      await plugin.check();
      assert.equal(await plugin.promptUpdate(), 'updated');

      // Visible feedback: a progress window from the first moment to the end.
      assert.equal(ctx.uis.length, 1);
      const ui = ctx.uis[0];
      assert.equal(ui.states[0].message, 'Đang tải bản 26.11.20 (925ff72)…');
      assert.equal(ui.states[0].percent, 0);
      assert.equal(ui.states[0].cancelLabel, 'Hủy');
      assert.deepEqual(ui.states.filter((st) => st.percent > 0).map((st) => st.percent), [50, 100]);
      assert.match(ui.states.find((st) => st.percent === 100).detail, /^100% · 0\.0 \/ 0\.0 MB$/);
      assert.equal(ui.states.at(-1).message, 'Đang kiểm tra file tải về…');
      assert.equal(ui.closed, true);
      assert.equal(ctx.shown.length, 1, 'only the "new release" notification, no fallback notice');

      assert.equal(trayDuring[0].label, 'Đang tải bản mới… 100%');
      assert.equal(trayDuring[0].enabled, false);
      assert.deepEqual(fs.readFileSync(appImage), NEW_CONTENT);
      assert.equal(fs.statSync(appImage).mode & 0o777, 0o755);
      assert.equal(fs.readFileSync(appImage + '.old', 'utf8'), 'old appimage');
      assert.deepEqual(fs.readdirSync(path.join(dir, 'bin')).sort(), ['Zalo.AppImage', 'Zalo.AppImage.old']);

      // Relaunch through a detached shell waiting for this PID, not app.relaunch().
      assert.equal(plugin.getState(), 'ready');
      assert.equal(dialog.asked[1].buttons[0], 'Khởi động lại');
      assert.equal(ctx.relaunched.length, 1);
      const { command, args, options, child } = ctx.relaunched[0];
      assert.equal(command, updateCheck.relaunchShell().command);
      assert.match(args[1], /exec \{fd\}>&-/, 'inherited fds are closed before waiting');
      assert.deepEqual(args.slice(3), ['4242', appImage, '--foo']);
      assert.equal(options.detached, true);
      assert.equal(options.stdio, 'ignore');
      assert.equal(child.unrefed, true);
      assert.equal(ctx.getQuit(), 1);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('cancelling from the progress window stops the download and leaves the AppImage alone', async () => {
    const dir = makeAppDir(BUILD);
    const appImage = path.join(dir, 'bin', 'Zalo.AppImage');
    try {
      const dialog = fakeDialog([0]);
      let ctx;
      ctx = registerWith(dir, {
        fetchRelease: async () => release(ASSETS_2620),
        dialog,
        download: (url, dest, onProgress, signal) => new Promise((resolve, reject) => {
          fs.writeFileSync(dest, 'partial');
          signal.addEventListener('abort', () => {
            const error = new Error('cancelled');
            error.code = 'CANCELLED';
            reject(error);
          });
          setImmediate(() => ctx.uis[0].onCancel());
        })
      });
      await ctx.plugin.check();
      assert.equal(await ctx.plugin.promptUpdate(), 'cancelled');
      assert.equal(ctx.uis[0].closed, true);
      assert.equal(dialog.asked.length, 1, 'no error dialog after a cancel');
      assert.equal(fs.readFileSync(appImage, 'utf8'), 'old appimage');
      assert.deepEqual(fs.readdirSync(path.join(dir, 'bin')), ['Zalo.AppImage']);
      assert.equal(ctx.plugin.getState(), 'idle');
      assert.equal(ctx.plugin.getTrayMenuItems()[0].label, 'Có bản mới: 26.11.20 (925ff72)');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('stops the waiting relauncher when the quit is cancelled, but not once quitting is committed', async () => {
    const dir = makeAppDir(BUILD);
    try {
      const cancelled = registerWith(dir, { fetchRelease: async () => release(ASSETS_2620), dialog: fakeDialog([0, 0]), quitGuardMs: 20 });
      await cancelled.plugin.check();
      await cancelled.plugin.promptUpdate();
      await new Promise((resolve) => setTimeout(resolve, 60));
      assert.deepEqual(cancelled.relaunched[0].child.killed, ['SIGTERM']);
      assert.equal(cancelled.plugin.getTrayMenuItems()[0].label, 'Khởi động lại để hoàn tất cập nhật');

      fs.rmSync(path.join(dir, 'bin', 'Zalo.AppImage.old'));
      fs.writeFileSync(path.join(dir, 'bin', 'Zalo.AppImage'), 'old appimage');
      fs.rmSync(path.join(dir, 'user', 'zalo-linux-settings.json'));
      const committed = registerWith(dir, { fetchRelease: async () => release(ASSETS_2620), dialog: fakeDialog([0, 0]), quitGuardMs: 20 });
      await committed.plugin.check();
      await committed.plugin.promptUpdate();
      committed.app.emit('will-quit');
      await new Promise((resolve) => setTimeout(resolve, 60));
      assert.deepEqual(committed.relaunched[0].child.killed, []);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('falls back to a notification when the progress window cannot open', async () => {
    const dir = makeAppDir(BUILD);
    try {
      const fake = fakeElectron([]);
      const plugin = updateCheck.register({
        app: Object.assign(new EventEmitter(), { getPath: () => path.join(dir, 'user'), quit() {} }),
        appDir: path.join(dir, 'app'),
        dialog: fakeDialog([0, 1]),
        env: { APPIMAGE: path.join(dir, 'bin', 'Zalo.AppImage') },
        electron: fake.electron,
        fetchRelease: async () => release(ASSETS_2620),
        download: async (url, dest) => fs.writeFileSync(dest, NEW_CONTENT),
        createProgressUi: () => { throw new Error('no display'); },
        setTimer: () => null,
        setRepeat: () => null
      });
      await plugin.check();
      assert.equal(await plugin.promptUpdate(), 'updated');
      assert.equal(fake.shown.length, 2);
      assert.equal(fake.shown[1].options.body, 'Đang tải bản 26.11.20 (925ff72)…');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('after "Later" on restart, the tray item offers the restart again', async () => {
    const dir = makeAppDir(BUILD);
    try {
      const dialog = fakeDialog([0, 1, 0]);
      const ctx = registerWith(dir, { fetchRelease: async () => release(ASSETS_2620), dialog });
      await ctx.plugin.check();
      await ctx.plugin.promptUpdate();
      assert.equal(ctx.relaunched.length, 0);
      assert.equal(ctx.getQuit(), 0);
      const item = ctx.plugin.getTrayMenuItems()[0];
      assert.equal(item.label, 'Khởi động lại để hoàn tất cập nhật');
      assert.equal(await ctx.plugin.promptUpdate(), 'ready');
      assert.equal(ctx.relaunched.length, 1);
      assert.equal(await ctx.plugin.check(), null, 'no re-check once installed');
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('rejects a download whose SHA-256 or size does not match and leaves the AppImage untouched', async () => {
    for (const bad of [Buffer.from('tampered appimage pay'), Buffer.from('short')]) {
      const dir = makeAppDir(BUILD);
      const appImage = path.join(dir, 'bin', 'Zalo.AppImage');
      try {
        const dialog = fakeDialog([0, 1]);
        const { plugin, relaunched } = registerWith(dir, {
          fetchRelease: async () => release(ASSETS_2620),
          dialog,
          download: async (url, dest) => fs.writeFileSync(dest, bad)
        });
        await plugin.check();
        assert.equal(await plugin.promptUpdate(), 'failed');
        assert.equal(fs.readFileSync(appImage, 'utf8'), 'old appimage');
        assert.deepEqual(fs.readdirSync(path.join(dir, 'bin')), ['Zalo.AppImage'], 'no temp file or .old left behind');
        assert.equal(dialog.asked[1].type, 'error');
        assert.match(dialog.asked[1].message, /SHA-256 mismatch|size mismatch/);
        assert.equal(plugin.getState(), 'idle');
        assert.equal(relaunched.length, 0);
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    }
  });

  it('falls back to the release page when the AppImage cannot be replaced', async () => {
    const dir = makeAppDir(BUILD);
    try {
      let downloads = 0;
      const dialog = fakeDialog([0]);
      const { plugin, opened } = registerWith(dir, {
        env: { APPIMAGE: path.join(dir, 'missing', 'Zalo.AppImage') },
        fetchRelease: async () => release(ASSETS_2620),
        dialog,
        download: async () => { downloads++; }
      });
      await plugin.check();
      assert.equal(await plugin.promptUpdate(), 'failed');
      assert.equal(downloads, 0);
      assert.equal(dialog.asked[1].type, 'warning');
      assert.deepEqual(opened, [release([]).html_url]);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('drops environment that points into the old AppImage mount', () => {
    const env = updateCheck.relaunchEnv({
      APPDIR: '/tmp/.mount_ZaloAB',
      APPIMAGE: '/home/u/Zalo.AppImage',
      ARGV0: 'Zalo.AppImage',
      OWD: '/home/u',
      PATH: '/tmp/.mount_ZaloAB/usr/bin:/usr/bin:/bin',
      LD_LIBRARY_PATH: '/tmp/.mount_ZaloAB/usr/lib',
      HOME: '/home/u'
    });
    assert.deepEqual(env, { PATH: '/usr/bin:/bin', HOME: '/home/u' });
  });

  it('really relaunches only after the old process exits, without NO_NEW_PRIVS', async () => {
    const { spawn } = require('child_process');
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-relaunch-'));
    const marker = path.join(dir, 'started');
    const target = path.join(dir, 'Zalo.AppImage');
    // Stands in for the new AppImage: records that it ran, its NoNewPrivs and env.
    fs.writeFileSync(target, `#!/bin/sh
{ grep NoNewPrivs /proc/self/status || echo 'NoNewPrivs: n/a'; echo "args=$*"; echo "appdir=\${APPDIR:-unset}"; } > "${marker}.tmp"
mv "${marker}.tmp" "${marker}"
`, { mode: 0o755 });
    const old = spawn('sleep', ['30'], { stdio: 'ignore' });
    try {
      updateCheck.spawnRelauncher({
        spawn,
        pid: old.pid,
        execPath: target,
        args: ['--foo'],
        env: { ...process.env, APPDIR: '/tmp/.mount_old' }
      });
      await new Promise((resolve) => setTimeout(resolve, 500));
      assert.equal(fs.existsSync(marker), false, 'must wait while the old process is alive');

      old.kill('SIGTERM');
      const deadline = Date.now() + 5000;
      while (!fs.existsSync(marker) && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 50));
      assert.ok(fs.existsSync(marker), 'new process must start after the old one exits');

      const report = fs.readFileSync(marker, 'utf8');
      const own = (fs.readFileSync('/proc/self/status', 'utf8').match(/NoNewPrivs:\s*(\d)/) || [])[1];
      if (own === '0') assert.match(report, /NoNewPrivs:\s*0/, 'relaunched process must be able to gain privileges (fusermount3)');
      assert.match(report, /args=--foo/);
      assert.match(report, /appdir=unset/);
    } finally {
      try { old.kill('SIGKILL'); } catch (_) {}
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('prefers bash to close inherited fds, with a best-effort sh fallback', () => {
    const bash = updateCheck.relaunchShell((candidate) => candidate === '/usr/bin/bash');
    assert.equal(bash.command, '/usr/bin/bash');
    assert.ok(bash.script.indexOf('exec {fd}>&-') < bash.script.indexOf('while '), 'fds are closed before the wait');
    const sh = updateCheck.relaunchShell(() => false);
    assert.equal(sh.command, '/bin/sh');
    assert.match(sh.script, /^exec 3>&- 4>&- 5>&- 6>&- 7>&- 8>&- 9>&-\n/);
  });

  it('the relaunched process and the waiting shell do not inherit the old process fds', async (t) => {
    if (!fs.existsSync('/bin/bash') && !fs.existsSync('/usr/bin/bash')) {
      t.skip('bash not available');
      return;
    }
    const { spawn, spawnSync } = require('child_process');
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'zalo-relaunch-fds-'));
    const sentinel = path.join(dir, 'old-zalo-cache');
    const marker = path.join(dir, 'fds');
    const target = path.join(dir, 'Zalo.AppImage');
    const helper = path.join(dir, 'old-zalo.js');
    fs.writeFileSync(sentinel, 'cache');
    // The "new AppImage": lists the fds of its own shell (fd 255 is bash's
    // script file); ls runs as a child, so its redirection is not counted.
    fs.writeFileSync(target, `#!/bin/bash
ls -l /proc/$$/fd > "${marker}.tmp"
mv "${marker}.tmp" "${marker}"
`, { mode: 0o755 });
    // The "old Zalo": a Node process holding non-CLOEXEC fds (3 low, one above 9,
    // one high), starting the relauncher the way the plugin does.
    fs.writeFileSync(helper, `
const u = require(${JSON.stringify(path.resolve(__dirname, '..', 'plugins', 'update-check'))});
const child = u.spawnRelauncher({ spawn: require('child_process').spawn, pid: Number(process.argv[2]), execPath: process.argv[3] });
console.log(child.pid);
`);
    const old = spawn('sleep', ['30'], { stdio: 'ignore' });
    try {
      const run = spawnSync('bash', ['-c', 'exec 7<"$1" 15<"$1" 200<"$1"; exec "$2" "$3" "$4" "$5"', 'x',
        sentinel, process.execPath, helper, String(old.pid), target], { encoding: 'utf8' });
      assert.equal(run.status, 0, run.stderr);
      const waiter = Number(run.stdout.trim());
      assert.ok(waiter > 0);

      // While waiting, the shell must already have dropped the inherited fds.
      await new Promise((resolve) => setTimeout(resolve, 300));
      const waiting = fs.readdirSync(`/proc/${waiter}/fd`).map((fd) => {
        try { return fs.readlinkSync(`/proc/${waiter}/fd/${fd}`); } catch (_) { return ''; }
      });
      assert.ok(!waiting.includes(sentinel), `waiting shell still holds the old fds: ${waiting.join(', ')}`);
      assert.equal(fs.existsSync(marker), false);

      old.kill('SIGTERM');
      const deadline = Date.now() + 5000;
      while (!fs.existsSync(marker) && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 50));
      const fds = fs.readFileSync(marker, 'utf8').split('\n')
        .map((line) => line.match(/ (\d+) -> (.*)$/))
        .filter(Boolean)
        .map(([, fd, link]) => ({ fd: Number(fd), link }));
      assert.deepEqual(fds.map(({ fd }) => fd).sort((a, b) => a - b), [0, 1, 2, 255],
        `only stdio and bash's script fd may be open: ${JSON.stringify(fds)}`);
      assert.equal(fds.find(({ fd }) => fd === 255).link, target);
      assert.ok(!fds.some(({ link }) => link === sentinel));
    } finally {
      try { old.kill('SIGKILL'); } catch (_) {}
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });

  it('progress window shows the page, replays state after load, and cancels from button or close', () => {
    const created = [];
    class FakeWindow extends EventEmitter {
      constructor(options) {
        super();
        this.options = options;
        this.destroyed = false;
        this.scripts = [];
        this.webContents = new EventEmitter();
        this.webContents.executeJavaScript = (js) => { this.scripts.push(js); return Promise.resolve(); };
        created.push(this);
      }
      isDestroyed() { return this.destroyed; }
      destroy() { this.destroyed = true; }
      loadFile(file) { this.file = file; }
      show() { this.shown = true; }
    }
    let cancels = 0;
    const ui = updateCheck.createProgressWindow({ BrowserWindow: FakeWindow, title: 'T', onCancel: () => cancels++ });
    const win = created[0];
    assert.equal(path.basename(win.file), 'progress.html');
    assert.ok(fs.existsSync(win.file));
    assert.equal(win.options.webPreferences.nodeIntegration, false);
    assert.equal(win.options.webPreferences.contextIsolation, true);

    ui.update({ message: 'a', percent: 0 });
    ui.update({ percent: 40 });
    assert.equal(win.scripts.length, 0, 'nothing runs before the page has loaded');
    win.webContents.emit('did-finish-load');
    assert.equal(win.scripts.length, 1);
    assert.match(win.scripts[0], /"message":"a"/);
    assert.match(win.scripts[0], /"percent":40/);

    win.emit('ready-to-show');
    assert.equal(win.shown, true);

    let prevented = 0;
    win.emit('page-title-updated', { preventDefault: () => prevented++ }, 'T');
    assert.equal(cancels, 0);
    win.emit('page-title-updated', { preventDefault: () => prevented++ }, 'cancel');
    assert.equal(cancels, 1);
    win.emit('close', { preventDefault: () => prevented++ });
    assert.equal(cancels, 2);
    assert.equal(prevented, 3, 'title kept and window kept open until the download stops');

    ui.close();
    assert.equal(win.destroyed, true);
    win.emit('close', { preventDefault: () => prevented++ });
    assert.equal(cancels, 2, 'closing after completion is not a cancel');
  });

  it('keeps other settings and survives network errors', async () => {
    const dir = makeAppDir(BUILD);
    try {
      fs.writeFileSync(path.join(dir, 'user', 'zalo-linux-settings.json'), JSON.stringify({ devMode: 1 }));
      const failing = registerWith(dir, { fetchRelease: async () => { throw new Error('offline'); } });
      assert.equal(await failing.plugin.check(), null);
      assert.deepEqual(failing.plugin.getTrayMenuItems(), []);

      const ok = registerWith(dir, { fetchRelease: async () => release(ASSETS_2620) });
      await ok.plugin.check();
      const settings = JSON.parse(fs.readFileSync(path.join(dir, 'user', 'zalo-linux-settings.json'), 'utf8'));
      assert.equal(settings.devMode, 1);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
