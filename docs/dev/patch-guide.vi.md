# Hướng dẫn viết và duy trì các bản vá (Patches)

[English](./patch-guide.md) | **Tiếng Việt**

Tài liệu này giải thích cơ chế hoạt động của các bản vá JavaScript trong **Zalo for Linux**, cách viết một bản vá mới, đăng ký vào build pipeline và duy trì patch khi Zalo cập nhật phiên bản mới.

---

## 1. Tổng quan

Zalo for Linux không biên dịch lại Zalo từ mã nguồn. Thay vào đó, dự án giải nén bộ cài macOS vào thư mục `app/` và áp dụng các bản vá trực tiếp lên các file bundle JavaScript trước khi đóng gói thành AppImage.

Toàn bộ các file patch nằm trong thư mục `scripts/patches/` với quy tắc đặt tên `patch-<tên-tính-năng>.js`.

Các loại bản vá chính trong dự án:
- **Tương thích nền tảng**: Xử lý đường dẫn XDG trên Linux, cờ sandbox và lệnh hệ thống (`patch-xdg-user-dirs.js`, `patch-shell-open-linux.js`).
- **Quản lý cửa sổ**: Bật thanh tiêu đề native trên X11 hoặc vẽ thanh tiêu đề riêng trên Wayland (`patch-titlebar.js`, `patch-wayland-titlebar.js`).
- **Native addons**: Điều hướng các file `.node` macOS sang các module viết lại cho Linux trong `nativelibs/` (`patch-sqlite3.js`, `patch-db-cross-v4.js`, `patch-zfile.js`).
- **Tính năng mở rộng**: Tích hợp engine gọi điện native (`patch-zcall-native.js`), tự động đổi theme ZaDark (`patch-auto-theme.js`), và chế độ offline (`patch-offline-startup.js`).

---

## 2. Cấu trúc tiêu chuẩn của một file Patch

Mỗi file patch bắt buộc phải export hàm bất đồng bộ `{ main }` và hỗ trợ chạy độc lập từ dòng lệnh:

```javascript
const fs = require('fs-extra');
const path = require('path');
const logger = require('../utils/logger');

const APP_DIR = path.join(__dirname, '..', '..', 'app');

async function main() {
  const targetPath = path.join(APP_DIR, 'main-dist', 'main.js');

  if (!fs.existsSync(targetPath)) {
    logger.warn('Không tìm thấy file mục tiêu, bỏ qua patch');
    return;
  }

  let content = fs.readFileSync(targetPath, 'utf8');

  const TARGET_PATTERN = /someMinifiedPattern\(\)/g;
  const REPLACEMENT = 'customLinuxImplementation()';

  // 1. Kiểm tra tính lũy biến (idempotency - đã patch chưa)
  if (content.includes(REPLACEMENT)) {
    logger.dim('Mục tiêu đã được patch từ trước');
    return;
  }

  // 2. Xác thực pattern mong đợi có tồn tại hay không
  if (!TARGET_PATTERN.test(content)) {
    logger.warn('Không tìm thấy pattern mong đợi; bỏ qua patch (code Zalo có thể đã đổi)');
    return;
  }

  // 3. Thực hiện thay thế và ghi lại file
  content = content.replace(TARGET_PATTERN, REPLACEMENT);
  fs.writeFileSync(targetPath, content, 'utf8');
  logger.dim('Đã áp dụng patch thành công vào main.js');
}

if (require.main === module) {
  main();
}

module.exports = { main };
```

### Các nguyên tắc bất di bất dịch:
1. **Tính lũy biến (Idempotence)**: Một patch phải an toàn khi chạy nhiều lần mà không làm nhân đôi mã nguồn hay gây lỗi. Luôn kiểm tra xem đoạn code thay thế đã có sẵn chưa.
2. **Không làm gãy build (Graceful Degradation)**: Không dùng `throw Error` làm sập toàn bộ build pipeline nếu một pattern phụ không khớp. Dùng `logger.warn()` để cảnh báo cho maintainer biết khi code minified của Zalo bị đổi.
3. **Chạy độc lập (Standalone)**: Khối `if (require.main === module) main();` giúp dev có thể test riêng một file patch qua lệnh `node scripts/patches/patch-<tên>.js`.

---

## 3. Đăng ký Patch vào Build Pipeline

Các patch được gọi tuần tự trong `scripts/prepare-app.js`.

Để đăng ký patch mới:
1. Mở file `scripts/prepare-app.js`.
2. Tìm khối `// Apply patches` bên trong hàm `extractAppAsar()`.
3. Import và `await` patch của bạn:

```javascript
// scripts/prepare-app.js
const { main: patchMyFeature } = require('./patches/patch-my-feature');
await patchMyFeature();
```

> **Lưu ý thứ tự thực thi**: Thứ tự rất quan trọng nếu nhiều patch cùng chỉnh sửa một file (như `app/main-dist/main.js`). Các patch điều hướng native addon nên chạy trước các patch giao diện.

---

## 4. Các file mục tiêu chính trong `app/`

| Đường dẫn file | Môi trường | Mô tả |
| :--- | :--- | :--- |
| `app/main-dist/main.js` | Electron Main | Quản lý vòng đời app, tạo cửa sổ BrowserWindow, xử lý IPC và cờ khởi động. |
| `app/main-dist/preload-*.js` | Electron Preload | Cầu nối contextBridge giữa renderer và main process. |
| `app/pc-dist/` | Renderer Process | Giao diện React, cài đặt người dùng, hiển thị tin nhắn và DOM styling. |

---

## 5. Kinh nghiệm xử lý Minification khi Zalo cập nhật

Vì Zalo bị nén và làm rối mã nguồn (obfuscated), tên biến và hàm cục bộ (`a`, `b`, `c`) thay đổi liên tục giữa các bản phát hành.

### Chiến thuật viết Regex bền vững:
- **Neo vào chuỗi ký tự cố định (String Literals)**: Minifier có thể đổi tên biến, nhưng các chuỗi ký tự (như `'darwin'`, `'ready'`, `'window-all-closed'`) thường được giữ nguyên.
- **Khớp theo cấu trúc, không khớp theo tên biến**: Sử dụng nhóm không chụp `(?:...)` hoặc `[\w$]+` thay vì hardcode tên biến tạm thời.
- **So sánh diff khi Zalo ra bản mới**: Nếu một patch báo warning sau khi Zalo nâng version, hãy mở `app/main-dist/main.js` mới và tìm kiếm chuỗi neo xung quanh để cập nhật lại regex.

---

## 6. Kiểm thử Patch

1. **Chạy unit tests**:
   ```bash
   npm test
   ```
2. **Kiểm thử độc lập trên file đã giải nén**:
   ```bash
   node scripts/patches/patch-<tên>.js
   ```
3. **Chạy toàn bộ pipeline chuẩn bị**:
   ```bash
   npm run main:setup
   ```
