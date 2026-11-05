# Known Issues & Troubleshooting 🛠️

**English** | [Tiếng Việt](./known-issues.vi.md)

Quick diagnostic lookup table for common runtime and configuration issues on Linux:

| Symptom / Error | Root Cause | Quick Fix |
| :--- | :--- | :--- |
| **Crash on launch (Ubuntu 23.10+)** | Unprivileged user namespaces restricted by AppArmor | Add `--no-sandbox` to Zalo launcher command or `.desktop` entry, or follow [AM Ubuntu Troubleshooting](https://github.com/ivan-hc/AM/blob/main/docs/troubleshooting.md#ubuntu-mess) to disable the restriction completely. |
| **Calls fail (Audio / Mic / Video)** | Missing system Opus codecs and PulseAudio utilities | Install required packages:<br>• **Debian / Ubuntu**: `sudo apt install python3 libopus0 pulseaudio-utils`<br>• **Arch Linux**: `sudo pacman -S python opus pulseaudio`<br>• **Fedora / RHEL**: `sudo dnf install python3 opus pulseaudio-utils`<br>• **openSUSE**: `sudo zypper install python3 libopus0 pulseaudio-utils`<br>• **Gentoo**: `sudo emerge --ask dev-lang/python media-libs/opus media-sound/pulseaudio-daemon` |
| **Cannot paste images (`Ctrl+V`)** | Missing clipboard tool for Wayland/X11 | Install `wl-clipboard` (Wayland) or `xclip` (X11). |
| **Dark mode does not follow system** | Application defaults to static theme | Built-in real-time detection via GNOME D-Bus and XDG Desktop Portal is enabled in current builds. |
| **Missing desktop shortcut** | Manual AppImage execution without integration | If installed via AM, run `am reinstall zalo`. Otherwise, use [Gear Lever](https://flathub.org/en/apps/it.mijorus.gearlever) to integrate into desktop menu, or create `~/.local/share/applications/zalo.desktop`. |

---

## ⚠️ Active Limitations

- **Calls on ARM64 (`aarch64`)**: The call engine runs on a native Node.js + Python stack (`zcall-native/`, eliminating x86-only Wine dependencies) and has been tested and verified working on ARM64 by community tester @lolmc0587 ([Issue #70](https://github.com/VN-Linux-Family/zalo-for-linux/issues/70)).
- **Ubuntu 23.10+ Sandbox Restrictions**: AppArmor blocks unprivileged user namespaces used by Electron's `chrome-sandbox`. Run with `--no-sandbox` or follow [AM Ubuntu Troubleshooting](https://github.com/ivan-hc/AM/blob/main/docs/troubleshooting.md#ubuntu-mess) ([Issue #72](https://github.com/VN-Linux-Family/zalo-for-linux/issues/72), [Issue #143](https://github.com/VN-Linux-Family/zalo-for-linux/issues/143)).

---

## ✅ Resolved Issues & Technical History

<details>
<summary><b>Click to expand full resolved bug history and community credits</b></summary>

- **Calls (Voice, Video, Screen Sharing, Group Calls):**
  - **Issue:** Calls could not be made or received on Linux.
  - **Solution:** Calls run on a native Linux call engine ([`zcall-native/`](../../zcall-native/README.md), from [zalo-linux-native](https://github.com/nct88/zalo-linux-native)). No Wine, no `ZaloCall.exe`, no 32-bit libraries.
  - **Credits:** [@nct88](https://github.com/nct88), [@collyn](https://github.com/collyn). See [PR #62](https://github.com/VN-Linux-Family/zalo-for-linux/pull/62), [PR #128](https://github.com/VN-Linux-Family/zalo-for-linux/pull/128), [PR #129](https://github.com/VN-Linux-Family/zalo-for-linux/pull/129), [PR #135](https://github.com/VN-Linux-Family/zalo-for-linux/pull/135), [PR #136](https://github.com/VN-Linux-Family/zalo-for-linux/pull/136), [PR #137](https://github.com/VN-Linux-Family/zalo-for-linux/pull/137).

- **System / Auto Theme Synchronization:**
  - **Issue:** Zalo and ZaDark ignored `prefers-color-scheme`.
  - **Solution:** Real-time theme detection via GNOME D-Bus (`org.gnome.desktop.interface color-scheme`), XDG Desktop Portal (`org.freedesktop.appearance`), or GTK theme. Synchronizes `nativeTheme.themeSource` and DOM `.dark` class without touching titlebar frames.
  - **Credits:** [@nct88](https://github.com/nct88). See [PR #87](https://github.com/VN-Linux-Family/zalo-for-linux/pull/87), [Issue #21](https://github.com/VN-Linux-Family/zalo-for-linux/issues/21), [Issue #22](https://github.com/VN-Linux-Family/zalo-for-linux/issues/22).

- **Message Synchronization (E2EE) & Info Panel:**
  - **Issue:** Missing `db-cross-v4` native module caused end-to-end encryption messages, conversation info panel media, and emoji reactions to fail on Linux.
  - **Solution:** Reimplemented clean-room `db-cross-v4` addon in C++.
  - **Credits:** [@realdtn2](https://github.com/realdtn2), [@DMKha2k7](https://github.com/DMKha2k7). See [PR #24](https://github.com/VN-Linux-Family/zalo-for-linux/pull/24), [Issue #15](https://github.com/VN-Linux-Family/zalo-for-linux/issues/15).

- **Pasting & Sending Images:**
  - **Issue:** Pressing `Ctrl+V` failed to paste copied images, and sending images defaulted to raw file attachments instead of inline photos.
  - **Solution:** Clipboard image files are pasted via `wl-clipboard` (Wayland) and `xclip` (X11).
  - **Credits:** [@realdtn2](https://github.com/realdtn2), [@DMKha2k7](https://github.com/DMKha2k7) ([PR #25](https://github.com/VN-Linux-Family/zalo-for-linux/pull/25)), [@brucenguyen1102](https://github.com/brucenguyen1102) ([PR #34](https://github.com/VN-Linux-Family/zalo-for-linux/pull/34)).

- **Linux Auto-Launch on Startup:**
  - **Issue:** Built-in "Start Zalo on system startup" setting failed on Linux.
  - **Solution:** Patched IPC handlers to generate standard autostart entry at `~/.config/autostart/zalo.desktop`.
  - **Credits:** [@akimiya7742](https://github.com/akimiya7742), [@DMKha241](https://github.com/DMKha241). See [PR #51](https://github.com/VN-Linux-Family/zalo-for-linux/pull/51), [PR #59](https://github.com/VN-Linux-Family/zalo-for-linux/pull/59), [PR #61](https://github.com/VN-Linux-Family/zalo-for-linux/pull/61).

- **XDG User Directories & Download Path:**
  - **Issue:** Zalo forced an English `~/Zalo Received Files` directory in `$HOME`.
  - **Solution:** Patched file download handlers to respect standard `$XDG_DOWNLOAD_DIR`.
  - **Credits:** [@akimiya7742](https://github.com/akimiya7742). See [PR #52](https://github.com/VN-Linux-Family/zalo-for-linux/pull/52), [Issue #20](https://github.com/VN-Linux-Family/zalo-for-linux/issues/20), [Issue #48](https://github.com/VN-Linux-Family/zalo-for-linux/issues/48).

- **Unread Message Badge Count:**
  - **Issue:** Dock/tray icon showed no badge indicator for unread messages.
  - **Solution:** Implemented badge counter support for Unity/KDE/GNOME dock protocols and custom number renderer over tray icon.
  - **Credits:** [@akimiya7742](https://github.com/akimiya7742) ([PR #43](https://github.com/VN-Linux-Family/zalo-for-linux/pull/43)), [@namtao](https://github.com/namtao) ([PR #151](https://github.com/VN-Linux-Family/zalo-for-linux/pull/151)).

- **In-App Screenshot Tool:**
  - **Issue:** Built-in screen capture failed on Linux.
  - **Solution:** Integrated native Linux screenshot tools (`spectacle`, `flameshot`, `gnome-screenshot`, `scrot`).
  - **Credits:** [@hthienloc](https://github.com/hthienloc). See [Issue #19](https://github.com/VN-Linux-Family/zalo-for-linux/issues/19).

- **Freeze on Login Screen:**
  - **Issue:** App hung when displaying the login window.
  - **Solution:** Replaced macOS SQLite3 binaries with native Linux builds ([Issue #13](https://github.com/VN-Linux-Family/zalo-for-linux/issues/13)).

</details>
