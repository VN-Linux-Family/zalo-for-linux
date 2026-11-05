# Hướng dẫn phát triển

[English](./development.md) | **Tiếng Việt**

Tài liệu này hướng dẫn cách build Zalo for Linux từ mã nguồn, bao gồm bộ công cụ build, các script hỗ trợ và quy trình thêm patch hoặc native addon mới.

Để nắm tổng quan cách dự án hoạt động, xem [architecture.vi.md](./architecture.vi.md). Để tìm hiểu về việc viết lại các native addon, xem [nativelibs/README.vi.md](../../nativelibs/README.vi.md).

## Yêu cầu môi trường

- Linux x86_64 hoặc aarch64
- Node.js (phiên bản 22+) và npm
- `7z` (`p7zip-full`) để giải nén gói ứng dụng macOS
- Bộ công cụ biên dịch C++ và Rust cho các native addon (xem [nativelibs/README.vi.md](../../nativelibs/README.vi.md))

Cài đặt trên Debian/Ubuntu:

```bash
sudo apt-get update && sudo apt-get install -y p7zip-full build-essential libssl-dev liblzma-dev clang libclang-dev llvm
```

## Bắt đầu nhanh

```bash
# 1. Clone repository
git clone https://github.com/VN-Linux-Family/zalo-for-linux.git
cd zalo-for-linux

# 2. Khởi tạo các submodule (ZaDark, v.v.)
git submodule update --init --recursive

# 3. Setup + build (tải DMG, giải nén, vá code, đóng gói AppImage)
npm run main
```

File AppImage đầu ra sẽ nằm tại: `dist/Zalo-<version>.AppImage`

## Build hai giai đoạn (Two-Phase Build)

Bạn có thể chạy riêng phần chuẩn bị mã nguồn và phần đóng gói:

```bash
# Giai đoạn 1: Tải DMG, giải nén vào app/ và áp dụng các bản vá
npm run main:setup

# Chạy thử ứng dụng đã giải nén ở chế độ lập trình viên:
npm run start

# Giai đoạn 2: Đóng gói AppImage từ thư mục app/
npm run main:build
```

## Thêm một bản vá mới (Adding a New Patch)

> Hướng dẫn toàn diện và quy chuẩn viết patch chống trôi minification, xem chi tiết tại [patch-guide.vi.md](./patch-guide.vi.md).

Các bản vá nằm riêng từng file trong `scripts/patches/patch-<tên>.js`.

1. Tạo file patch mới:
```javascript
const fs = require('fs-extra');
const path = require('path');
const APP_DIR = path.join(__dirname, '..', '..', 'app');

async function main() {
  const targetPath = path.join(APP_DIR, 'main-dist', 'main.js');
  if (!fs.existsSync(targetPath)) return;

  let content = fs.readFileSync(targetPath, 'utf8');
  if (content.includes('OLD_PATTERN')) {
    content = content.replace(/OLD_PATTERN/g, 'NEW_PATTERN');
    fs.writeFileSync(targetPath, content, 'utf8');
  }
}

module.exports = { main };
```

2. Đăng ký patch vào `scripts/prepare-app.js`:
```javascript
const { main: patchMyFeature } = require('./patches/patch-my-feature');
await patchMyFeature();
```

## Gỡ lỗi và kiểm tra ứng dụng (Debugging)

- **DevTools**: Nhấn phím `Ctrl` + `Shift` + `I` trong cửa sổ Zalo.
- **Menu khay hệ thống**: Nhấp chuột phải vào biểu tượng khay $\rightarrow$ chọn **"Toggle DevTools"**.
- **Log terminal**: Khởi chạy với biến môi trường `ELECTRON_ENABLE_LOGGING=1`.

## Chạy kiểm thử tự động

```bash
npm test
```
Kiểm thử toàn bộ các test suite JS và thuật toán mã hóa cuộc gọi ZRTP/SRTP.
