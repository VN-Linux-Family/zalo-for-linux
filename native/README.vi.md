# Các Module và Engine Native

Thư mục này tập trung toàn bộ các bản triển khai native cho Linux thay thế các thành phần độc quyền của Zalo trên macOS và Windows:

```text
native/
├── builder.js                # Công cụ build hợp nhất (C++ node-gyp & Rust NAPI)
├── addons/                   # Các native addon N-API nạp động qua require()
│   ├── db-cross-v4/          # Giải mã bản sao lưu & đồng bộ tin nhắn E2EE (C++)
│   ├── file-utilities/       # Quét hệ thống tệp bất đồng bộ & khớp mẫu (Rust)
│   ├── file-utils/           # Tiện ích truy cập & quản lý tệp (Rust)
│   ├── mp4thumb/             # Trích xuất ảnh thu nhỏ từ video MP4 (Rust)
│   ├── os-sys-info/          # Đọc thông tin hệ điều hành & PID (Rust)
│   ├── v8-profiles/          # Phân tích hiệu năng CPU V8 (C++)
│   ├── zfile/                # Thông tin ổ đĩa, dung lượng & sao chép tệp (C++)
│   ├── zimage/               # Nén & chuyển đổi định dạng ảnh (Rust)
│   ├── zjxl/                 # Giải mã & mã hóa ảnh JPEG XL (Rust)
│   ├── zlang/                # Nhận diện ngôn ngữ văn bản (Rust)
│   └── zwalker/              # Dọn rác bộ nhớ đệm thư mục media (Rust)
└── engines/                  # Các hệ thống con độc lập
    ├── zcall/                # Engine gọi điện thoại (báo hiệu, ZRTP/SRTP & Rust audio)
    └── zocr/                 # Daemon nhận diện chữ OCR & ONNX runtime (Rust)
```

## Biên dịch Native Addons

Tất cả các native addon trong `addons/` đều có thể biên dịch bằng công cụ hợp nhất:

```bash
# Biên dịch addon C++ hoặc Rust bất kỳ:
node native/builder.js native/addons/<tên-addon>

# Ví dụ:
node native/builder.js native/addons/db-cross-v4
node native/builder.js native/addons/zwalker
```

Công cụ tự động nhận diện toolchain (`binding.gyp` cho C++ node-gyp, `Cargo.toml` cho Rust) và xuất file nhị phân `.node`.

## Rust Workspace

Toàn bộ các crate Rust trong `native/addons/` và `native/engines/` đều nằm trong Cargo workspace gốc:

```bash
cargo test --workspace --locked
```
