---
status: Accepted
date: 2026-11-24
deciders: ngxccc
---

# 1. Linux Autostart Opt-In Default, OS SSOT, and Flatpak/Host Separation

## Context

In [Issue #229](https://github.com/VN-Linux-Family/zalo-for-linux/issues/229), deleting `~/.config/autostart/Zalo.AppImage.desktop` and `~/.config/autostart/zalo.desktop` failed to persist across restarts on a Wayland/Umbriel environment where `org.freedesktop.portal.Background` is unavailable (`UnknownMethod`). Three coupled causes were identified in `app/main-dist/main.js`, `app/main-dist/compact-app.js`, and `scripts/patches/patch-auto-launch.js`:

1. Upstream Zalo's `configureLauncher` initializer evaluates `shouldEnable` as `!1 !== e.get("autolaunch")` (defaulting to `true` when unset) and reconciles `isEnabled() !== shouldEnable` on every startup by invoking `enable()`. Any external deletion of the XDG `.desktop` file is therefore overwritten on the next launch.
2. `__zaloWrapLinuxLauncher.enable()` in `scripts/patches/patch-auto-launch.js` calls `r.enable()` unconditionally even after `RequestBackground` succeeds, and `gdbus call ... RequestBackground` only checks D-Bus call acceptance rather than the asynchronous `org.freedesktop.portal.Request::Response` signal.
3. `LAUNCHER_OPTIONS_PATCHED` and `COMPACT_LAUNCHER_OPTIONS_PATCHED` set `e.path = process.env.APPIMAGE || i.getPath("exe")` without normalizing `e.name`, causing the bundled `auto-launch` module to write `<basename(APPIMAGE)>.desktop` (e.g., `Zalo.AppImage.desktop`) instead of `zalo.desktop`.

- **Target Module**: `scripts/patches/patch-auto-launch.js`
- **Spec Reference**: `docs/design/linux-autostart-ssot.md`

## Decision

1. **Opt-In Default & OS State as SSOT**: On Linux, change the startup autostart evaluation so it never auto-enables when `autolaunch` is unset and never auto-recreates a missing autostart entry on startup. OS state (`isEnabled()` from Portal in Flatpak or `~/.config/autostart/zalo.desktop` on host) is the Single Source of Truth; autostart entries are written or removed only on explicit user action via `toggleAutoLaunch`.
2. **Strict Flatpak vs Host Separation**:
   - **Flatpak (`FLATPAK_ID` or `/.flatpak-info`)**: Use `org.freedesktop.portal.Background.RequestBackground` and `PermissionStore` exclusively; do not invoke `r.enable()` / `r.disable()` inside the sandbox.
   - **Host (AppImage / `.deb` / Nix / tarball)**: Use XDG Autostart (`r.enable()` / `r.disable()`) exclusively; do not invoke `RequestBackground` on host.
3. **Canonical Desktop Entry Name**: Force `e.name = "zalo"` on Linux so host autostart always targets `~/.config/autostart/zalo.desktop` regardless of the AppImage filename, and clean up legacy `Zalo.AppImage.desktop` on `disable()` when applicable.

## Positive Consequences

- External removal of `~/.config/autostart/zalo.desktop` or DE autostart disabling is permanently respected across restarts.
- Eliminates duplicate `.desktop` entries and Flatpak sandbox autostart writes.
- Eliminates 5-second D-Bus timeout delays and silent `RequestBackground` rejections on non-Flatpak Wayland compositors (Umbriel, Sway, Hyprland) and X11 sessions.
- AppImage renames or versioned filenames (e.g., `Zalo-26.08.20.AppImage`) no longer orphan stale `.desktop` files.

## Negative Consequences / Risks

- **Behavior Change for New Linux Installs**: Fresh Linux installations will no longer auto-start on login by default; users must opt in via Zalo Settings.
- **No Portal Background-Apps Integration on Host**: Non-Flatpak builds (AppImage/`.deb`) will use `~/.config/autostart/zalo.desktop` rather than registering via `org.freedesktop.portal.Background` on GNOME/KDE.
- **Additional AST Patch Surface**: Patching upstream startup sync in `app/main-dist/main.js` and `compact-app.js` adds one more anchor to maintain across upstream Zalo updates.

## Explicit Tradeoffs

- **User Autonomy vs Upstream Parity**: Prioritize Linux XDG convention (opt-in autostart, respecting external file deletion) over Windows/macOS opt-out parity.
- **Deterministic XDG vs Host Portal**: Prioritize 100% deterministic `~/.config/autostart/zalo.desktop` behavior on host over partial `RequestBackground` support that fails silently without `Response` signal handling and matching `/usr/share/applications` app IDs.
