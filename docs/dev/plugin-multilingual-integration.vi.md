# Language Core, Bridge và tích hợp đa ngôn ngữ cho plugin

[English](./plugin-multilingual-integration.md) | **Tiếng Việt**

Hệ thống đa ngôn ngữ trong Zalo for Linux gồm ba phần: Language Core, bridge phân phối thông tin locale giữa các renderer/process và contract để plugin tự bản địa hóa giao diện.

Để hiểu kiến trúc tổng thể, xem [Kiến trúc](./architecture.vi.md).

> **Nguyên tắc cốt lõi:** Zalo là nguồn locale duy nhất. Language Core chỉ đọc locale Zalo và phát thông báo; mỗi plugin tự sở hữu dictionary và tự cập nhật giao diện.

## 1. Phạm vi và nguyên tắc

Mục tiêu của tích hợp:

- Giao diện plugin cập nhật ngay khi người dùng đổi ngôn ngữ trong Zalo.
- Các plugin dùng chung một contract, không tự dò locale.
- Dictionary và logic hiển thị thuộc về plugin, không thuộc Language Core.
- Không thay đổi lựa chọn ngôn ngữ, storage hoặc luồng nghiệp vụ của Zalo.
- Giữ nguyên IPC, callback, shortcut, state, quyền, layout và lifecycle hiện có.

Hiện tại chỉ hỗ trợ chính xác hai locale: `vi` và `en`. Không tự thêm locale khác nếu chưa cập nhật contract, schema và test tương ứng.

### Ranh giới trách nhiệm

| Thành phần | Trách nhiệm |
|---|---|
| Zalo | Quyết định locale đang hoạt động. |
| Language Core | Đọc locale đã được Zalo áp dụng; phát `LANGUAGE_UPDATE_REQUEST`; quản lý đăng ký trong renderer. |
| Plugin | Sở hữu dictionary, dịch UI, render lại khi nhận request và dọn đăng ký khi kết thúc. |
| Main-process relay | Chuyển snapshot locale đã xác thực tới cửa sổ/plugin riêng khi cần. |

Không để Core đọc dictionary hoặc quyết định câu chữ thay plugin. Không dịch dữ liệu không thuộc UI plugin, chẳng hạn tên liên hệ, tên file, nội dung chat hoặc thông báo nguyên văn từ hệ điều hành.

## 2. Ranh giới renderer/process và định tuyến locale

`window.ZaloLanguage` chỉ có trong renderer đã được inject Language Core. Xác định đúng process/renderer trước khi viết tích hợp:

| Bề mặt plugin | Cách nhận locale |
|---|---|
| UI chạy trong renderer Zalo có Core | Đăng ký trực tiếp qua `window.ZaloLanguage`. |
| Cửa sổ riêng do plugin tạo | Dùng relay locale main-process hiện có; không giả định cửa sổ có `window.ZaloLanguage`. |
| Electron Tray/Menu hoặc UI tạo từ main process | Dùng snapshot/callback locale ở main process; không truy cập `window`. |
| Plugin có nhiều renderer | Phân phối locale đã xác thực tới từng renderer; mỗi renderer tự render dictionary của mình. |

Không tạo đường truyền locale mới nếu ứng dụng đã có relay phù hợp. Trước khi sửa, kiểm tra `plugins/index.js`, `main.js`, preload và lifecycle của plugin liên quan. Tái sử dụng contract hiện có, không sao chép IPC channel mà chưa xác minh sender, frame, URL và quyền sở hữu cửa sổ.

## 3. Public API của Language Core

Trong renderer có Core, API được expose dưới `window.ZaloLanguage`:

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

Quy tắc sử dụng:

- `id` dùng chữ thường, số và dấu gạch ngang, theo mẫu `^[a-z0-9][a-z0-9-]*$`; id phải duy nhất trong renderer.
- `registerPlugin()` trả về hàm unregister. Gọi hàm này khi plugin dispose/unload.
- `getLocale()` có thể trả `null`; không tự chọn locale mặc định hoặc suy đoán.
- API không có setter. Plugin không được tự bổ sung `setLocale()` hoặc `changeLanguage()`.

### Vòng đời request

- Nếu locale đã biết khi đăng ký, Core gửi ngay request `source: 'initial'`, `previousLocale: null`.
- Nếu locale chưa biết, Core không gửi request giả; plugin chờ locale hợp lệ.
- Khi Zalo thực sự đổi locale, Core gửi request `source: 'zalo'` cùng locale trước đó.
- Nếu locale mới trùng locale hiện tại, Core không broadcast lại.
- Handler lỗi đồng bộ hoặc Promise reject được cô lập; plugin khác vẫn nhận request.
- Unregister ngăn plugin nhận request tiếp theo. Gọi unregister nhiều lần an toàn.
- Registry và API độc lập theo từng renderer; plugin phải đăng ký ở mỗi renderer nơi nó chạy.

## 4. Tạo dictionary cho plugin

Mỗi plugin sở hữu file `locale.json` của mình. Schema chuẩn: `plugins/language-core/locale-schema.json`.

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

Quy ước:

- `plugin` phải trùng plugin id; mọi key phải nằm trong namespace đó.
- Key dùng chữ thường, phân cấp theo ý nghĩa, ví dụ `my-plugin.settings.language.title`.
- Mỗi key phải có đúng hai thuộc tính `vi` và `en`, đều là string.
- Không dùng lại key của plugin khác, không đặt câu dịch trực tiếp rải rác trong logic UI.
- Placeholder phải tương thích giữa hai locale, ví dụ `{count}` và `{name}`.
- Không dùng câu dịch của locale này làm fallback cho locale kia. Thiếu key phải được phát hiện bằng test.
- Dictionary do plugin đọc và render; Language Core không tải, validate hoặc tra cứu dictionary.

## 5. Ví dụ tích hợp trong renderer Zalo

Ví dụ dưới đây minh họa dictionary, hàm dịch, render và đăng ký lifecycle. Điều chỉnh cách nạp JSON theo bundler/runtime của plugin.

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

Lưu ý:

- Khai báo handler và hàm render trước khi đăng ký vì Core có thể gửi request initial ngay trong `registerPlugin()`.
- Không render bằng locale tự đặt trước khi nhận request đầu tiên. Khi locale là `null`, giữ UI trung tính hoặc chưa hiển thị nhãn phụ thuộc locale.
- Nếu plugin mount/unmount nhiều lần, bảo đảm không đăng ký trùng và luôn unregister khi dispose.
- Không thêm listener `pagehide` lặp lại mỗi lần mount nếu không có cleanup tương ứng.

## 6. Locale bridge: cửa sổ riêng và main process

Cửa sổ riêng và Electron Tray/Menu không tự động có `window.ZaloLanguage`. Dùng relay locale đã có trong ứng dụng. Luồng khái quát:

```text
Zalo primary renderer
  └─ Language Core
       └─ trusted, one-way locale transport (vi/en)
            └─ Main process validates sender/frame/URL
                 ├─ updates in-memory locale snapshot
                 └─ notifies the owned plugin/window
                      └─ plugin renders its own dictionary
```

Yêu cầu an toàn và lifecycle:

1. Chỉ renderer Zalo được phép mới là nguồn locale toàn cục; cửa sổ plugin phụ không được publish locale.
2. Chỉ chấp nhận chính xác `vi` hoặc `en` ở preload và main process.
3. Xác thực sender, main frame, URL và quyền sở hữu cửa sổ trước khi cập nhật snapshot.
4. Snapshot chỉ lưu trong RAM; không ghi settings, file hoặc database.
5. Khi cửa sổ mở/reload, gửi snapshot mới nhất sau khi renderer sẵn sàng.
6. Khi locale đổi, gửi thông báo một chiều tới đúng cửa sổ; renderer gọi hàm render của plugin.
7. Khi cửa sổ đóng, gỡ listener và reference liên quan.
8. Không expose `ipcRenderer` thô và không thêm IPC cho plugin phụ đổi locale Zalo.

Ví dụ phần render nhận locale từ relay:

```js
function onLocaleChanged(locale) {
  if (locale !== 'vi' && locale !== 'en') return;
  currentLocale = locale;
  render();
}
```

Đây chỉ là ví dụ tại điểm nhận locale; channel và API cụ thể phải theo relay đang được triển khai trong repository. Không tự tạo channel mới chỉ để dùng ví dụ này.

## 7. Quy tắc render giao diện

Mỗi lần đổi locale phải cập nhật toàn bộ chuỗi UI do plugin sở hữu:

- Tiêu đề, label, placeholder, tooltip, `aria-label` và accessible description.
- Button, menu item, context menu và action text.
- Dialog, confirm, empty/loading/error state do plugin định nghĩa.
- Trạng thái hoạt động, hoàn tất, thất bại, cảnh báo và trạng thái.
- Node được tạo động sau lần render đầu tiên.
- Nội dung đang hiển thị khi locale đổi, không bắt người dùng đóng/mở lại cửa sổ.

Nên tập trung cập nhật UI vào `render()` hoặc `renderX(locale)`. Lưu dữ liệu nghiệp vụ (status code, count, tên, thời lượng), không lưu câu đã dịch làm state chính. Khi state thay đổi, render lại từ state + locale hiện tại.

Với trạng thái đến từ engine/native layer, truyền mã trạng thái ổn định và params; renderer tra dictionary. Không gửi nguyên câu tiếng Việt/Anh từ engine. Với Electron Menu tạo ở main process, dựng lại label theo locale mới nhưng giữ nguyên callback, checked/enabled state, thứ tự và item do plugin khác sở hữu.

## 8. Kiểm thử bắt buộc

Mỗi plugin thêm đa ngôn ngữ phải có test riêng hoặc suite tích hợp plugin riêng. Không gộp test dictionary/UI của plugin vào test nội bộ Language Core.

**Dictionary**

- JSON parse được và tuân thủ `locale-schema.json`.
- Plugin id và namespace key khớp nhau.
- Mỗi key có đủ `vi`/`en`, đều là string.
- Placeholder giữa hai locale tương thích.
- Không còn nhãn UI hardcode trong phạm vi đã chuyển dịch.

**Core contract và render**

- Đăng ký đúng plugin id và handler.
- Request sai type hoặc locale bị bỏ qua.
- Hai locale đều render đúng; initial request hoạt động.
- Đổi locale khi UI đang mở cập nhật ngay mà không reset state nghiệp vụ.
- Unregister/dispose ngăn callback tiếp tục chạy.
- Handler lỗi không làm hỏng plugin khác.

**Relay riêng/main process (nếu có)**

- Cửa sổ mở sau khi locale đổi nhận snapshot mới nhất.
- Reload/reopen nhận đúng locale.
- Sender, subframe, URL hoặc locale không hợp lệ bị từ chối.
- Đóng cửa sổ thì listener/reference được dọn.
- Không có API/IPC cho plugin phụ đổi locale Zalo.

Chạy test mục tiêu, sau đó `npm test` và `git diff --check`. Smoke-test bằng cách đổi Zalo `vi` ↔ `en` khi plugin đang mở.

## 9. Luồng tích hợp đa ngôn ngữ cho plugin

Luồng tích hợp giao diện plugin với Language Core và locale bridge. Giữ nguyên hành vi và lifecycle sẵn có của plugin ngoài những thay đổi phục vụ bản địa hóa.

1. Xác định process/renderer và liệt kê UI text do plugin sở hữu.
2. Chọn `window.ZaloLanguage` hoặc relay locale hiện có; không tự tạo nguồn locale.
3. Tạo `locale.json` theo schema và namespace plugin id.
4. Tạo helper `t(key, params)` trong phạm vi plugin.
5. Chuyển UI text sang key; giữ status code/dữ liệu thay vì câu dịch trong state.
6. Đăng ký handler, xử lý initial request, cập nhật khi locale đổi và unregister đúng lifecycle.
7. Viết test dictionary, render, dynamic state và cleanup.
8. Kiểm tra ranh giới IPC nếu plugin có cửa sổ riêng/main process.
9. Smoke-test hai chiều `vi` ↔ `en` khi UI đang hoạt động.


## 10. Runtime Language Core và Zalo Locale Bridge

Build pipeline dùng `scripts/patches/patch-zalo-language-bridge.js` để triển khai **Zalo Locale Bridge** tại điểm Zalo hoàn tất cập nhật locale. Bridge công bố snapshot và phát event page-level:

```js
window.__ZALO_LANGUAGE_BRIDGE__ = {
  version: 1,
  locale: 'vi', // hoặc 'en'
  timestamp: Date.now()
};
window.dispatchEvent(new CustomEvent('zalo-language:changed', {
  detail: window.__ZALO_LANGUAGE_BRIDGE__
}));
```

Core đăng ký event listener trước khi đọc snapshot để không bỏ lỡ thay đổi xảy ra trước lúc Core khởi tạo. Chỉ chấp nhận snapshot version 1, locale `vi`/`en` và timestamp hữu hạn; snapshot cũ hoặc event không khớp trạng thái hiện tại bị bỏ qua.

Nếu nguồn event của Zalo Locale Bridge không khả dụng, Core chỉ dùng adapter đọc-only trên singleton đã tồn tại trong Webpack cache khi xác định được duy nhất một ứng viên. Nếu có nhiều ứng viên, Core không đoán. Không dùng `html[lang]` làm nguồn thay thế vì thuộc tính này không xác nhận locale đang hoạt động.

Core chỉ đọc snapshot/event hoặc đăng ký callback chỉ đọc. Locale chưa xác định được giữ là `null`; Core không chọn mặc định, không ghi storage, không gọi API đổi ngôn ngữ và không đồng bộ ngược về Zalo.

Trong main process, Language Core được đăng ký trước userscripts. Chỉ renderer Zalo chính (`index.html`/`login.html`) được gửi locale lên Main; renderer phụ chỉ nhận snapshot relay. API và registry tồn tại riêng trong từng renderer. Khi renderer rời trang hoặc app dispose, listener, callback và registry tương ứng được dọn.

