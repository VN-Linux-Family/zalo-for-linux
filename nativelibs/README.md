# nativelibs

**English** | [Tiếng Việt](./README.vi.md)
Linux reimplementations of Zalo's proprietary macOS native addons.

## Why?

The official Zalo desktop app for macOS uses closed-source native addons
(`.node` files) for various features. Some of these are essential
(send/receive messages, decrypt backups). The macOS versions are Mach-O
binaries and cannot be loaded on Linux.

Since the original source is not available, we use **clean-room reverse
engineering** based on disassembly of the macOS binaries. Each
reimplementation is rebuilt from source during the build process — no
proprietary binaries are committed to this repository.

## Directory Structure

```
nativelibs/
├── README.md                       # This file
├── builder.js                      # CLI build helper
└── <addon-name>/                   # One folder per addon
    ├── .gitignore                  # ignore node_modules/, build/
    ├── README.md                   # addon-specific docs
    ├── binding.gyp                 # node-gyp config
    ├── package.json                # Node package manifest
    └── src/
        └── main.cc                 # C++ source
```

`builder.js` at the top level is the shared build helper used by all
addons. Each addon is otherwise self-contained.

## Available Addons

| Addon | Status | Description |
|-------|--------|-------------|
| [db-cross-v4](./db-cross-v4) | ✅ Implemented | Backup decryption & E2EE message synchronization |
| [file-utilities](./file-utilities) | ✅ Implemented | Asynchronous filesystem scanning & pattern matching |
| [file-utils](./file-utils) | ✅ Implemented | Filesystem access and file management utilities |
| [mp4thumb](./mp4thumb) | ✅ Implemented | Video thumbnail extraction from MP4 files |
| [os-sys-info](./os-sys-info) | ✅ Implemented | Operating system and process information reader |
| [v8-profiles](./v8-profiles) | ✅ Implemented | Native CPU profiler for Zalo |
| [zfile](./zfile) | ✅ Implemented | Disk/volume info (free space, mount points) |
| [zimage](./zimage) | ✅ Implemented | Image processing, resizing, and format conversions |
| [zjxl](./zjxl) | ✅ Implemented | JPEG XL (JXL) image decoding and encoding support |
| [zwalker](./zwalker) | ✅ Implemented | Media-cache garbage collecting support |
| [zcall-audio](./zcall-audio) | ✅ Implemented | Native PulseAudio/PipeWire low-latency audio engine for calls |
| [zlang](./zlang) | ✅ Implemented | Natural language identification for Linux (whatlang) |

## Building an Addon

### For gyp-based addons:

```bash
node nativelibs/builder.js nativelibs/<addon-name>
```

### For Rust-based addons:

```bash
node nativelibs/builder-rust.js nativelibs/<addon-name>
```

The compiled binary stays in:
`<project-root>/nativelibs/<addon-name>/build/Release/*.node # gyp-based addons`

`<project-root>/nativelibs/<addon-name>/target/release/*.node # Rust-based addons`

This path matches the structure expected by the Zalo app's JS bindings.

## Build Helper

`builder.js` and `builder-rust.js` is a CLI script, it reads the Electron version from the project root `package.json` automatically.

## Adding a New Addon

To add a new reimplementation:

1. **Create the folder structure** — copy an existing addon as a template:
   ```bash
   cp -r nativelibs/db-cross-v4 nativelibs/<new-addon>
   ```

2. **Update the static files**:
   - `binding.gyp` — change `target_name` and `sources`
   - `package.json` — change `name` and `description`
   - `src/main.cc` — replace with your C++ implementation

3. **Update `README.md`** — document what the addon does and any
   platform-specific notes

4. **Add a patch script** — each addon typically needs a patch script in
   `scripts/patches/` that handles building and any JS binding patches

## Requirements

Building addons requires:

- Node.js (matches project version)
- C++ compiler (gcc/clang)
- `node-gyp` (installed via npm)
- OpenSSL development headers (`libssl-dev`) — for crypto addons
- LZMA development headers (`liblzma-dev`) — for compression addons

- **Debian / Ubuntu**:
  ```bash
  sudo apt install -y build-essential libssl-dev liblzma-dev clang libclang-dev llvm
  ```
- **Arch Linux**:
  ```bash
  sudo pacman -S base-devel openssl xz clang llvm
  ```
- **Fedora / RHEL**:
  ```bash
  sudo dnf install @development-tools openssl-devel xz-devel clang llvm
  ```

## Source vs. Binary

**We do not commit prebuilt `.node` binaries.** The `build/` and
`node_modules/` directories in each addon are gitignored. The binary is
always rebuilt from source during the build pipeline.

This ensures:
- The build is **reproducible** — anyone can regenerate the exact same
  binary from the C++ source
- **No proprietary code** ships in our repository
- **No supply chain risk** from committing a binary blob of unknown
  origin
- Security researchers can **review the source**

## Credits

Reimplementations are based on reverse engineering work by
[realdtn2](https://github.com/realdtn2), [DMKha241](https://github.com/DMKha241) and [thotam](https://github.com/thotam). See each
addon's `README.md` for specific attribution.
