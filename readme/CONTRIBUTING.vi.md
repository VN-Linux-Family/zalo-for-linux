# Đóng góp cho Zalo for Linux

[English](../CONTRIBUTING.md) | **Tiếng Việt**

Cảm ơn bạn đã dành thời gian đóng góp. Zalo for Linux là bản port có nguồn gốc từ bản macOS chính thức rồi đóng gói lại kèm các patch cho Linux.

## Trước khi bắt đầu

Đọc các tài liệu mô tả cách dự án hoạt động:

- [ARCHITECTURE.vi.md](./ARCHITECTURE.vi.md) — build pipeline và cách các patch được áp dụng
- [DEVELOPMENT.vi.md](./DEVELOPMENT.vi.md) — build từ mã nguồn, các script, biến môi trường, cách thêm patch
- [FEATURES.vi.md](./FEATURES.vi.md) — những gì bản port này thêm so với client chính thức
- [KNOWN_ISSUES.vi.md](./KNOWN_ISSUES.vi.md) — các giới hạn đã biết và lịch sử lỗi đã sửa

Sau đó tìm trong [issue](https://github.com/VN-Linux-Family/zalo-for-linux/issues) và pull request hiện có trước khi bắt tay: có thể đã có người làm cùng việc.

Bản thân Zalo là phần mềm độc quyền. Không bao giờ commit binary của Zalo, nội dung `app.asar` đã giải nén, hay file `.node` dựng sẵn — các native addon luôn được build lại từ source và không có mã độc quyền nào trong repo ([nativelibs.vi.md](./nativelibs.vi.md)).

## Các cách đóng góp

- **Báo lỗi** — xem [Báo cáo lỗi](#báo-cáo-lỗi).
- **Đề xuất tính năng** — xem [Đề xuất tính năng](#đề-xuất-tính-năng).
- **Code** — patch JavaScript trong `scripts/patches/`, addon Rust trong `nativelibs/` và `zocr/`, Node.js và Python trong `zcall-native/`.
- **Tài liệu** — giữ bản tiếng Anh và tiếng Việt khớp nhau (xem [Tài liệu](#tài-liệu)).
- **Kiểm thử** — nhất là trên aarch64
- **Đóng gói** — AppImage là artefact chính thức; repo cũng có Nix flake (xem mục NixOS trong [README.vi.md](./README.vi.md)).

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

Cũng có thể chạy setup và đóng gói riêng:

```bash
npm run main:setup      # tải + giải nén vào app/ và temp/
npm run main:build      # đóng gói AppImage từ app/
npm run start           # chạy app đã giải nén ở chế độ phát triển
```

Danh sách đầy đủ các script, biến môi trường (`ZALO_VERSION`, `ZADARK_VERSION`, `FORCE_DOWNLOAD`) và cách thêm patch: xem [DEVELOPMENT.vi.md](./DEVELOPMENT.vi.md).

## Phong cách code

Patch nằm mỗi file một cái trong `scripts/patches/patch-<name>.js` và export `{ main }`. Luôn kiểm tra pattern mong đợi có tồn tại trước khi thay — vì Zalo cập nhật là code sẽ bị sửa -> không tìm được pattern để patch — rồi đăng ký patch trong `scripts/prepare-app.js` (xem mục "Adding a New Patch" trong [DEVELOPMENT.vi.md](./DEVELOPMENT.vi.md)).

## Kiểm thử

```bash
npm ci --prefer-offline --no-audit   # chạy một lần
npm test                             # node --test 'tests/**/*.test.js'
```

Thêm test dưới `tests/<area>-<topic>.test.js` — ví dụ `scripts-patches.test.js`, `plugins-badge-icon.test.js`, `zcall-zrtp-packet.test.js`.

Các crate Rust:

```bash
cargo test --manifest-path zocr/Cargo.toml --locked
for c in nativelibs/*/Cargo.toml; do cargo test --manifest-path "$c" --locked; done
```

Benchmark tuỳ chọn: `npm run bench` và `npm run bench:patches`.

## Tài liệu

Mỗi tài liệu tồn tại hai bản: `<NAME>.md` ở root (tiếng Anh) và `readme/<NAME>.vi.md` (tiếng Việt). Cả hai đều có dòng chuyển ngữ ngay dưới tiêu đề:

```markdown
**English** | [Tiếng Việt](./readme/<NAME>.vi.md)
```

```markdown
[English](../<NAME>.md) | **Tiếng Việt**
```

Cập nhật cả hai bản trong cùng một pull request.

## Commit và pull request

Pull request nhắm vào **`main`**.

- Tên nhánh: `feat/<slug>`, `fix/<slug>`, `docs/<slug>`, `chore/<slug>`, tuyệt đối không pr từ nhánh `main` của repo fork để nhắm vào nhánh `main` của repo gốc.
- Mỗi pull request một thay đổi logic, nếu sửa nhiều logic, vui lòng tách ra, việc tách mỗi pr 1 chức năng giúp cho reviewer dễ đọc code của bạn hơn.
- Luôn rebase nhánh `main` trước khi mở pr.
- `app.setName` trong `main.js` và mục StartupWMClass trong phần `build` của `package.json` **luôn phải là `Zalo`**.
- Nếu bạn phát triển một tính năng mới, vui lòng kiểm thử tính năng đó trên máy trước, sau đó chụp một ảnh màn hình về tính năng bạn mới thêm (nếu ảnh chụp chứa thông tin riêng tư/nhạy cảm của bạn, vui lòng làm mờ chúng, nếu không **chúng mình sẽ xóa ảnh chụp này vì quyền riêng tư của bạn**)
- Nếu bạn thêm/cập nhật một Native addon, vui lòng cập nhật file [ARCHITECTURE.md](./ARCHITECTURE.md) and [nativelibs/README.md](./nativelibs/README.md) (kể cả bản tiếng Việt của chúng), để những người mới có thể hiểu rõ được addon của bạn sẽ làm gì.
- Commit subject: tuân thủ convention, có thể tham khảo [link này](https://www.conventionalcommits.org/en/v1.0.0/).
- Nội dung pull request: thay đổi gì và vì sao, `Closes #N` nếu sửa một issue, nền tảng bạn đã thử (x86_64 hay aarch64, X11 hay Wayland), và screenshot cho thay đổi UI.

Checklist trước khi mở:

- [ ] `npm test` pass, và các test Rust ở trên pass nếu bạn sửa crate.
- [ ] Không có file sinh ra khi build (`app/`, `temp/`, `dist/`, `node_modules/`).
- [ ] Không có binary hay file `.node` dựng sẵn.
- [ ] Đã kiểm thử trên máy bạn và chụp ảnh màn hình.
- [ ] Tài liệu đã cập nhật cả hai ngôn ngữ.

## Báo cáo lỗi

Kèm theo:

- phiên bản và biến thể (Original hoặc ZaDark), cùng cách bạn cài (AppImage, AM, Gear Lever, NixOS, AUR);
- distro và phiên bản kernel;
- desktop environment và display server (X11 hay Wayland);
- các bước tái hiện, bạn mong đợi gì và thực tế ra sao;

## Đề xuất tính năng

Kiểm tra [FEATURES.vi.md](./FEATURES.vi.md), [KNOWN_ISSUES.vi.md](./KNOWN_ISSUES.vi.md) và các issue hiện có trước. Mô tả use case và vì sao nó thuộc bản port này chứ không phải bản thân Zalo.

## Sử dụng AI

Chúng mình hoan nghênh việc sử dụng các công cụ AI một cách có trách nhiệm (như Codex, Claude Code, v.v.) để hỗ trợ viết các patch, tài liệu hoặc gỡ lỗi. Tuy nhiên, vui lòng tuân thủ các quy định sau:

- **Rà soát lại mọi thứ:** Bạn chịu trách nhiệm hoàn toàn cho tất cả code và nội dung do mình gửi lên. Hãy đảm bảo bạn đã kiểm tra kỹ, kiểm thử và hiểu rõ bất kỳ kết quả nào do AI tạo ra trước khi mở pull request.
- **Khai báo nếu sử dụng nhiều:** Nếu pull request hoặc một phần lớn của patch/tài liệu được tạo ra hoặc hỗ trợ chủ yếu bởi AI, vui lòng đề cập ngắn gọn trong phần mô tả của pull request để đảm bảo tính minh bạch.
- **Không gửi code chưa qua kiểm chứng:** Không sao chép và dán mù quáng code do AI tạo ra, đặc biệt là đối với các addon gốc (`nativelibs/`, `zocr/`) hoặc các thành phần nhạy cảm về bảo mật, vì chúng có thể gây ra các lỗi an toàn bộ nhớ hoặc làm phát sinh lỗi cũ.

## Bảo mật

Không mở issue công khai cho lỗ hổng bảo mật. Dùng tab **Security** của repo và chọn "Report a vulnerability" (private advisory của GitHub).

## Giấy phép

Bằng việc đóng góp, bạn đồng ý rằng phần đóng góp của bạn được cấp phép theo cùng giấy phép với dự án — xem [LICENSE](../LICENSE). Repo này không có CLA. Zalo là thương hiệu của VNG Corporation; dự án này không liên kết với VNG Corporation và cũng không được VNG Corporation bảo trợ.

## Quy tắc ứng xử

Dự án theo [Contributor Covenant, phiên bản 2.0](https://www.contributor-covenant.org/version/2/0/code_of_conduct/).

Cảm ơn bạn đã đóng góp!
