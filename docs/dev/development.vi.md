# Hướng dẫn phát triển

[English](./development.md) | **Tiếng Việt**

Tài liệu này hướng dẫn cách build Zalo for Linux từ mã nguồn, bao gồm bộ công cụ build, các script hỗ trợ và quy trình thêm patch hoặc native addon mới.

Để nắm tổng quan cách dự án hoạt động, xem [architecture.vi.md](./architecture.vi.md). Để tìm hiểu về việc viết lại các native addon, xem [nativelibs/README.vi.md](../../nativelibs/README.vi.md).

## Yêu cầu môi trường

- Linux x86_64 hoặc aarch64
- Node.js (phiên bản 22+) và npm
- `7z` (`p7zip-full`) để giải nén gói ứng dụng macOS
- Bộ công cụ biên dịch C++ và Rust (`cargo` / `rustup`) cho các native addon (xem [nativelibs/README.vi.md](../../nativelibs/README.vi.md))

- **Debian / Ubuntu**:

  ```bash
  sudo apt-get update && sudo apt-get install -y p7zip-full build-essential libssl-dev liblzma-dev clang libclang-dev llvm cargo
  ```

- **Arch Linux**:

  ```bash
  sudo pacman -S p7zip base-devel openssl xz clang llvm rust
  ```

- **Fedora / RHEL**:

  ```bash
  sudo dnf install p7zip p7zip-plugins @development-tools openssl-devel xz-devel clang llvm cargo rust
  ```

- **openSUSE**:

  ```bash
  sudo zypper install p7zip patterns-devel-base-devel_basis libopenssl-devel xz-devel clang llvm rust cargo
  ```

> _Hoặc cài đặt Rust qua [rustup](https://rustup.rs/): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`_

## Bắt đầu nhanh

```bash
# 1. Clone repository
git clone https://github.com/VN-Linux-Family/zalo-for-linux.git
cd zalo-for-linux

# 2. Khởi tạo các submodule (ZaDark, v.v.)
git submodule update --init --recursive

# 3. Cài đặt các gói phụ thuộc
npm ci --prefer-offline --no-audit

# 4. Setup + build (tải DMG, giải nén, vá code, đóng gói AppImage)
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
const fs = require("fs-extra");
const path = require("path");
const APP_DIR = path.join(__dirname, "..", "..", "app");

async function main() {
  const targetPath = path.join(APP_DIR, "main-dist", "main.js");
  if (!fs.existsSync(targetPath)) return;

  let content = fs.readFileSync(targetPath, "utf8");
  if (content.includes("OLD_PATTERN")) {
    content = content.replace(/OLD_PATTERN/g, "NEW_PATTERN");
    fs.writeFileSync(targetPath, content, "utf8");
  }
}

module.exports = { main };
```

1. Đăng ký patch vào `scripts/prepare-app.js`:

```javascript
const { main: patchMyFeature } = require("./patches/patch-my-feature");
await patchMyFeature();
```

## Cắt bớt dung lượng gói

`scripts/clean-unused.js` dọn binary không phải Linux, mã nguồn native
(`.rs`, `Cargo.*`, `.cargo`) và các file chỉ dùng để build trong addon
(`zlang/index.ts`, `zlang/scripts`, ...) trước khi đóng gói, sau đó cảnh báo
nếu còn `.node` không phải Linux sót lại.

`scripts/trim-appdir.js` chạy từ `scripts/build-stage2.sh` ngay trước khi
đóng gói lại AppDir, nên áp dụng cho cả bản Gốc và AppDir ZaDark được tái sử
dụng. Mặc định chỉ giữ locale `vi`, `en-US`, `en-GB`; locale thiếu sẽ tự
fallback về `en-US`. Các thư viện GPU fallback là tuỳ chọn vì máy ảo và
remote desktop có thể cần tới.

| Biến                    | Mô tả                                     | Ví dụ                     |
| ----------------------- | ----------------------------------------- | ------------------------- |
| `ZALO_KEEP_LOCALES`     | Giữ toàn bộ locale pack trong AppImage    | `ZALO_KEEP_LOCALES=1`     |
| `ZALO_TRIM_SWIFTSHADER` | Bỏ SwiftShader (Vulkan phần mềm)          | `ZALO_TRIM_SWIFTSHADER=1` |
| `ZALO_TRIM_VULKAN`      | Bỏ `libvulkan.so.1` (mất tăng tốc Vulkan) | `ZALO_TRIM_VULKAN=1`      |

## Gỡ lỗi và kiểm tra ứng dụng (Debugging)

- **Bật Chế độ nhà phát triển**: Mở **Cài đặt → Nâng cao... → Chế độ nhà phát triển** và bật công tắc. DevTools mặc định không được phép mở.
- **Mở DevTools**: Nhấn `Ctrl` + `Shift` + `I` trong cửa sổ Zalo, hoặc nhấp chuột phải vào biểu tượng khay hệ thống và chọn **Bật/tắt DevTools**.
- **Ghi đè bằng biến môi trường**: Đặt `ZALO_DEV_MODE=1` để bật cưỡng bức Chế độ nhà phát triển mà không thay đổi cấu hình đã lưu.
- **Log terminal**: Khởi chạy với biến môi trường `ELECTRON_ENABLE_LOGGING=1`.

## Chạy kiểm thử tự động

```bash
npm test
```

Kiểm thử toàn bộ các test suite JS và thuật toán mã hóa cuộc gọi ZRTP/SRTP.

## Phát triển plugin

Các plugin và module liên quan nằm trong `plugins/`. Ứng dụng có hai điểm tích hợp:

- `plugins/index.js` điều phối lifecycle của các plugin tính năng. Module cần khởi tạo trước khi tạo cửa sổ có thể dùng `initEarly()`; sau đó orchestrator gọi `register()` và truyền vào các dịch vụ Electron cùng dependency cần thiết. Lỗi đăng ký của một plugin được cô lập để không ngăn các plugin còn lại được nạp.
- `main.js` kết nối những thành phần cần tham gia trực tiếp vào quá trình khởi động ứng dụng, quản lý cửa sổ, dựng tray hoặc lifecycle của call engine. Không phải thư mục nào trong `plugins/` cũng được đăng ký thông qua `plugins/index.js`.

### Thêm hoặc chỉnh sửa plugin

1. Xác định vị trí của tính năng trong lifecycle khởi động. Dùng `plugins/index.js` để đăng ký plugin tính năng theo cơ chế điều phối chung; chỉ tích hợp trực tiếp trong `main.js` khi tính năng thực sự cần được kết nối tại đó.
2. Khi phù hợp, giữ state, UI, persistence và cleanup trong module của plugin. Nhận dịch vụ Electron và dependency qua context đăng ký thay vì phụ thuộc không cần thiết vào biến toàn cục của ứng dụng.
3. Giữ nguyên ranh giới sở hữu và hành vi hiện có. Không tiếp quản menu item, shortcut, IPC channel hoặc cài đặt thuộc plugin khác.
4. Cô lập lỗi, kiểm tra dữ liệu IPC đầu vào và dọn listener, timer, tham chiếu cửa sổ khi plugin hoặc cửa sổ được dispose.
5. Bổ sung hoặc cập nhật test riêng trong `tests/`, sau đó chạy `npm test` và `git diff --check`.

### Tích hợp đa ngôn ngữ cho plugin

Đối với phát triển plugin đa ngôn ngữ, xem [Hướng dẫn tích hợp đa ngôn ngữ cho plugin](./plugin-multilingual-integration.vi.md).
