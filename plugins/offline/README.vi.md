# Chế độ Ngoại tuyến

[English](./README.md) | **Tiếng Việt**

Chế độ Ngoại tuyến cho phép Zalo sử dụng lại phiên đăng nhập đã lưu cục bộ của tài khoản hiện tại và các cơ sở dữ liệu tin nhắn do chính Zalo quản lý. Plugin không tạo kho danh bạ hoặc tin nhắn thứ hai.

## Kiến trúc

Tính năng gồm Offline plugin và startup patch:

- `index.js` quản lý cache phiên đăng nhập được mã hóa, xác thực IPC sender và cung cấp mã nguồn renderer cùng các yêu cầu lưu trữ.
- `store.js` đọc và ghi bản ghi phiên đã mã hóa.
- `renderer.js` quản lý state machine khởi động/kết nối lại và status badge nhỏ. Phần này độc lập với tên module Webpack của Zalo để có thể kiểm thử mà không cần Electron.
- `patch-offline-startup.js` điều chỉnh mã khởi động đã extract của Zalo. Patch kết nối phản hồi đăng nhập đã cache với bootstrap gốc của Zalo và bỏ qua các công việc chỉ cần mạng khi Offline đang bật.

Patch chạy sau `patch-network-and-sync-fix.js`. Patch kiểm tra các anchor upstream và dừng thay vì áp dụng một phần nếu Zalo thay đổi cấu trúc.

Khi khởi động Offline, Zalo vẫn mở các database SQLite/IndexedDB riêng của từng tài khoản. Patch bỏ qua nhánh xác minh Over-Queue qua cloud trong bộ tải lịch sử hội thoại ban đầu, nhưng vẫn giữ nguyên thao tác đọc database local và dispatch kết quả. Patch cũng bảo vệ message renderer khi `focusId` chưa tồn tại trong chế độ Offline. Luồng Online giữ nguyên hành vi gốc của Zalo.

## Cache phiên đăng nhập

Phản hồi đăng nhập Online thành công chỉ được cache khi có cookie `zpw_sek` hợp lệ. Cache có các đặc điểm:

- Mã hóa AES-256-GCM.
- Khóa được dẫn xuất từ cookie `zpw_sek` hiện có, vốn là cookie secure và HttpOnly. Không lưu thêm session key dạng plaintext.
- Ghi file nguyên tử với quyền `0600` trong thư mục `userData` của Electron.
- Gắn với client ID và UID tài khoản. Thiếu hoặc thay đổi cookie, dữ liệu hỏng, client ID khác hoặc UID khác đều ngăn khôi phục phiên.

Đăng xuất, đổi tài khoản và phiên bị máy chủ từ chối sẽ vô hiệu hóa cache. Generation check ngăn một thao tác ghi đang chờ tạo lại cache sau khi cache đã bị vô hiệu hóa.

## Khởi động và kết nối lại

- Khi có cache, yêu cầu đăng nhập Online bị giới hạn trong tám giây. Chỉ lỗi mạng mới cho phép dùng phản hồi đã cache.
- Khi khởi động Offline, renderer báo trạng thái cho main process và thử xác thực lại mỗi mười giây, đồng thời khi browser phát sự kiện `online`.
- Mỗi thời điểm chỉ có một lần thử kết nối lại; mỗi lần thử có timeout mười lăm giây.
- Khi xác thực thành công và giao diện local đã sẵn sàng, trang reload một lần để Zalo áp dụng phản hồi máy chủ mới và đồng bộ theo luồng bình thường.
- Nếu phiên bị từ chối, cache bị xóa và Zalo quay về luồng đăng nhập bình thường.

Shared Worker yêu cầu main process cung cấp trạng thái Offline qua một IPC riêng đã được xác thực. Main process chỉ nhận báo cáo trạng thái từ main frame đáng tin cậy của trang login/index cục bộ; Shared Worker chỉ được đọc trạng thái, không được phép gửi trạng thái.

## Phát triển và kiểm thử

Sau khi extract ứng dụng, chạy network/sync patch và Offline startup patch, sau đó khởi động lại ứng dụng phát triển:

```sh
node scripts/patches/patch-network-and-sync-fix.js
node scripts/patches/patch-offline-startup.js
npm start
```

Mở Zalo Online một lần và bật **Ghi nhớ đăng nhập** để tạo cache. Bộ test Offline bao phủ session store, khởi động/kết nối lại và phân phối renderer qua IPC:

```sh
node --test tests/offline.test.js
```

Chạy toàn bộ test bằng `npm test`.

Kiểm thử thủ công với tài khoản thật:

1. Mở Zalo Online, bật Ghi nhớ đăng nhập. Mở vài hội thoại rồi thoát ứng dụng bình thường.
2. Ngắt mạng và khởi động lại. Kiểm tra tài khoản, danh bạ, nhóm và tin nhắn đã tải trước đó. Mở nhiều hội thoại đã cache và kiểm tra app lock.
3. Kết nối mạng lại. Xác nhận chỉ reload tự động một lần, nhận tin nhắn mới và giao diện được cập nhật.
4. Thử lại khi Wi-Fi vẫn kết nối nhưng không có internet, và sau khi suspend/resume.
5. Đăng xuất, thoát ứng dụng rồi khởi động Offline. Phiên cũ không được khôi phục. Lặp lại sau khi đổi tài khoản và khi phiên bị máy chủ thu hồi.

## Giới hạn

Chỉ dữ liệu Zalo đã tải về máy mới dùng được Offline. Đăng nhập lần đầu, tin nhắn chưa cache, tệp đính kèm không có sẵn cục bộ và tin nhắn gửi khi mất mạng đều cần kết nối mạng. Cookie hết hạn cũng cần xác thực Online; Offline Mode không gia hạn hoặc tạo lại cookie phiên.
