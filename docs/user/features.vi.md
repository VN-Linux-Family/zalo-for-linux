# Tổng quan tính năng ✨

[English](./features.md) | **Tiếng Việt**

Tài liệu này tổng hợp các tính năng nổi bật được bổ sung trong **Zalo for Linux** mà bản Zalo chính thức không có.

---

### 🌐 1. Chế độ Ngoại tuyến (Offline Mode)

Đọc danh bạ, danh sách nhóm và tin nhắn đã được lưu cục bộ trên máy khi mất kết nối mạng.

- **Cách hoạt động**: Mở Zalo online một lần và bật tùy chọn "Ghi nhớ đăng nhập". Khi mất mạng, Zalo tự động khởi động ở chế độ ngoại tuyến có biểu tượng chỉ báo.
- **Tự động kết nối lại**: Zalo tự động xác thực ở chế độ nền và tải lại đầy đủ khi mạng được khôi phục.
- **Lưu ý**: Các tệp đính kèm hoặc tin nhắn gửi trong lúc mất mạng sẽ chỉ tải được khi có mạng trở lại. Đăng xuất tài khoản sẽ xóa bộ nhớ đệm offline.

---

### 🙈 2. Khởi động Ẩn dưới Khay hệ thống (Tray)

Tự động thu nhỏ Zalo xuống khay hệ thống khi bật máy hoặc khi gọi từ dòng lệnh.

- **Cách kích hoạt**:
  - **Trong ứng dụng**: Vào **Cài đặt → Chung** và bật **"Khởi động Zalo khi bật máy"**.
  - **Dòng lệnh (CLI)**: Thêm cờ `--hidden` hoặc `--start-hidden`:
    ```bash
    /path/to/zalo --hidden
    ```
- **Cách mở cửa sổ**: Nhấp vào icon ở khay hệ thống, bấm vào thông báo đến, hoặc khởi chạy lại Zalo.
- **Cơ chế an toàn**: Nếu môi trường desktop không hỗ trợ khay hệ thống, cờ này tự động bị bỏ qua để đảm bảo cửa sổ luôn mở được.

---

### ⚙️ 3. Cài đặt Âm thanh Nâng cao

Tùy chỉnh nhạc chuông cuộc gọi đến và âm chờ cuộc gọi đi trực tiếp trong giao diện.

- **Cách truy cập**: Mở **Cài đặt → Nâng cao...**.
- **Các âm thanh tùy biến**:
  - **Nhạc chuông (Ringtone)**: File âm thanh khi có cuộc gọi đến.
  - **Âm chờ (Ringback)**: File âm thanh phát trong lúc chờ người nhận bắt máy.
- **Lưu ý**: Hỗ trợ duyệt file từ ổ cứng, nghe thử trước khi lưu và có nút khôi phục về mặc định. Cài đặt được lưu lại qua các phiên làm việc.

---

### 🧩 4. Trình quản lý Userscripts tích hợp

Chạy các đoạn mã JavaScript mở rộng chuẩn Tampermonkey trực tiếp bên trong giao diện Zalo.

- **Cách truy cập**: Mở **Cài đặt → Nâng cao... → Userscript Manager**.
- **Tính năng hỗ trợ**:
  - Tạo mới, chỉnh sửa, dán mã, nhập file (`.js`, `.user.js`), bật/tắt và xóa script.
  - Nhận diện metadata Tampermonkey: `@name`, `@description`, `@version`, `@match`, `@include`, `@exclude`.
  - Hỗ trợ các GM API: `GM_info`, `GM_addStyle`, `GM_getValue`, `GM_setValue`, `GM_deleteValue`, `GM_listValues` và `unsafeWindow`.
- **Cảnh báo an toàn**: Userscript có quyền truy cập toàn bộ giao diện và tin nhắn. Chỉ cài đặt script từ nguồn mà bạn tin cậy.

---

### 🌙 5. Tích hợp Giao diện ZaDark

Bổ sung Dark Mode, bảo vệ quyền riêng tư và tùy biến giao diện được phát triển bởi [ZaDark](https://github.com/quaric/zadark).

- **Dark Mode tối ưu**: Giao diện tối hoàn chỉnh, tương thích với phong cách desktop Linux hiện đại.
- **Bảo vệ riêng tư**: Chống nhìn trộm tin nhắn (làm mờ nội dung) và ẩn trạng thái "đang soạn tin" / "đã xem".
- **Cá nhân hóa**: Đổi hình nền hội thoại tùy ý, chỉnh font chữ, dịch nhanh tin nhắn và bộ 80+ emoji độc quyền.
- **Tích hợp nguyên bản**: Được tự động biên dịch và tích hợp trực tiếp vào AppImage trong chu trình build.

---

### 🗂️ 6. Giữ tin nhắn đã thu hồi

Giữ lại nội dung tin nhắn mà người khác thu hồi, để bạn xem lại sau. Tính năng này tắt sẵn.

- **Cách bật**: Mở **Cài đặt → Nâng cao...**, bật **Giữ tin nhắn đã thu hồi**. Áp dụng ngay, không cần mở lại Zalo.
- **Cách xem**: Khi có tin bị thu hồi, nút **Thu hồi** hiện phía trên ô soạn tin. Hộp danh sách có hai mục: **Cuộc trò chuyện này** và **Tất cả**. Ảnh và video hiện dạng ảnh nhỏ, bấm vào để xem ảnh lớn hoặc phát video. Nút xoá đi theo mục đang xem: **Xoá tin cuộc này** hoặc **Xoá hết**.
- **Giới hạn**:
  - Chỉ giữ được tin bị thu hồi lúc Zalo đang mở trên máy.
  - Giữ tin chữ, link, ảnh và video. Với ảnh và video, chỉ giữ đường link tới máy chủ Zalo; nếu Zalo xoá tệp trên máy chủ thì không xem lại được.
  - Lưu trên máy này, tối đa 500 tin. Tắt tính năng sẽ xoá toàn bộ tin đã giữ.
