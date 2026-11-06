# Kiến trúc hệ thống

[English](./architecture.md) | **Tiếng Việt**

Tài liệu này giải thích cách Zalo for Linux hoạt động bên trong.

## Tổng quan

Dự án này không viết lại Zalo từ đầu. Thay vào đó, dự án:

1. Tải file `.dmg` chính thức của Zalo cho macOS.
2. Dùng `7z` để giải nén file `app.asar`, nơi chứa phần logic chính của ứng dụng viết bằng JavaScript.
3. Loại bỏ các file native của macOS không tương thích.
4. Bọc ứng dụng đã giải nén trong một Electron shell tối giản, tương thích với Linux.
5. Dùng `electron-builder` để đóng gói tất cả thành một file `AppImage` duy nhất, chạy được ở nhiều nơi.

## Cấu trúc thư mục

```text
zalo-for-linux/
├── .github/          # GitHub workflows, issue templates và hướng dẫn
├── CONTRIBUTING.md   # Hướng dẫn đóng góp
├── CONTRIBUTING.vi.md# Bản tiếng Việt của hướng dẫn đóng góp
├── docs/             # Thư mục tài liệu tập trung (SSOT)
│   ├── dev/          # Tài liệu kỹ thuật & phát triển (Kiến trúc, Viết patch, CI)
│   └── user/         # Hướng dẫn người dùng (Tính năng, Bảng tra cứu lỗi, README tiếng Việt)
├── LICENSE           # Giấy phép phần mềm (BSD 3-Clause)
├── main.js           # Điểm khởi chạy của Electron
├── README.md         # Trang chủ dự án
├── CONTRIBUTING.md   # Hướng dẫn đóng góp (Tiếng Anh)
├── CONTRIBUTING.vi.md# Hướng dẫn đóng góp (Tiếng Việt)
├── nativelibs        # Các native addon của Zalo được viết lại cho Linux
│   ├── builder.js    # Công cụ hỗ trợ build qua dòng lệnh
│   ├── builder-rust.js # Công cụ hỗ trợ build cho addon viết bằng Rust
│   ├── db-cross-v4   # Addon giải mã bản sao lưu và đồng bộ tin nhắn E2EE
│   ├── file-utilities# Addon quét hệ thống file bất đồng bộ
│   ├── file-utils    # Tiện ích truy cập và quản lý file
│   ├── mp4thumb      # Trích xuất ảnh thu nhỏ từ video MP4
│   ├── os-sys-info   # Bộ đọc thông tin hệ điều hành và ID tiến trình
│   ├── v8-profiles   # Phân tích hiệu năng CPU native cho Zalo
│   ├── README.md     # Tài liệu tổng quan về nativelibs
│   ├── zfile         # Đọc thông tin ổ đĩa (dung lượng trống, mount point)
│   ├── zimage        # Xử lý ảnh và chuyển đổi định dạng
│   ├── zjxl          # Giải mã và mã hóa ảnh JPEG XL (JXL)
│   ├── zcall-audio   # Engine âm thanh PulseAudio/PipeWire thời gian thực cho cuộc gọi
│   ├── zlang         # Nhận diện ngôn ngữ tự nhiên (whatlang)
│   └── zwalker       # Bộ dọn rác bộ nhớ đệm media
├── package.json      # Metadata dự án + cấu hình electron-builder
├── plugins           # Các plugin/extension của Zalo for Linux
│   ├── launcher-badge# Hiển thị số tin nhắn chưa đọc trên dock
│   ├── start-hidden  # Khởi động ẩn trong khay hệ thống
│   ├── tray-badge    # Vẽ số tin nhắn chưa đọc lên icon khay hệ thống
│   ├── userscripts   # Trình quản lý userscript Tampermonkey
│   ├── zadark        # Tiện ích giao diện tối và quyền riêng tư
│   └── zcall         # Cửa sổ gọi điện và thông báo cuộc gọi đến
├── scripts
│   ├── build.js      # Script đóng gói AppImage
│   ├── check-versions.js # Kiểm tra phiên bản Zalo mới từ VNG
│   ├── download.js   # Tải file DMG macOS và EXE Windows
│   ├── main.js       # Script điều phối (setup + build)
│   ├── patches/      # Các script vá JavaScript riêng lẻ
│   ├── prepare-app.js# Giải nén app.asar và áp dụng các patch
│   └── update-nix-sources.sh # Cập nhật mã băm vào nix/sources.json
└── zcall-native      # Engine gọi điện native (không Wine): tín hiệu, ZRTP/SRTP, Opus
```

## Build Pipeline

`npm run main` chạy toàn bộ pipeline (chi tiết về tự động hóa CI/CD, xem [ci-workflow.vi.md](./ci-workflow.vi.md)):

1. **check-versions**: So sánh phiên bản hiện có với bản Zalo mới nhất trên trang chủ Zalo.
2. **download**: Tải `ZaloSetup-universal-<version>.dmg` về thư mục `temp/`.
3. **prepare-zadark**: Build tài nguyên ZaDark từ submodule.
4. **prepare-app**: Giải nén `app.asar` vào `app/` và áp dụng các bản vá thông qua `scripts/patches/`.
5. **build**: Đóng gói tất cả thành một AppImage trong `dist/`.

## Các bản vá cho ứng dụng đã giải nén

Sau khi giải nén `app.asar` của macOS, cần chỉnh sửa mã nguồn JavaScript để tương thích với Linux. Các bản vá này được áp dụng tuần tự trong `scripts/prepare-app.js`. Để xem hướng dẫn chi tiết về cách viết và kiểm thử patch, đọc [patch-guide.vi.md](./patch-guide.vi.md).

Để tìm hiểu thêm về các native addon, xem [nativelibs/README.vi.md](../../nativelibs/README.vi.md).

## Vì sao cần Electron Shell?

Ứng dụng Zalo sau khi giải nén là một ứng dụng Electron được thiết kế riêng cho macOS. Dự án:

1. Cung cấp một file `main.js` tối giản để:
   - Khởi tạo các plugin bổ sung.
   - Nạp `app/main-dist/main.js` từ ứng dụng đã giải nén.
2. Dùng file `package.json` riêng với cấu hình `electron-builder` chuẩn cho Linux.
3. Đổi tên file `package.json` đã giải nén thành `package.json.bak` để tránh xung đột dependency.

## Bên trong thư mục `app/` có gì?

Mọi thứ trong `app/` đều do build pipeline **tự động tạo lại** và bị gitignore:

- `main-dist/main.js`: Bundle cho main process của Zalo.
- `main-dist/preload-*.js`: Các preload script tạo context bridge.
- `main-dist/compact-app.js`: Utility process chạy tác vụ ngầm.
- `pc-dist/`: Các bundle giao diện frontend (React).
- `native/nativelibs/`: Các native addon (được thay thế bằng các bản viết lại cho Linux).

Mã nguồn đã giải nén thuộc sở hữu của VNG/Zalo. Dự án không chỉnh sửa trực tiếp mã này trong git mà áp dụng các bản vá tự động mỗi lần build qua `prepare-app.js`.
