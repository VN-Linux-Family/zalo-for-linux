# Đóng góp cho Zalo for Linux

[English](./CONTRIBUTING.md) | **Tiếng Việt**

Cảm ơn bạn đã dành thời gian đóng góp. Zalo for Linux là bản port có nguồn gốc từ bản macOS chính thức rồi đóng gói lại kèm các bản vá (patch) cho Linux.

## Trước khi bắt đầu

Đọc các tài liệu mô tả cách dự án hoạt động:

- [docs/dev/architecture.vi.md](./docs/dev/architecture.vi.md) — build pipeline và cách các patch được áp dụng
- [docs/dev/development.vi.md](./docs/dev/development.vi.md) — build từ mã nguồn, các script, biến môi trường, cách thêm patch
- [docs/dev/patch-guide.vi.md](./docs/dev/patch-guide.vi.md) — hướng dẫn viết, kiểm thử và duy trì các bản vá JavaScript
- [docs/dev/ci-workflow.vi.md](./docs/dev/ci-workflow.vi.md) — quy trình CI/CD, cơ chế thăm dò Zalo mới và bot phát hành
- [docs/user/features.vi.md](./docs/user/features.vi.md) — những tính năng bổ sung so với client chính thức
- [docs/user/known-issues.vi.md](./docs/user/known-issues.vi.md) — các giới hạn hiện tại và bảng tra cứu cách khắc phục

Sau đó tìm trong [issue](https://github.com/VN-Linux-Family/zalo-for-linux/issues) và pull request hiện có trước khi bắt tay: có thể đã có người làm cùng việc.

Bản thân Zalo là phần mềm độc quyền. Không bao giờ commit binary của Zalo, nội dung `app.asar` đã giải nén, hay file `.node` dựng sẵn — các native addon luôn được build lại từ source và không có mã độc quyền nào trong repo ([nativelibs/README.vi.md](./nativelibs/README.vi.md)).

## Các cách đóng góp

- **Báo lỗi** — xem [Báo cáo lỗi](#báo-cáo-lỗi).
- **Đề xuất tính năng** — xem [Đề xuất tính năng](#đề-xuất-tính-năng).
- **Code** — patch JavaScript trong `scripts/patches/`, addon Rust trong `nativelibs/` và `zocr/`, Node.js và Python trong `zcall-native/`.
- **Tài liệu** — hỗ trợ song ngữ (tiếng Anh và tiếng Việt) cho toàn bộ tài liệu (xem [Tài liệu](#tài-liệu)).
- **Kiểm thử** — nhất là trên aarch64
- **Đóng gói** — AppImage là bản build chính thức; repo cũng có Nix flake (xem mục NixOS trong [README.vi.md](./docs/user/README.vi.md)).

## Thiết lập môi trường phát triển

Yêu cầu:

- Linux x86_64 hoặc aarch64
- Node.js 22 và npm (CI dùng `node-version: '22'`)
- `7z` để giải nén ứng dụng macOS
- C++ build tools cho các native addon
- Rust toolchain (stable)
- Để gọi điện: `python3`, libopus và các công cụ PulseAudio

```bash
git clone https://github.com/VN-Linux-Family/zalo-for-linux.git
cd zalo-for-linux
git submodule update --init --recursive
npm run main
```

Có thể chạy riêng từng bước:

```bash
npm run main:setup      # tải + giải nén vào app/ và temp/
npm run main:build      # đóng gói AppImage từ app/
npm run start           # chạy ứng dụng đã giải nén ở chế độ dev
```

Danh sách đầy đủ các script, biến môi trường (`ZALO_VERSION`, `ZADARK_VERSION`, `FORCE_DOWNLOAD`) và cách thêm patch: xem [DEVELOPMENT.vi.md](./docs/dev/development.vi.md).

## Phong cách code

Patch nằm mỗi file một cái trong `scripts/patches/patch-<name>.js` và export `{ main }`. Luôn kiểm tra pattern mong đợi có tồn tại trước khi thay — vì Zalo cập nhật là code sẽ bị sửa -> không tìm được pattern để patch — rồi đăng ký patch trong `scripts/prepare-app.js` (xem [patch-guide.vi.md](./docs/dev/patch-guide.vi.md)).

## Kiểm thử

```bash
npm ci --prefer-offline --no-audit   # chạy một lần
npm test                             # node --test 'tests/**/*.test.js'
```

Thêm test trong `tests/<area>-<topic>.test.js`.

Kiểm thử các crate Rust:

```bash
cargo test --manifest-path zocr/Cargo.toml --locked
for c in nativelibs/*/Cargo.toml; do cargo test --manifest-path "$c" --locked; done
```

## Tài liệu

Tài liệu được tổ chức tập trung với hỗ trợ song ngữ đầy đủ (Tiếng Anh & Tiếng Việt):

- **Tài liệu kỹ thuật / Lập trình viên** (`docs/dev/`): Kiến trúc, phát triển, viết patch, CI workflow (`<tên>.md` và `<tên>.vi.md`).
- **Tài liệu người dùng** (`docs/user/` và `README` ở root): Hướng dẫn cài đặt, lỗi đã biết, tính năng (`<tên>.md` và `<tên>.vi.md`).
- **Tài liệu thành phần**: Các native addon trong `nativelibs/` có tài liệu riêng (`nativelibs/README.md` và `nativelibs/README.vi.md`).

## Pull Request và Commit

Pull request gửi vào nhánh **`main`**.

- Tên nhánh: `feat/<slug>`, `fix/<slug>`, `docs/<slug>`, `chore/<slug>` — không mở PR từ nhánh `main` của fork cá nhân vào `main` của repo gốc.
- Một thay đổi logic trên mỗi pull request để tiện review.
- Luôn rebase trên `main` trước khi mở PR.
- `app.setName` trong `main.js` và StartupWMClass trong `package.json` **bắt buộc là `Zalo`**.
- Nếu phát triển tính năng mới, hãy kiểm thử trước và chụp ảnh màn hình (nhớ làm mờ thông tin cá nhân).
- Tiêu đề commit: tuân thủ Conventional Commits (ví dụ: `feat(call): ...`, `fix(ui): ...`).
- Mô tả PR: nêu rõ lý do thay đổi, `Closes #N` nếu sửa issue, nền tảng đã thử (x86_64 hay aarch64, X11 hay Wayland).

Checklist trước khi mở PR:

- [ ] `npm test` pass, và các test Rust ở trên pass nếu bạn sửa crate.
- [ ] Không có file sinh ra khi build (`app/`, `temp/`, `dist/`, `node_modules/`).
- [ ] Không có binary hay file `.node` dựng sẵn.
- [ ] Đã kiểm thử trên máy bạn và chụp ảnh màn hình (nếu sửa UI).
- [ ] Tài liệu đã được cập nhật tương ứng.

## Báo cáo lỗi

Kèm theo:
- Phiên bản và biến thể (Original hoặc ZaDark), cùng cách bạn cài (AppImage, am, Gear Lever, NixOS, AUR);
- Distro và phiên bản kernel;
- Desktop environment và display server (X11 hay Wayland);
- Các bước tái hiện, kết quả mong đợi và thực tế.

## Đề xuất tính năng

Kiểm tra [features.vi.md](./docs/user/features.vi.md), [known-issues.vi.md](./docs/user/known-issues.vi.md) và các issue hiện có trước. Mô tả use case và lý do tính năng thuộc phạm vi của bản port này chứ không phải bản thân Zalo.

## Sử dụng AI

Dự án hoan nghênh việc sử dụng các công cụ AI một cách có trách nhiệm (như Codex, Claude Code, v.v.) để hỗ trợ viết code, patch hoặc tài liệu. Tuy nhiên, vui lòng tuân thủ:

- **Rà soát lại mọi thứ:** Bạn chịu trách nhiệm hoàn toàn cho tất cả code và nội dung do mình gửi lên. Đảm bảo đã kiểm tra kỹ, kiểm thử và hiểu rõ bất kỳ kết quả nào do AI tạo ra.
- **Khai báo nếu sử dụng nhiều:** Nếu pull request hoặc một phần lớn của patch/tài liệu được tạo ra chủ yếu bởi AI, vui lòng đề cập ngắn gọn trong phần mô tả của pull request.
- **Không gửi code chưa qua kiểm chứng:** Không sao chép mù quáng code do AI tạo ra, đặc biệt là đối với các addon native (`nativelibs/`, `zocr/`) để tránh lỗi an toàn bộ nhớ.

## Code từ các repo khác

Nếu bạn tích hợp mã nguồn từ một repo khác không sử dụng giấy phép BSD 3-Clause, vui lòng đính kèm thông tin giấy phép gốc và ghi rõ nguồn gốc xuất xứ.

## Bảo mật

Không mở issue công khai cho lỗ hổng bảo mật. Dùng tab **Security** của repo và chọn "Report a vulnerability" (GitHub private advisory).

## Giấy phép

Bằng việc đóng góp, bạn đồng ý rằng phần đóng góp của bạn được cấp phép theo cùng giấy phép với dự án — xem [LICENSE](./LICENSE). Repo này không có CLA. Zalo là thương hiệu của VNG Corporation; dự án này không liên kết với VNG Corporation và cũng không được VNG Corporation bảo trợ.

## Quy tắc ứng xử

Dự án tuân theo [Contributor Covenant, phiên bản 2.0](https://www.contributor-covenant.org/version/2/0/code_of_conduct/).

Cảm ơn bạn đã đóng góp!
