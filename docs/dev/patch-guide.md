# Patch Authoring and Maintenance Guide

This document explains how JavaScript patches work in **Zalo for Linux**, how to author new patches, register them into the build pipeline, and maintain them across upstream Zalo updates.

---

## 1. Overview

Zalo for Linux does not recompile Zalo from source. Instead, it extracts the official macOS application bundle into `app/` and dynamically patches the extracted JavaScript bundles before packaging the application as an AppImage.

All patch scripts live in `scripts/patches/` with the naming convention `patch-<feature-name>.js`.

Patches address several classes of issues:
- **Platform compatibility**: Adapting macOS-specific paths, sandbox flags, and shell commands to Linux (`patch-xdg-user-dirs.js`, `patch-shell-open-linux.js`).
- **Window management**: Native titlebars on X11 vs frameless Wayland titlebar fallback (`patch-titlebar.js`, `patch-wayland-titlebar.js`).
- **Native addons**: Redirecting closed-source macOS `.node` bindings to Linux clean-room replacements in `nativelibs/` (`patch-sqlite3.js`, `patch-db-cross-v4.js`, `patch-zfile.js`).
- **Media and features**: Integrating the native call engine (`patch-zcall-native.js`), ZaDark themes (`patch-auto-theme.js`), and offline mode (`patch-offline-startup.js`).

---

## 2. Patch Anatomy and Structure

Every patch file must export an asynchronous `{ main }` function and support direct CLI execution:

```javascript
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const APP_DIR = path.join(__dirname, '..', '..', 'app');

async function main() {
  const targetPath = path.join(APP_DIR, 'main-dist', 'main.js');

  if (!fs.existsSync(targetPath)) {
    logger.warn('Target file not present, skipping patch');
    return;
  }

  let content = fs.readFileSync(targetPath, 'utf8');

  const TARGET_PATTERN = /someMinifiedPattern\(\)/g;
  const REPLACEMENT = 'customLinuxImplementation()';

  // 1. Check if already patched (idempotency)
  if (content.includes(REPLACEMENT)) {
    logger.dim('Target already patched');
    return;
  }

  // 2. Verify target pattern exists before replacing
  if (!TARGET_PATTERN.test(content)) {
    logger.warn('Expected pattern not found; skipping patch (Zalo code may have changed)');
    return;
  }

  // 3. Apply replacement and write back
  content = content.replace(TARGET_PATTERN, REPLACEMENT);
  fs.writeFileSync(targetPath, content, 'utf8');
  logger.dim('Successfully applied patch to main.js');
}

if (require.main === module) {
  main();
}

module.exports = { main };
```

### Essential Invariants
1. **Idempotence**: A patch must be safe to run multiple times without duplicating or corrupting code. Always check if the replacement token already exists.
2. **Graceful Degradation**: Never crash the whole build pipeline if a non-critical pattern fails to match. Log a descriptive warning using `logger.warn()` so maintainers know when an upstream update changes minified identifiers.
3. **Stand-alone Executability**: Include `if (require.main === module) main();` to allow testing individual patches via `node scripts/patches/patch-<name>.js`.

---

## 3. Registering Patches in the Build Pipeline

Patches are executed sequentially during the preparation phase by `scripts/prepare-app.js`.

To register a new patch:
1. Open `scripts/prepare-app.js`.
2. Locate the `// Apply patches` block inside `extractAppAsar()`.
3. Import and await your patch:

```javascript
// scripts/prepare-app.js
const { main: patchMyFeature } = require('./patches/patch-my-feature');
await patchMyFeature();
```

> **Execution Order Notice**: Order matters when multiple patches modify the same file (e.g. `app/main-dist/main.js`). Place low-level native addon redirections before UI and feature patches.

---

## 4. Key Target Files in `app/`

| Target Path | Environment | Description |
| :--- | :--- | :--- |
| `app/main-dist/main.js` | Electron Main | Primary process bundle: window creation, app lifecycle, protocol handlers, IPC listeners. |
| `app/main-dist/preload-*.js` | Electron Preload | Context bridge scripts bridging renderer windows and the main process. |
| `app/pc-dist/` | Renderer Process | Frontend React/web bundle: DOM UI, settings views, chat interfaces, styles. |

---

## 5. Handling Upstream Minification & Updates

Because Zalo releases are minified and obfuscated, variable names and function signatures frequently change between versions.

### Strategies for Robust Matching:
- **Anchor against string literals**: Minifiers compress identifiers (`a`, `b`, `c`), but string literals (e.g. `'mac'`, `'darwin'`, event names `'ready'`, `'window-all-closed'`) usually remain stable.
- **Match structure, not local variable names**: Use flexible regular expressions with non-capturing groups `(?:...)` rather than hardcoding temporary variable names.
- **Inspect changes across versions**: When an upstream update causes a patch warning, extract the new `app/main-dist/main.js` and search for surrounding string anchors to inspect the updated minified syntax.

---

## 6. Testing Patches

1. **Unit and Integration Tests**: Run existing test suites covering patch verification:
   ```bash
   npm test
   ```
2. **Isolated Execution**: Test a single patch directly against an extracted `app/` tree:
   ```bash
   node scripts/patches/patch-<name>.js
   ```
3. **Full Build Verification**: Run the full preparation pipeline to verify all patches apply cleanly:
   ```bash
   npm run main:setup
   ```
