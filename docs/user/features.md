# Features added in this project that original Zalo doesn't have ✨


This document show the added features included in **Zalo for Linux**, which doesn't have in the official version.

---

## Open Zalo offline

After opening this build online once with a remembered login, you can restart
Zalo without internet and read the last account's locally stored chat history,
contacts and groups. An offline indicator is shown. Messages or attachments
that have never been downloaded are unavailable offline.

Zalo retries authentication in the background. Once the server accepts the
session, the interface reloads automatically and normal synchronization resumes.
Logging out or receiving a rejected session clears the offline session cache.

## 🙈 Start hidden in the tray

When **Settings → Launch Zalo on startup** is enabled, Zalo starts minimized
to the system tray at login. The autostart entry it writes to
`~/.config/autostart/` launches it with `--hidden`. If you enabled the setting
with an older build, turn it off and on again to update the entry.

You can also pass the flag yourself (`--start-hidden` works too):

```bash
/path/to/zalo --hidden
```

Open the window from the tray icon, by clicking a notification, or by launching
Zalo again. If no system tray is available, the flag is ignored so the window
is never unreachable.

## ⚙️ Advanced Linux settings

Open **Settings → Nâng cao...** to customize the ringtone (incoming calls) and
ringback (played locally while an outgoing call waits for an answer). Enter an
absolute audio file path or browse for a file, preview it, then save. Each sound
can be reset to Zalo's default. Settings persist across restarts and apply on the
next playback; missing or unplayable custom files fall back to the default.

The Userscript Manager opens in the same settings window, with a transition and
a back button. Returning to Advanced keeps the script editor's current content.

## 🧩 Userscripts manager

Open **Settings → Nâng cao... → Userscript Manager** to create, paste, edit, import, delete,
and enable or disable scripts that run inside Zalo. Tampermonkey-style metadata
such as `@name`, `@description`, `@version`, `@match`, `@include`, and
`@exclude` is recognized. Imported scripts may use the `.js` or `.user.js`
extension.

The compatibility layer currently provides `GM_info`, `GM_addStyle`,
`GM_getValue`, `GM_setValue`, `GM_deleteValue`, `GM_listValues`, and
`unsafeWindow`. Changes take effect the next time the Zalo page is loaded.

> **Security:** Userscripts execute with access to the current Zalo page and
> messages displayed in it. Only install scripts whose source you trust.

## 🌙 ZaDark Integration

This project includes integrated [ZaDark](https://github.com/quaric/zadark), ZaDark is an extension that helps you enable Dark Mode, more privacy features, and additional functionality.

**ZaDark helps you experience Zalo 🔒 more privately ✨ more personalized.**

### Features

- 🌙 **Dark Mode optimized specifically for Zalo** - Complete dark theme tailored for Zalo interface
- 🆃 **Customize fonts and font sizes** - Personalize text appearance to your preference
- 🖼️ **Custom chat backgrounds** - Set personalized backgrounds for conversations
- 🔤 **Quick message translation** - Instantly translate messages to your preferred language
- 😊 **Express emotions with 80+ Emojis** - Enhanced emoji reactions for messages
- 🔒 **Anti-message peeking protection** - Prevent others from secretly viewing your messages
- 👁️ **Hide status indicators** - Hide "typing", "delivered" and "read" status from others
- 📱 **Native Integration** - Seamlessly integrated during build process

> **Note:** ZaDark is licensed under MPL-2.0 and is developed by [Quaric](https://zadark.com). The setup process automatically prepares ZaDark, and build process integrates it seamlessly!

