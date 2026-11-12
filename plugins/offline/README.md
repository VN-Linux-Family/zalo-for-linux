# Offline Mode

**English** | [Tiếng Việt](./README.vi.md)

Offline Mode lets Zalo reuse the current account's locally cached session and the message databases already maintained by Zalo. It does not create a second contact or message store.

## Architecture

The feature is split across the Offline plugin and its startup patch:

- `index.js` owns the encrypted session cache, validates IPC senders, and serves the renderer source and storage requests.
- `store.js` reads and writes the encrypted session record.
- `renderer.js` owns the startup/reconnect state machine and the small status badge. It is kept independent of Zalo's Webpack module names so its state machine can be tested without Electron.
- `patch-offline-startup.js` adapts Zalo's extracted startup code. It connects the cached login response to the normal Zalo bootstrap and skips network-only work while Offline is active.

The patch runs after `patch-network-and-sync-fix.js`. It validates upstream anchors and fails rather than silently applying a partial patch when Zalo changes.

During Offline startup, Zalo still opens its own per-account SQLite/IndexedDB databases. The patch bypasses the Over-Queue cloud verification branch in the initial conversation-history loader, while leaving the local database read and dispatch intact. It also guards the message renderer against an absent `focusId` in Offline mode. The online path retains Zalo's original behavior.

## Session cache

A successful online login response is cached only when the account has a valid `zpw_sek` cookie. The cache is:

- Encrypted with AES-256-GCM.
- Keyed from the existing secure, HttpOnly `zpw_sek` cookies; no separate plaintext session key is stored.
- Written atomically with file mode `0600` under Electron's `userData` directory.
- Bound to the client ID and account UID. Missing or changed cookies, corrupt data, a different client ID, or a different UID prevent restoration.

Logout, account replacement, and rejected sessions invalidate the cache. Generation checks prevent an outstanding write from recreating a cache after invalidation.

## Startup and reconnection

- A cached online login attempt is bounded by an eight-second timeout. Only network failures allow the cached response to be used.
- Offline startup reports its state to the main process and retries authentication every ten seconds, as well as on the browser's `online` event.
- Only one reconnect attempt runs at a time; each attempt has a fifteen-second timeout.
- Once authentication succeeds and local UI initialization is ready, the page reloads once so Zalo can apply the fresh server response and synchronize normally.
- Session rejection clears the cache and returns to Zalo's normal login flow.

The Shared Worker asks the main process for the current Offline state through a separate, validated IPC request. The main process accepts state reports only from the trusted main frame of the local login/index pages; the Shared Worker may read the state but cannot publish it.

## Development and validation

After extracting an app, apply the network/sync patch and Offline startup patch, then restart the development app:

```sh
node scripts/patches/patch-network-and-sync-fix.js
node scripts/patches/patch-offline-startup.js
npm start
```

Open Zalo online once with **Remember Login** enabled to seed the cache. The Offline feature tests cover session storage, startup/recovery behavior, and IPC renderer delivery:

```sh
node --test tests/offline.test.js
```

Run the full suite with `npm test`.

Manual acceptance with a real account:

1. Open Zalo online with Remember Login enabled. Open several chats, then quit normally.
2. Disconnect the network and restart. Verify the account, contacts, groups, and previously downloaded messages. Open multiple cached chats and check app lock.
3. Reconnect. Verify one automatic reload, incoming messages, and updated UI.
4. Repeat with Wi-Fi connected but no internet uplink, and after suspend/resume.
5. Log out, quit, and start Offline. The previous session must not be restored. Repeat after switching accounts and with a server-revoked session.

## Limitations

Only data already downloaded by Zalo is available Offline. First login, uncached messages, attachments that are not locally available, and messages sent while disconnected require a network connection. Cookie expiry also requires online authentication; Offline Mode does not extend or recreate session cookies.
