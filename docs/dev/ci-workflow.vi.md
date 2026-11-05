# Tự động hóa CI/CD và Quy trình Release

[English](./ci-workflow.md) | **Tiếng Việt**

Tài liệu này mô tả chi tiết pipeline CI/CD, cơ chế tự động release và kiến trúc thăm dò phiên bản Zalo mới được cấu hình trong `.github/workflows/build.yml`.

---

## 1. Kiến trúc Pipeline và Các Trigger

Quy trình tự động build và release được định nghĩa trong `.github/workflows/build.yml`, hỗ trợ 4 trigger chính:

| Trigger | Mô tả | Mục đích |
| :--- | :--- | :--- |
| `schedule` (`cron: '0 0 * * *'`) | Chạy tự động hàng ngày lúc 00:00 UTC | Thăm dò máy chủ VNG để bắt kịp các bản Zalo desktop mới. |
| `push` (`branches: [main]`) | Kích hoạt khi có commit mới vào `main` | Build và kiểm thử code mới khi merge PR. |
| `pull_request` (`branches: [main]`) | Kích hoạt khi mở hoặc cập nhật PR | Chạy ma trận build để kiểm tra lỗi mà không tạo release. |
| `workflow_dispatch` | Kích hoạt thủ công từ giao diện GitHub Web | Cho phép maintainer build với các tham số phiên bản tùy chọn. |

---

## 2. Cơ chế thăm dò phiên bản Zalo từ máy chủ VNG

Vì Zalo là phần mềm độc quyền đóng gói của VNG, các bản phát hành mới không phát sinh commit hay webhook nào trên GitHub.

Để người dùng Linux luôn có bản mới nhất, CI tự động kiểm tra hàng ngày:

1. `scripts/check-versions.js` gửi một HTTP GET request tới `https://zalo.me/download/zalo-pc?utm=90000`.
2. Máy chủ của VNG phản hồi bằng mã chuyển hướng HTTP 302/301 chứa tên file DMG macOS mới nhất:
   ```text
   Location: .../ZaloSetup-universal-26.11.10.dmg
   ```
3. Script bóc tách phiên bản (ví dụ `26.11.10`) và gọi GitHub API kiểm tra xem bản này đã được build trước đó chưa.
4. **Quy tắc thực thi**:
   - Nếu phiên bản này đã có release: workflow đặt `build=false` và kết thúc ngay lập tức để tiết kiệm phút chạy CI.
   - Nếu phát hiện phiên bản mới: workflow đặt `build=true` và bắt đầu chu trình biên dịch.

---

## 3. Ma trận biên dịch (`jobs.build`)

Job `build` biên dịch và đóng gói AppImage trên 2 kiến trúc phần cứng:

- **`x86_64`**: Chạy trên `ubuntu-latest` bên trong container `ubuntu:22.04`.
- **`aarch64`**: Chạy trên `ubuntu-24.04-arm` bên trong container `ubuntu:22.04`.

Các bước thực hiện:
1. Biên dịch các native addon trong `nativelibs/` cho kiến trúc tương ứng.
2. Tải và giải nén file DMG chính thức.
3. Áp dụng các bản vá JavaScript từ `scripts/patches/`.
4. Biên dịch tài nguyên giao diện ZaDark từ submodule.
5. Sử dụng `electron-builder` và `quick-sharun` để đóng gói thành AppImage.
6. Tải các file AppImage hoàn chỉnh lên làm artifact của workflow.

---

## 4. Cơ chế Release và Tự động Cập nhật Nix (`jobs.release`)

Khi chạy trên nhánh `main` (không phải pull request) và có `build=true`, job `release` sẽ:

1. **Quản lý Asset**: Xóa các asset cũ trùng phiên bản nếu cần dọn dẹp.
2. **Xuất bản GitHub Release**: Tạo GitHub Release kèm link tải AppImage cho x86_64 và aarch64 (cả 2 biến thể Original và ZaDark).
3. **Cập nhật mã băm Nix Flake**:
   - Chạy `scripts/update-nix-sources.sh` để tính lại mã băm SHA256 của các AppImage vừa tạo và cập nhật vào `nix/sources.json`.
   - Bot `github-actions[bot]` tự động commit thay đổi vào nhánh `main`:
     ```text
     chore(nix): update sources for <version> (<commit-hash>)
     ```
   - Nhờ đó, người dùng cài Zalo qua NixOS Flake luôn có mã băm chính xác mà maintainer không cần sửa thủ công.

---

## 5. Lưu ý quan trọng khi Fork Repository

Khi bạn fork `zalo-for-linux`, toàn bộ workflow GitHub Actions cũng được nhân bản sang tài khoản cá nhân:

### Vấn đề lệch nhánh trên Fork:
- Nếu GitHub Actions được bật trên fork cá nhân, cron job hàng ngày sẽ chạy trên tài khoản của bạn.
- Bot trên fork sẽ tự commit file `nix/sources.json` vào nhánh `main` của fork.
- Điều này khiến nhánh `main` trên fork bị **lệch lịch sử (diverged)** so với upstream, gây xung đột khi bạn pull code mới về.

### Khuyến nghị cho Contributor:
1. **Tắt Workflow trên Fork**:
   - Vào repo fork trên GitHub: `Settings` $\rightarrow$ `Actions` $\rightarrow$ `General` $\rightarrow$ chọn **Disable actions**.
2. **Điều kiện chặn chạy release trên Fork**:
   - Khi sửa CI, nên thêm điều kiện để các bước tạo release và commit chỉ chạy trên repo gốc:
     ```yaml
     if: github.repository == 'VN-Linux-Family/zalo-for-linux' && github.ref == 'refs/heads/main'
     ```
