# zcall-bridge — gọi thoại/video/share screen trên Linux qua Wine

Chạy **ZaloCall.exe** (engine gọi của Zalo PC, bản Windows) dưới Wine, bắc cầu
giao tiếp bằng named pipes:

```
┌── Zalo app (Linux, đã patch) ────────────────────────────────┐
│  main process: listen TCP 127.0.0.1:29631 (recv) / 29632 (send)
│  spawn: wine pipebridge.exe 29631 29632
│  spawn: wine ZaloCall.exe \\.\pipe\PipeZCallRecv \\.\pipe\PipeZCallSend
└──────────────┬───────────────────────────────────────────────┘
               │ TCP loopback
┌──────────────▼───────────────────────────────────────────────┐
│  WINE prefix                                                  │
│  pipebridge.exe (C, không cần runtime) — named pipes ⇄ TCP   │
│  ZaloCall.exe — engine gọi thật (Qt5)                        │
└──────────────────────────────────────────────────────────────┘
```

Transport: AES-128-CBC, message JSON phân tách `$`.

## Cài đặt (máy build)

```bash
node scripts/setup-zcall-bridge.js
```

Tải installer Windows chính thức → tách `plugins/capture/` →
`app/native/qt-call-and-cap/` (tỉa còn ~67MB); compile `pipebridge.exe`
(mingw: `gcc-mingw-w64-i686`) và 2 shim `streamproxy.so` / `streamproxy-x86_64.so`
(multilib: `gcc-multilib libc6-dev-i386 libx11-dev:i386 libxcb1-dev:i386 libxext-dev:i386`).
⚠️ Không thêm 2 file `.so` vào `.gitignore` — electron-builder tôn trọng
`.gitignore` khi đóng gói `extraFiles`, ignore = AppImage ship thiếu shim.

**Wine yêu cầu**: bản **classic** (hỗ trợ 32-bit). App dùng và tự tải
[`wine-11.17-amd64.tar.xz`](https://github.com/Kron4ek/Wine-Builds/releases/download/11.17/wine-11.17-amd64.tar.xz)
— đã chạy video call ổn định trên Linux Mint.

## Người dùng cuối

Không cần cài gì thủ công:

- **Biến thể Full**: wine classic + GStreamer 64-bit bundle sẵn trong
  AppImage → mở là gọi/share màn hình được ngay.
- **Bản thường / package native**: lần đầu app tự tải wine (~96MB) +
  GStreamer (~105MB) về `<userData>/`; package native (deb/rpm/pacman) thay
  vào đó để package manager cài wine + gst từ repo.
- Có wine hệ thống hợp lệ → app dùng im lặng. Không có / không chạy được
  app 32-bit → hộp thoại hỏi tải, bật lại được qua menu khay
  → "Cài đặt gọi điện…".

Thứ tự dò wine: `ZCALL_WINE` → bundle Full → đã tải về → `wine` trong PATH →
runner Bottles.

### Biến môi trường chính

| Biến | Ý nghĩa |
|---|---|
| `ZCALL_WINE` | Đường dẫn tuyệt đối tới binary `wine` |
| `ZCALL_WINEPREFIX` | Prefix riêng của app (mặc định `<userData>/zcall-wine`) |
| `ZCALL_DISABLE` | Set giá trị bất kỳ để tắt tính năng gọi |
| `ZCALL_WINE_DOWNLOAD_URL` | Ghi đè URL tải wine portable |
| `ZCALL_CAMERA_LOCK_FMT` | `1` = khóa YUYV 640x480@30 (mặc định cho mọi wine — chất lượng cố định, không nhảy); `best` = tự dò format tốt nhất (HD cho máy mạnh) |
| `ZCALL_CAMERA_HIDE` | Ẩn camera (gọi video một chiều, không crash) |

## Thư viện 32-bit cho wine classic

App tự hiện hướng dẫn đúng distro khi thiếu. Copy-paste nhanh:

```bash
# Ubuntu/Debian
sudo dpkg --add-architecture i386 && sudo apt update
sudo apt install -y libc6:i386 libx11-6:i386 libfreetype6:i386 libgl1:i386 \
  libpulse0:i386 libasound2:i386 zlib1g:i386 \
  libgstreamer1.0-0:i386 libgstreamer-plugins-base1.0-0:i386 \
  gstreamer1.0-plugins-good:i386 gstreamer1.0-libav:i386 libv4l-0:i386

# Fedora (H.264 decode cần RPM Fusion: gstreamer1-plugin-libav.i686)
sudo dnf install -y glibc.i686 libX11.i686 freetype.i686 mesa-libGL.i686 \
  pulseaudio-libs.i686 alsa-lib.i686 zlib-ng-compat.i686 \
  gstreamer1.i686 gstreamer1-plugins-base.i686 gstreamer1-plugins-good.i686 libv4l.i686

# Arch (bật multilib)
sudo pacman -S --needed lib32-glibc lib32-libx11 lib32-freetype2 lib32-mesa \
  lib32-libpulse lib32-alsa-lib lib32-zlib \
  lib32-gstreamer lib32-gst-plugins-base lib32-gst-plugins-good lib32-gst-libav lib32-libv4l
```

## Share screen trên Wayland

XWayland không thấy desktop → app có bridge riêng: XDG ScreenCast portal →
PipeWire → GStreamer → **Xvfb headless `:99`** → shim `streamproxy.so` chặn
lời gọi chụp màn hình của ZaloCall và trả nội dung từ `:99`. Bấm Share screen
như thường — hộp thoại quyền portal tự hiện. Trên X11 hoạt động trực tiếp,
không cần bridge.

## Lưu ý

- **Chỉ dùng wine classic**: bản wow64 crash camera DirectShow trên một số
  máy (bug marshaling WoW64 trong qcap của wine, chưa merge tới 11.17 — repro
  trong `camtest.c`).
- **Camera**: app tự khóa YUYV 640x480@30 cho mượt (720p qua wine là
  CPU-bound). Máy mạnh muốn HD: `ZCALL_CAMERA_LOCK_FMT=best`.
- **Prefix**: `<userData>/zcall-wine`, không đụng `~/.wine`; app tự dọn
  wine session khi thoát. Reset trạng thái gọi: xóa thư mục prefix.

## Đã xác minh

- Gọi thoại + video call hoạt động: Mint 22 (wine 11.17 classic), Ubuntu
  26.04, Fedora (wine 11.14/11.17 + gst 32-bit).
- Share screen Wayland hoạt động (KDE, người dùng xác nhận).
- Bundle Full self-contained — 4 gate tự động tại build (ldd, dlopen
  RTLD_NOW, python dbus+gi, gst-inspect + Xvfb smoke).
- Wine classic 11.17: `wineboot` + pipebridge + ZaloCall + camera DirectShow
  chạy thật (repro `camtest.c` bắt frame thành công).
