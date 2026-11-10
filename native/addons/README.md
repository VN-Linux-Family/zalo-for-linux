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
native/addons/
├── README.md                       # This file
└── <addon-name>/                   # One folder per addon
    ├── .gitignore                  # ignore node_modules/, build/
    ├── README.md                   # addon-specific docs
    ├── binding.gyp / Cargo.toml    # build manifest
    └── src/                        # source code
```

`native/builder.js` at the parent level is the unified build helper used by all
addons.

## Available Addons

| Addon | Status | Description |
|-------|--------|-------------|
| [db-cross-v4](./db-cross-v4) | ✅ Implemented | Backup decryption & E2EE message synchronization |
| [file-utilities](./file-utilities) | ✅ Implemented | Asynchronous filesystem scanning & pattern matching |
| [file-utils](./file-utils) | ✅ Implemented | Filesystem access and file management utilities |
| [mp4thumb](./mp4thumb) | ✅ Implemented | Video thumbnail extraction from MP4 files |
| [os-sys-info](./os-sys-info) | ✅ Implemented | Operating system and process information reader |
| [zfile](./zfile) | ✅ Implemented | Disk/volume info (free space, mount points) |
| [zimage](./zimage) | ✅ Implemented | Image processing, resizing, and format conversions |
| [zjxl](./zjxl) | ✅ Implemented | JPEG XL (JXL) image decoding and encoding support |
| [zwalker](./zwalker) | ✅ Implemented | Media-cache garbage collecting support |
| [zcall-audio](../engines/zcall-audio) | ✅ Implemented | Native PulseAudio/PipeWire low-latency audio engine for calls |
| [zlang](./zlang) | ✅ Implemented | Natural language identification for Linux (whatlang) |

## Building an Addon

```bash
node native/builder.js native/addons/<addon-name>
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

Reimplementations are based on clean-room reverse engineering work by
[@ngxccc](https://github.com/ngxccc) ([alolite](https://github.com/ngxccc/alolite)), [DMKha241](https://github.com/DMKha241) and [thotam](https://github.com/thotam). See each
addon's `README.md` for specific attribution.
