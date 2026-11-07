# Native Modules and Engines

This directory consolidates all Linux native implementations replacing proprietary macOS/Windows components of Zalo:

```text
native/
├── builder.js                # Unified CLI builder helper (C++ node-gyp & Rust NAPI)
├── addons/                   # N-API cdylib addons loaded dynamically via require()
│   ├── db-cross-v4/          # Backup decryption & E2EE message synchronization (C++)
│   ├── file-utilities/       # Asynchronous filesystem scanning & pattern matching (Rust)
│   ├── file-utils/           # Filesystem access and file management utilities (Rust)
│   ├── mp4thumb/             # MP4 video thumbnail extractor (Rust)
│   ├── os-sys-info/          # Operating system metadata & PID inspector (Rust)
│   ├── v8-profiles/          # V8 CPU profiling addon (C++)
│   ├── zfile/                # Disk space, mount point & file copying utilities (C++)
│   ├── zimage/               # Image compression & conversion addon (Rust)
│   ├── zjxl/                 # JPEG XL decoder & encoder (Rust)
│   ├── zlang/                # Language detection engine (Rust)
│   └── zwalker/              # Media directory garbage collector (Rust)
└── engines/                  # Standalone native subsystems
    ├── zcall/                # VoIP calling engine (signaling, ZRTP/SRTP & Rust audio)
    └── zocr/                 # OCR engine host daemon & ONNX runtime integration (Rust)
```

## Building Native Addons

All native addons in `addons/` can be built using the unified builder:

```bash
# Build any C++ or Rust addon:
node native/builder.js native/addons/<addon-name>

# Examples:
node native/builder.js native/addons/db-cross-v4
node native/builder.js native/addons/zwalker
```

The unified builder automatically detects the toolchain (`binding.gyp` for C++ node-gyp, `Cargo.toml` for Rust) and outputs the compiled `.node` artifact.

## Rust Workspace

All Rust crates across `native/addons/` and `native/engines/` belong to the root Cargo workspace:

```bash
cargo test --workspace --locked
```
