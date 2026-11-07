// Linux wrapper around the native zfile addon.
//
// The Windows Zalo renderer's Data-Management screen looks up disk info by
// PATH, transformed through `formatDrivePath` which, for a Windows path,
// yields a drive key like "C:\". On Linux `formatDrivePath` returns the path
// unchanged (e.g. "/home/user/..."), so the renderer does
// `diskInfo()["/home/user/..."]` — a key our mount-point-keyed getDiskInfo
// does not contain → `undefined.label` throws in the renderer → the screen
// spins forever.
//
// Fix: getDiskInfo() returns a Proxy that resolves any absolute POSIX path
// key to the entry of the mount point that contains it (longest-prefix
// match). Exact mount keys still hit directly, and Object.entries() still
// enumerates the real mount points (the Proxy only adds a `get` trap), so the
// drive-list bar keeps working while by-path lookups also resolve.

const native = require('./zfile-native.node');

// Find the entry of the mount whose path is the longest prefix of `key`.
function resolveByPath(disks, key) {
  if (typeof key !== 'string') return disks[key];
  if (Object.prototype.hasOwnProperty.call(disks, key)) return disks[key];
  if (key[0] !== '/') return disks[key];
  let best;
  let bestLen = -1;
  for (const m of Object.keys(disks)) {
    const match = m === '/' ? true : (key === m || key.startsWith(m + '/'));
    if (match && m.length > bestLen) { best = m; bestLen = m.length; }
  }
  return best === undefined ? undefined : disks[best];
}

module.exports = {
  getInfo: (p, isFolder) => native.getInfo(p, isFolder),
  copyFolder: (src, dest, cb) => native.copyFolder(src, dest, cb),
  cancelCopy: () => native.cancelCopy(),
  canReadAndWrite: (p) => native.canReadAndWrite(p),
  canRead: (p) => native.canRead(p),
  canWrite: (p) => native.canWrite(p),
  getDiskInfo: async () => {
    const disks = await native.getDiskInfo();
    return new Proxy(disks, {
      get(target, key) {
        if (typeof key === 'symbol') return target[key];
        return resolveByPath(target, key);
      },
    });
  },
};
