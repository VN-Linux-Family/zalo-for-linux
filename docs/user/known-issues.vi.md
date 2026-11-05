# Lỗi thường gặp & Cách khắc phục 🛠️

[English](./known-issues.md) | **Tiếng Việt**

Bảng tra cứu nhanh các lỗi phổ biến khi cài đặt và sử dụng Zalo for Linux:

| Hiện tượng / Triệu chứng lỗi | Nguyên nhân cốt lõi | Lệnh / Thao tác khắc phục ngay |
| :--- | :--- | :--- |
| **Văng ngay khi mở (Ubuntu 23.10+)** | AppArmor chặn unprivileged user namespace | Thêm cờ `--no-sandbox` vào lệnh chạy hoặc file `.desktop`. |
| **Không gọi được (Mất tiếng/Mic/Cam)** | Thiếu thư viện âm thanh Opus và PulseAudio | Cài đặt các gói phụ thuộc:<br>• **Debian / Ubuntu**: `sudo apt install python3 libopus0 pulseaudio-utils`<br>• **Arch Linux**: `sudo pacman -S python opus pulseaudio`<br>• **Fedora**: `sudo dnf install python3 opus pulseaudio-utils` |
| **Không dán được ảnh (`Ctrl+V`)** | Thiếu công cụ clipboard cho Wayland / X11 | Cài đặt `wl-clipboard` (nếu dùng Wayland) hoặc `xclip` (nếu dùng X11). |
| **Giao diện không tự đổi Dark/Light** | Ứng dụng mặc định giữ theme tĩnh | Đã hỗ trợ tự động nhận diện theo thời gian thực qua GNOME D-Bus và XDG Desktop Portal trong các bản build mới. |
| **Không có icon trên menu ứng dụng** | Chạy AppImage trực tiếp chưa tích hợp | Dùng [Gear Lever](https://flathub.org/en/apps/it.mijorus.gearlever) để đưa vào Menu, hoặc tạo file `~/.local/share/applications/zalo.desktop`. |

---

## ⚠️ Các giới hạn hiện tại

- **Cuộc gọi trên ARM64 (`aarch64`)**: Engine gọi điện chạy hoàn toàn bằng Node.js + Python (`zcall-native/`, không dùng Wine của x86), nhưng chưa được kiểm thử toàn diện trên thiết bị phần cứng ARM64 thực tế ([Issue #70](https://github.com/VN-Linux-Family/zalo-for-linux/issues/70)).
- **Giới hạn Sandbox trên Ubuntu 23.10+**: Do cơ chế AppArmor mới, cần chạy với `--no-sandbox` hoặc cấu hình profile AppArmor riêng cho `chrome-sandbox` ([Issue #72](https://github.com/VN-Linux-Family/zalo-for-linux/issues/72), [Issue #143](https://github.com/VN-Linux-Family/zalo-for-linux/issues/143)).

---

## ✅ Lịch sử các lỗi đã sửa & Đóng góp từ cộng đồng

<details>
<summary><b>Bấm để xem chi tiết lịch sử sửa lỗi và ghi nhận người đóng góp</b></summary>

- **Cuộc gọi (Thoại, Video, Chia sẻ màn hình, Gọi nhóm):**
  - **Lỗi:** Không gọi hay nhận cuộc gọi được trên Linux.
  - **Khắc phục:** Sử dụng engine gọi native cho Linux ([`zcall-native/`](../../zcall-native/README.md), từ [zalo-linux-native](https://github.com/nct88/zalo-linux-native)). Hoàn toàn không dùng Wine, không cần `ZaloCall.exe`, không cần thư viện 32-bit.
  - **Đóng góp:** [@nct88](https://github.com/nct88), [@collyn](https://github.com/collyn). Xem [PR #62](https://github.com/VN-Linux-Family/zalo-for-linux/pull/62), [PR #128](https://github.com/VN-Linux-Family/zalo-for-linux/pull/128), [PR #129](https://github.com/VN-Linux-Family/zalo-for-linux/pull/129), [PR #135](https://github.com/VN-Linux-Family/zalo-for-linux/pull/135), [PR #136](https://github.com/VN-Linux-Family/zalo-for-linux/pull/136), [PR #137](https://github.com/VN-Linux-Family/zalo-for-linux/pull/137).

- **Tự động đổi theme Sáng / Tối theo hệ thống:**
  - **Lỗi:** Zalo và ZaDark bỏ qua `prefers-color-scheme` của hệ điều hành.
  - **Khắc phục:** Nhận diện thời gian thực qua GNOME D-Bus (`org.gnome.desktop.interface color-scheme`), XDG Desktop Portal (`org.freedesktop.appearance`) hoặc GTK theme. Tự động đồng bộ `nativeTheme.themeSource` và class `.dark` trong DOM.
  - **Đóng góp:** [@nct88](https://github.com/nct88). Xem [PR #87](https://github.com/VN-Linux-Family/zalo-for-linux/pull/87), [Issue #21](https://github.com/VN-Linux-Family/zalo-for-linux/issues/21), [Issue #22](https://github.com/VN-Linux-Family/zalo-for-linux/issues/22).

- **Đồng bộ tin nhắn mã hóa đầu cuối (E2EE) & Bảng thông tin hội thoại:**
  - **Lỗi:** Thiếu native module `db-cross-v4` khiến tin nhắn E2EE, media trong panel thông tin và thả cảm xúc emoji bị lỗi trên Linux.
  - **Khắc phục:** Viết lại addon native `db-cross-v4` bằng C++.
  - **Đóng góp:** [@realdtn2](https://github.com/realdtn2), [@DMKha2k7](https://github.com/DMKha2k7). Xem [PR #24](https://github.com/VN-Linux-Family/zalo-for-linux/pull/24), [Issue #15](https://github.com/VN-Linux-Family/zalo-for-linux/issues/15).

- **Dán và gửi ảnh từ clipboard:**
  - **Lỗi:** Nhấn `Ctrl+V` không dán được ảnh, gửi ảnh bị biến thành file đính kèm thô thay vì ảnh xem trước.
  - **Khắc phục:** Tích hợp bộ dán ảnh qua `wl-clipboard` (Wayland) và `xclip` (X11).
  - **Đóng góp:** [@realdtn2](https://github.com/realdtn2), [@DMKha2k7](https://github.com/DMKha2k7) ([PR #25](https://github.com/VN-Linux-Family/zalo-for-linux/pull/25)), [@brucenguyen1102](https://github.com/brucenguyen1102) ([PR #34](https://github.com/VN-Linux-Family/zalo-for-linux/pull/34)).

- **Khởi động Zalo cùng hệ thống trên Linux:**
  - **Lỗi:** Tùy chọn "Khởi động cùng hệ thống" trong cài đặt Zalo không hoạt động trên Linux.
  - **Khắc phục:** Vá IPC handler để tự động tạo hoặc xóa file cấu hình tiêu chuẩn `~/.config/autostart/zalo.desktop`.
  - **Đóng góp:** [@akimiya7742](https://github.com/akimiya7742), [@DMKha241](https://github.com/DMKha241). Xem [PR #51](https://github.com/VN-Linux-Family/zalo-for-linux/pull/51), [PR #59](https://github.com/VN-Linux-Family/zalo-for-linux/pull/59), [PR #61](https://github.com/VN-Linux-Family/zalo-for-linux/pull/61).

- **Thư mục tải về chuẩn XDG:**
  - **Lỗi:** Zalo tự động tạo thư mục tiếng Anh `~/Zalo Received Files` ở `$HOME` thay vì tuân theo chuẩn Linux.
  - **Khắc phục:** Điều hướng đường dẫn tải file về đúng thư mục tiêu chuẩn `$XDG_DOWNLOAD_DIR`.
  - **Đóng góp:** [@akimiya7742](https://github.com/akimiya7742). Xem [PR #52](https://github.com/VN-Linux-Family/zalo-for-linux/pull/52), [Issue #20](https://github.com/VN-Linux-Family/zalo-for-linux/issues/20), [Issue #48](https://github.com/VN-Linux-Family/zalo-for-linux/issues/48).

- **Huy hiệu đếm số tin nhắn chưa đọc (Badge Counter):**
  - **Lỗi:** Icon trên thanh dock hoặc khay hệ thống không hiện số tin nhắn chưa đọc.
  - **Khắc phục:** Tích hợp huy hiệu số đếm cho các môi trường desktop Unity/KDE/GNOME và vẽ trực tiếp số lên icon khay hệ thống.
  - **Đóng góp:** [@akimiya7742](https://github.com/akimiya7742) ([PR #43](https://github.com/VN-Linux-Family/zalo-for-linux/pull/43)), [@namtao](https://github.com/namtao) ([PR #151](https://github.com/VN-Linux-Family/zalo-for-linux/pull/151)).

- **Công cụ chụp màn hình trong ứng dụng:**
  - **Lỗi:** Phím chụp màn hình tích hợp của Zalo không hoạt động trên Linux.
  - **Khắc phục:** Tích hợp với các công cụ chụp ảnh màn hình native của Linux (`spectacle`, `flameshot`, `gnome-screenshot`, `scrot`).
  - **Đóng góp:** [@hthienloc](https://github.com/hthienloc). Xem [Issue #19](https://github.com/VN-Linux-Family/zalo-for-linux/issues/19).

- **Treo ở màn hình đăng nhập:**
  - **Lỗi:** Ứng dụng bị đơ ngay khi mở màn hình đăng nhập.
  - **Khắc phục:** Thay thế các file nhị phân SQLite3 của macOS bằng bản build native cho Linux ([Issue #13](https://github.com/VN-Linux-Family/zalo-for-linux/issues/13)).

</details>
