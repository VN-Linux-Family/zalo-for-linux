#include <napi.h>
#include <unistd.h>
#include <string>
#include <sys/statvfs.h>
#include <mntent.h>
#include <cstdio>
#include <vector>
#include <set>
#include <cstdint>
#include <dirent.h>
#include <sys/stat.h>
#include <cerrno>
#include <atomic>
#include <fcntl.h>

// ---- sync access checks: access(2) → boolean, never throws ----
static Napi::Value CanAccess(const Napi::CallbackInfo& info, int mode) {
  Napi::Env env = info.Env();
  if (info.Length() < 1 || !info[0].IsString()) {
    return Napi::Boolean::New(env, false);
  }
  std::string path = info[0].As<Napi::String>().Utf8Value();
  return Napi::Boolean::New(env, access(path.c_str(), mode) == 0);
}
static Napi::Value CanRead(const Napi::CallbackInfo& info) { return CanAccess(info, R_OK); }
static Napi::Value CanWrite(const Napi::CallbackInfo& info) { return CanAccess(info, W_OK); }
static Napi::Value CanReadAndWrite(const Napi::CallbackInfo& info) { return CanAccess(info, R_OK | W_OK); }

// ---- getDiskInfo: /proc/mounts + statvfs, filtered ----
struct DiskEntry { std::string path; uint64_t total; uint64_t used; bool isExternal; };

static bool IsRealFs(const std::string& fstype, const std::string& dev) {
  static const std::set<std::string> kPseudo = {
    "proc","sysfs","tmpfs","devtmpfs","devpts","cgroup","cgroup2","overlay",
    "squashfs","mqueue","debugfs","tracefs","securityfs","pstore","bpf",
    "configfs","hugetlbfs","autofs","fusectl","binfmt_misc","ramfs","nsfs",
    "rpc_pipefs","fuse.gvfsd-fuse","fuse.portal","efivarfs"
  };
  if (kPseudo.count(fstype)) return false;
  static const std::set<std::string> kAllow = {
    "ext2","ext3","ext4","btrfs","xfs","vfat","exfat","ntfs","ntfs3",
    "fuseblk","f2fs","zfs","jfs","reiserfs"
  };
  if (dev.rfind("/dev/", 0) == 0) return true;
  return kAllow.count(fstype) > 0;
}

class DiskInfoWorker : public Napi::AsyncWorker {
 public:
  explicit DiskInfoWorker(Napi::Env env)
      : Napi::AsyncWorker(env), deferred(Napi::Promise::Deferred::New(env)) {}
  Napi::Promise GetPromise() { return deferred.Promise(); }

 protected:
  void Execute() override {
    FILE* f = setmntent("/proc/mounts", "r");
    if (!f) return;
    struct mntent mntbuf;
    char strbuf[4096];
    struct mntent* m;
    std::set<std::string> seen;
    while ((m = getmntent_r(f, &mntbuf, strbuf, sizeof(strbuf))) != nullptr) {
      std::string dir = m->mnt_dir, type = m->mnt_type, dev = m->mnt_fsname;
      if (seen.count(dir)) continue;
      if (!IsRealFs(type, dev)) continue;
      struct statvfs vfs;
      if (statvfs(dir.c_str(), &vfs) != 0) continue;
      uint64_t frsize = vfs.f_frsize ? vfs.f_frsize : vfs.f_bsize;
      uint64_t total = (uint64_t)vfs.f_blocks * frsize;
      uint64_t used = (vfs.f_blocks >= vfs.f_bfree)
                          ? (uint64_t)(vfs.f_blocks - vfs.f_bfree) * frsize
                          : 0;
      if (total == 0) continue;
      seen.insert(dir);
      bool ext = dir.rfind("/media/", 0) == 0 || dir.rfind("/run/media/", 0) == 0
                 || dir.rfind("/mnt/", 0) == 0;
      entries.push_back({dir, total, used, ext});
    }
    endmntent(f);
  }
  void OnOK() override {
    Napi::Env env = Env();
    Napi::HandleScope scope(env);
    Napi::Object out = Napi::Object::New(env);
    for (const auto& e : entries) {
      Napi::Object o = Napi::Object::New(env);
      o.Set("name", e.path);
      o.Set("label", e.path);
      o.Set("path", e.path);
      o.Set("isExternal", Napi::Boolean::New(env, e.isExternal));
      o.Set("totalSpace", Napi::Number::New(env, (double)e.total));
      o.Set("usedSpace", Napi::Number::New(env, (double)e.used));
      out.Set(e.path, o);
    }
    deferred.Resolve(out);
  }
  void OnError(const Napi::Error& e) override { deferred.Reject(e.Value()); }

 private:
  Napi::Promise::Deferred deferred;
  std::vector<DiskEntry> entries;
};

static Napi::Value GetDiskInfo(const Napi::CallbackInfo& info) {
  DiskInfoWorker* w = new DiskInfoWorker(info.Env());
  Napi::Promise p = w->GetPromise();
  w->Queue();
  return p;
}

// ---- getInfo: stat file, or recursive walk folder ----
static std::string BaseName(const std::string& p) {
  size_t s = p.find_last_of('/');
  return s == std::string::npos ? p : p.substr(s + 1);
}
static void WalkDir(const std::string& dir, uint64_t& size, uint64_t& count) {
  DIR* d = opendir(dir.c_str());
  if (!d) return;
  struct dirent* ent;
  while ((ent = readdir(d)) != nullptr) {
    std::string n = ent->d_name;
    if (n == "." || n == "..") continue;
    std::string full = dir + "/" + n;
    struct stat st;
    if (lstat(full.c_str(), &st) != 0) continue;
    if (S_ISDIR(st.st_mode)) WalkDir(full, size, count);
    else if (S_ISREG(st.st_mode)) { size += (uint64_t)st.st_size; count++; }
  }
  closedir(d);
}

class InfoWorker : public Napi::AsyncWorker {
 public:
  InfoWorker(Napi::Env env, std::string path, bool isFolder)
      : Napi::AsyncWorker(env), deferred(Napi::Promise::Deferred::New(env)),
        path_(std::move(path)), isFolder_(isFolder) {}
  Napi::Promise GetPromise() { return deferred.Promise(); }

 protected:
  void Execute() override {
    struct stat st;
    if (stat(path_.c_str(), &st) != 0) { exists_ = false; return; }
    exists_ = true;
    isDir_ = S_ISDIR(st.st_mode);
    mtimeMs_ = (double)st.st_mtime * 1000.0;
    if (isFolder_) { WalkDir(path_, size_, fileCount_); }
    else { size_ = (uint64_t)st.st_size; }
  }
  void OnOK() override {
    Napi::Env env = Env();
    Napi::HandleScope scope(env);
    if (!exists_) {
      deferred.Reject(Napi::Error::New(env, "ENOENT: no such file or directory: " + path_).Value());
      return;
    }
    Napi::Object o = Napi::Object::New(env);
    o.Set("size", Napi::Number::New(env, (double)size_));
    o.Set("isDirectory", Napi::Boolean::New(env, isDir_));
    o.Set("name", BaseName(path_));
    o.Set("lastWriteTime", Napi::Number::New(env, mtimeMs_));
    if (isFolder_) o.Set("fileCount", Napi::Number::New(env, (double)fileCount_));
    deferred.Resolve(o);
  }
  void OnError(const Napi::Error& e) override { deferred.Reject(e.Value()); }

 private:
  Napi::Promise::Deferred deferred;
  std::string path_;
  bool isFolder_;
  bool exists_ = false;
  bool isDir_ = false;
  uint64_t size_ = 0;
  uint64_t fileCount_ = 0;
  double mtimeMs_ = 0;
};

static Napi::Value GetInfo(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  std::string path = info.Length() > 0 && info[0].IsString()
                         ? info[0].As<Napi::String>().Utf8Value() : "";
  bool isFolder = info.Length() > 1 && info[1].ToBoolean().Value();
  InfoWorker* w = new InfoWorker(env, path, isFolder);
  Napi::Promise p = w->GetPromise();
  w->Queue();
  return p;
}

// ---- copyFolder / cancelCopy ----
static std::atomic<bool> g_cancelCopy{false};

static bool CopyOneFile(const std::string& src, const std::string& dst) {
  int in = open(src.c_str(), O_RDONLY);
  if (in < 0) return false;
  struct stat st;
  if (fstat(in, &st) != 0) { close(in); return false; }
  int out = open(dst.c_str(), O_WRONLY | O_CREAT | O_TRUNC, st.st_mode & 0777);
  if (out < 0) { close(in); return false; }
  char buf[65536];
  ssize_t n;
  bool ok = true;
  while ((n = read(in, buf, sizeof(buf))) > 0) {
    ssize_t off = 0;
    while (off < n) {
      ssize_t w = write(out, buf + off, n - off);
      if (w < 0) { ok = false; break; }
      off += w;
    }
    if (!ok) break;
  }
  if (n < 0) ok = false;
  close(in);
  close(out);
  return ok;
}

// returns: 0 ok, 1 error, 2 cancelled
static int CopyDirRec(const std::string& src, const std::string& dst) {
  if (g_cancelCopy.load()) return 2;
  if (mkdir(dst.c_str(), 0755) != 0 && errno != EEXIST) return 1;
  DIR* d = opendir(src.c_str());
  if (!d) return 1;
  struct dirent* ent;
  int rc = 0;
  while ((ent = readdir(d)) != nullptr) {
    if (g_cancelCopy.load()) { rc = 2; break; }
    std::string n = ent->d_name;
    if (n == "." || n == "..") continue;
    std::string s = src + "/" + n, t = dst + "/" + n;
    struct stat st;
    if (lstat(s.c_str(), &st) != 0) { rc = 1; break; }
    if (S_ISDIR(st.st_mode)) {
      int sub = CopyDirRec(s, t);
      if (sub != 0) { rc = sub; break; }
    } else if (S_ISREG(st.st_mode)) {
      if (!CopyOneFile(s, t)) { rc = 1; break; }
    }
    // other types (symlink/device) skipped
  }
  closedir(d);
  return rc;
}

class CopyWorker : public Napi::AsyncWorker {
 public:
  CopyWorker(Napi::Env env, std::string src, std::string dst)
      : Napi::AsyncWorker(env), deferred(Napi::Promise::Deferred::New(env)),
        src_(std::move(src)), dst_(std::move(dst)) {}
  Napi::Promise GetPromise() { return deferred.Promise(); }

 protected:
  void Execute() override { rc_ = CopyDirRec(src_, dst_); }
  void OnOK() override {
    Napi::Env env = Env();
    Napi::HandleScope scope(env);
    if (rc_ == 2) deferred.Reject(Napi::Error::New(env, "CANCELLED").Value());
    else if (rc_ == 1) deferred.Reject(Napi::Error::New(env, "copyFolder failed: " + src_).Value());
    else deferred.Resolve(Napi::String::New(env, dst_));
  }
  void OnError(const Napi::Error& e) override { deferred.Reject(e.Value()); }

 private:
  Napi::Promise::Deferred deferred;
  std::string src_, dst_;
  int rc_ = 0;
};

static Napi::Value CopyFolder(const Napi::CallbackInfo& info) {
  Napi::Env env = info.Env();
  std::string src = info.Length() > 0 && info[0].IsString() ? info[0].As<Napi::String>().Utf8Value() : "";
  std::string dst = info.Length() > 1 && info[1].IsString() ? info[1].As<Napi::String>().Utf8Value() : "";
  g_cancelCopy.store(false);  // reset before starting a new copy
  CopyWorker* w = new CopyWorker(env, src, dst);
  Napi::Promise p = w->GetPromise();
  w->Queue();
  return p;
}
static Napi::Value CancelCopy(const Napi::CallbackInfo& info) {
  g_cancelCopy.store(true);
  return info.Env().Undefined();
}

Napi::Object Init(Napi::Env env, Napi::Object exports) {
  exports.Set("canRead", Napi::Function::New(env, CanRead));
  exports.Set("canWrite", Napi::Function::New(env, CanWrite));
  exports.Set("canReadAndWrite", Napi::Function::New(env, CanReadAndWrite));
  exports.Set("getDiskInfo", Napi::Function::New(env, GetDiskInfo));
  exports.Set("getInfo", Napi::Function::New(env, GetInfo));
  exports.Set("copyFolder", Napi::Function::New(env, CopyFolder));
  exports.Set("cancelCopy", Napi::Function::New(env, CancelCopy));
  return exports;
}
NODE_API_MODULE(zfile_native, Init)
