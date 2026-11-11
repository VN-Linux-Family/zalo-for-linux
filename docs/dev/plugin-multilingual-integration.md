# Language Core, Bridge, and Multilingual Plugin Integration

**English** | [Tiếng Việt](./plugin-multilingual-integration.vi.md)

Multilingual support in Zalo for Linux consists of three parts: Language Core, the bridge that distributes locale information across renderers and processes, and the contract plugins use to localize their own UI.

For the overall system design, see [Architecture](./architecture.md).

> **Core principle:** Zalo is the only source of truth for the active locale. Language Core reads the locale and emits notifications; each plugin owns its dictionary and updates its own UI.

## 1. Scope and principles

The integration must ensure that:

- Plugin UI updates immediately when the user changes Zalo's language.
- Plugins use one shared contract instead of discovering the locale themselves.
- Dictionaries and rendering logic remain plugin-owned, not part of Language Core.
- Zalo's language preference, storage, and business logic remain untouched.
- Existing IPC, callbacks, shortcuts, state, permissions, layout, and lifecycle are preserved.

The supported locales are exactly `vi` and `en`. Do not add another locale without updating the contract, schema, and corresponding tests.

### Responsibility boundaries

| Component | Responsibility |
|---|---|
| Zalo | Chooses the active locale. |
| Language Core | Reads the locale applied by Zalo, emits `LANGUAGE_UPDATE_REQUEST`, and manages registrations within a renderer. |
| Plugin | Owns its dictionary, translates UI, re-renders on requests, and cleans up its registration. |
| Main-process relay | Forwards an authenticated locale snapshot to separate plugin windows when required. |

Do not make Core read dictionaries or choose translated strings on behalf of plugins. Do not translate data that is not plugin UI, such as contact names, filenames, chat content, or verbatim operating-system messages.

## 2. Renderer/process boundaries and locale routing

`window.ZaloLanguage` is available only in renderers where Language Core has been injected. Identify the process/renderer before implementing integration:

| Plugin surface | Locale source |
|---|---|
| UI inside a Zalo renderer with Core | Register directly through `window.ZaloLanguage`. |
| A separate window created by a plugin | Use the application's existing main-process locale relay; do not assume the window has `window.ZaloLanguage`. |
| Electron Tray/Menu or UI created in the main process | Use the main-process locale snapshot/callback; do not access `window`. |
| A plugin with multiple renderers | Distribute the authenticated locale to each renderer; each renderer renders its own dictionary. |

Do not create another locale transport if the application already has a suitable relay. Before changing code, inspect `plugins/index.js`, `main.js`, the preload, and the relevant plugin lifecycle. Reuse the existing contract; do not copy an IPC channel without verifying its sender, frame, URL, and window ownership.

## 3. Language Core public API

In a renderer where Core is present, the API is exposed as `window.ZaloLanguage`:

```ts
type Locale = 'vi' | 'en';

type LanguageUpdateRequest = Readonly<{
  type: 'LANGUAGE_UPDATE_REQUEST';
  requestId: string;
  locale: Locale;
  previousLocale: Locale | null;
  source: 'initial' | 'zalo';
}>;

type PluginRegistration = {
  id: string;
  onLanguageUpdateRequest(request: LanguageUpdateRequest): void | Promise<void>;
};

interface ZaloLanguageAPI {
  getLocale(): Locale | null;
  getStatus(): Readonly<{
    ready: boolean;
    locale: Locale | null;
    source: string;
    bridge: 'zalo-locale-bridge' | 'readonly-singleton' | 'unavailable';
    registeredPlugins: number;
    lastRequestId: string | null;
    dispatchedRequests: number;
    dispatchErrors: number;
  }>;
  registerPlugin(options: PluginRegistration): () => boolean;
}
```

Usage rules:

- `id` must contain lowercase letters, digits, and hyphens, matching `^[a-z0-9][a-z0-9-]*$`. It must be unique within that renderer.
- `registerPlugin()` returns an unregister function. Call it when the plugin is disposed or unloaded.
- `getLocale()` may return `null`; do not choose a default or guess the locale.
- The API has no setter. Do not add `setLocale()` or `changeLanguage()` yourself.

### Request lifecycle

- If the locale is known at registration time, Core immediately sends a request with `source: 'initial'` and `previousLocale: null`.
- If the locale is unknown, Core sends no synthetic request; the plugin waits for a valid locale.
- When Zalo actually changes locale, Core sends a request with `source: 'zalo'` and the previous locale.
- If the new locale is the same as the current locale, Core does not broadcast again.
- Synchronous handler errors and rejected Promises are isolated; other plugins still receive the request.
- Unregistering prevents future requests. Calling the unregister function more than once is safe.
- The API and registry are per-renderer; register the plugin in every renderer where it runs.

## 4. Create a plugin dictionary

Each plugin owns its own `locale.json`. The canonical schema is `plugins/language-core/locale-schema.json`.

```json
{
  "schemaVersion": 1,
  "plugin": "my-plugin",
  "messages": {
    "my-plugin.panel.title": {
      "vi": "Cài đặt của tôi",
      "en": "My settings"
    },
    "my-plugin.action.save": {
      "vi": "Lưu",
      "en": "Save"
    },
    "my-plugin.status.items": {
      "vi": "Có {count} mục",
      "en": "{count} items"
    }
  }
}
```

Conventions:

- `plugin` must match the plugin id; every key must use that namespace.
- Use lowercase, semantic keys, for example `my-plugin.settings.language.title`.
- Every key must contain exactly `vi` and `en`, both strings.
- Do not reuse another plugin's keys or scatter translated strings through UI logic.
- Placeholders must be compatible across locales, such as `{count}` and `{name}`.
- Do not use one locale's text as the fallback for the other. Tests must catch missing keys.
- The plugin loads and renders its dictionary. Language Core does not load, validate, or look up dictionaries.

## 5. Example: integrate in a Zalo renderer

This example demonstrates the dictionary lookup, translation helper, rendering, and registration lifecycle. Adapt JSON loading to the plugin's bundler/runtime.

```js
const dictionary = require('./locale.json').messages;
let currentLocale = null;
let unregisterLanguage = null;

function t(key, params = {}) {
  const entry = dictionary[`my-plugin.${key}`];
  const template = entry && entry[currentLocale];

  if (typeof template !== 'string') {
    console.warn('[my-plugin] Missing translation:', key, currentLocale);
    return `my-plugin.${key}`;
  }

  return template.replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (_match, name) => {
    const value = params[name];
    return value == null ? '' : String(value);
  });
}

function render() {
  const title = document.querySelector('[data-role="title"]');
  const save = document.querySelector('[data-action="save"]');
  if (title) title.textContent = t('panel.title');
  if (save) save.textContent = t('action.save');
}

function onLanguageUpdateRequest(request) {
  if (request?.type !== 'LANGUAGE_UPDATE_REQUEST') return;
  if (request.locale !== 'vi' && request.locale !== 'en') return;

  currentLocale = request.locale;
  render();
}

function startLanguageSupport() {
  const api = window.ZaloLanguage;
  if (!api || typeof api.registerPlugin !== 'function') {
    console.warn('[my-plugin] Language Core is unavailable in this renderer.');
    return false;
  }

  unregisterLanguage = api.registerPlugin({
    id: 'my-plugin',
    onLanguageUpdateRequest
  });
  return true;
}

function stopLanguageSupport() {
  if (typeof unregisterLanguage === 'function') {
    unregisterLanguage();
    unregisterLanguage = null;
  }
  currentLocale = null;
}

startLanguageSupport();
window.addEventListener('pagehide', stopLanguageSupport, { once: true });
```

Notes:

- Define the handler and render functions before registering because Core may send the initial request synchronously from `registerPlugin()`.
- Do not render with a hard-coded locale before the first request. While the locale is `null`, keep the UI neutral or defer locale-dependent labels.
- If the plugin mounts/unmounts repeatedly, prevent duplicate registrations and always unregister on disposal.
- Do not add a new `pagehide` listener on every mount unless it has matching cleanup.

## 6. Locale bridge: separate windows and the main process

Separate plugin windows and Electron Tray/Menu do not automatically have `window.ZaloLanguage`. Use the application's existing locale relay. The high-level flow is:

```text
Zalo primary renderer
  └─ Language Core
       └─ trusted, one-way locale transport (vi/en)
            └─ Main process validates sender/frame/URL
                 ├─ updates in-memory locale snapshot
                 └─ notifies the owned plugin/window
                      └─ plugin renders its own dictionary
```

Security and lifecycle requirements:

1. Only an authorized Zalo renderer is the global locale source; secondary plugin windows must not publish locale.
2. Accept exactly `vi` or `en` in both preload and main process.
3. Validate the sender, main frame, URL, and window ownership before updating the snapshot.
4. Keep the snapshot in memory only; do not write it to settings, files, or a database.
5. When a window opens or reloads, send the latest snapshot after its renderer is ready.
6. On locale changes, send a one-way notification to the correct window; the renderer calls the plugin's render function.
7. Remove listeners and references when the window closes.
8. Do not expose raw `ipcRenderer` or add IPC that lets a secondary plugin window change Zalo's locale.

Example locale receiver:

```js
function onLocaleChanged(locale) {
  if (locale !== 'vi' && locale !== 'en') return;
  currentLocale = locale;
  render();
}
```

This is only an example of the receiving-side handler. The actual channel and API must follow the relay implemented in the repository. Do not create a new channel just to match this example.

## 7. UI rendering rules

Every locale change must update all plugin-owned UI strings:

- Titles, labels, placeholders, tooltips, `aria-label`, and accessible descriptions.
- Buttons, menu items, context menus, and action text.
- Plugin-defined dialogs, confirmations, empty/loading/error states.
- Running, completed, failed, warning, and call statuses.
- Dynamically created nodes after the initial render.
- Content currently on screen; users should not have to close and reopen the window.

Centralize UI updates in `render()` or `renderX(locale)`. Store business data (status codes, counts, names, durations), not translated sentences as primary state. When state changes, render from state + current locale.

For statuses from an engine/native layer, send stable status codes and parameters; let the renderer look up the dictionary. Do not send hard-coded Vietnamese/English sentences from the engine. For Electron Menus created in the main process, rebuild labels for the new locale while preserving callbacks, checked/enabled state, ordering, and items owned by other plugins.

## 8. Required tests

Every plugin adding multilingual support must have its own tests or use a dedicated plugin-integration suite. Do not mix plugin dictionary/UI tests into Language Core's internal tests.

**Dictionary**

- JSON parses and follows `locale-schema.json`.
- Plugin id and key namespace match.
- Every key has both `vi` and `en` string values.
- Placeholders are compatible across locales.
- No hard-coded UI labels remain in the migrated scope.

**Core contract and rendering**

- The plugin registers the correct id and handler.
- Invalid request types or locales are ignored.
- Both locales render correctly; the initial request is handled.
- Changing locale while the UI is open updates it without resetting business state.
- Unregister/dispose prevents further callbacks.
- A failing handler does not break other plugins.

**Separate-window/main-process relay (if applicable)**

- A window opened after a locale change receives the latest snapshot.
- Reload/reopen receives the correct locale.
- Invalid senders, subframes, URLs, or locales are rejected.
- Listeners/references are removed when the window closes.
- No API/IPC allows a secondary plugin window to change Zalo's locale.

Run the targeted tests, then `npm test` and `git diff --check`. Smoke-test by switching Zalo between `vi` and `en` while the plugin remains open.

## 9. Plugin multilingual-integration flow

Integration flow for connecting a plugin UI to Language Core and the locale bridge. Keep the plugin’s existing behavior and lifecycle outside the localization changes.

1. Identify the process/renderer and list all UI strings owned by the plugin.
2. Choose `window.ZaloLanguage` or the existing locale relay; do not invent a locale source.
3. Create `locale.json` using the schema and plugin-id namespace.
4. Add a small `t(key, params)` helper inside the plugin.
5. Replace UI strings with keys; keep status codes/data rather than translated sentences in state.
6. Register the handler, handle the initial request, re-render on locale changes, and unregister at the right lifecycle point.
7. Add tests for the dictionary, rendering, dynamic state, and cleanup.
8. Review IPC boundaries if the plugin uses a separate window or the main process.
9. Smoke-test both `vi` ↔ `en` directions while the UI is active.


## 10. Language Core runtime and Zalo Locale Bridge

The build pipeline uses `scripts/patches/patch-zalo-language-bridge.js` to implement the **Zalo Locale Bridge** at the point where Zalo completes a locale update. The bridge publishes a snapshot and emits a page-level event:

```js
window.__ZALO_LANGUAGE_BRIDGE__ = {
  version: 1,
  locale: 'vi', // or 'en'
  timestamp: Date.now()
};
window.dispatchEvent(new CustomEvent('zalo-language:changed', {
  detail: window.__ZALO_LANGUAGE_BRIDGE__
}));
```

Core registers the event listener before reading the snapshot so it cannot miss a locale change that happened before Core initialized. It accepts only version 1 snapshots with locale `vi`/`en` and a finite timestamp; stale snapshots and events that do not match the current state are ignored.

If the Zalo Locale Bridge event source is unavailable, Core may use a read-only adapter for an existing singleton in the Webpack cache, but only when exactly one candidate is identified. It fails closed when multiple candidates exist. It does not use `html[lang]` as a fallback because that attribute does not confirm the active locale.

Core only reads the snapshot/event or subscribes to a read-only callback. An unknown locale remains `null`; Core does not choose a default, write storage, call language-changing APIs, or synchronize changes back to Zalo.

In the main process, Language Core is registered before userscripts. Only the primary Zalo renderer (`index.html`/`login.html`) may send locale to Main; secondary renderers only receive relayed snapshots. The API and registry are isolated per renderer. Page exit and app disposal clean up the corresponding listeners, callbacks, and registry.

