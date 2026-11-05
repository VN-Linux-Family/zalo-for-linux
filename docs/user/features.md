# Feature Overview ✨

**English** | [Tiếng Việt](./features.vi.md)

This document highlights features added to **Zalo for Linux** that are not present in the official Zalo client.

---

### 🌐 1. Offline Mode

Read locally cached chats, contacts, and groups when working without an internet connection.

- **How it works**: Open Zalo online once with "Remember Login" checked. If internet is disconnected later, Zalo boots into offline mode with a visual indicator.
- **Auto-reconnect**: Zalo retries authentication in the background and seamlessly reloads once network access returns.
- **Notes**: Uncached attachments or messages sent during downtime remain unavailable until reconnection. Logging out clears the offline session cache.

---

### 🙈 2. Start Minimized to System Tray

Launch Zalo hidden in the system tray on startup or from the command line.

- **How to enable**:
  - **In App**: Navigate to **Settings → General** and enable **"Launch Zalo on system startup"**.
  - **CLI Command**: Launch directly with `--hidden` or `--start-hidden`:
    ```bash
    /path/to/zalo --hidden
    ```
- **How to open window**: Click the tray icon, click an incoming notification, or launch Zalo again.
- **Failsafe**: If no system tray is detected in your desktop environment, the flag is ignored so the window is never lost.

---

### ⚙️ 3. Advanced Linux Audio Settings

Customize call ringtones and audio playback without modifying application assets.

- **How to access**: Open **Settings → Nâng cao... (Advanced)**.
- **Configurable sounds**:
  - **Ringtone**: Custom audio file for incoming calls.
  - **Ringback**: Custom audio file played while waiting for outgoing calls to connect.
- **Notes**: Supports browsing local audio files with built-in preview and reset-to-default buttons. Persists across restarts.

---

### 🧩 4. Built-in Userscripts Manager

Run custom Tampermonkey-compatible JavaScript extensions directly inside Zalo.

- **How to access**: Open **Settings → Nâng cao... → Userscript Manager**.
- **Supported features**:
  - Create, edit, paste, import (`.js`, `.user.js`), toggle, and delete scripts.
  - Supports Tampermonkey metadata: `@name`, `@description`, `@version`, `@match`, `@include`, `@exclude`.
  - Implemented GM APIs: `GM_info`, `GM_addStyle`, `GM_getValue`, `GM_setValue`, `GM_deleteValue`, `GM_listValues`, and `unsafeWindow`.
- **Security warning**: Userscripts run with full DOM access to messages and chat views. Only install scripts from sources you trust.

---

### 🌙 5. Integrated ZaDark Theme

Native dark mode, privacy enhancements, and custom UI personalization powered by [ZaDark](https://github.com/quaric/zadark).

- **Dark Mode**: Fully optimized dark theme matching modern Linux desktop aesthetics.
- **Privacy Protections**: Anti-peeking message blurring and optional hidden typing/read indicators.
- **Customization**: Custom conversation wallpapers, font overrides, quick translation, and 80+ emoji reactions.
- **Integration**: Pre-compiled and bundled directly into the AppImage during build.
