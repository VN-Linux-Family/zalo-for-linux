#!/bin/bash
# 1) Icon fallback
cp /opt/Zalo/app/pc-dist/favicon-512x512.png /usr/share/pixmaps/zalo.png 2>/dev/null || true
sed -i 's|^Icon=.*|Icon=/usr/share/pixmaps/zalo.png|' /usr/share/applications/zalo.desktop 2>/dev/null || true

# 2) Pre-fetch wine
WINE_URL="https://github.com/Kron4ek/Wine-Builds/releases/download/11.17/wine-11.17-amd64.tar.xz"
EXPECTED_VERSION="wine-11.17"
WINE_DIR="/opt/Zalo/app/native/wine-runtime"

NEED=1
if [ -x "$WINE_DIR/bin/wine" ]; then
  CUR=$("$WINE_DIR/bin/wine" --version 2>/dev/null || true)
  [ "$CUR" = "$EXPECTED_VERSION" ] && NEED=0
fi

if [ "$NEED" = "1" ]; then
  rm -rf "$WINE_DIR"
  mkdir -p "$WINE_DIR"
  TMP=$(mktemp -d)
  if curl -fsSL --retry 3 -o "$TMP/wine.tar.xz" "$WINE_URL" \
      && tar -xf "$TMP/wine.tar.xz" -C "$WINE_DIR" --strip-components=1; then
    rm -rf "$WINE_DIR/include"
    find "$WINE_DIR/lib" -type f \( -name '*.a' -o -name '*.def' -o -name '*.c' \) -delete 2>/dev/null || true
    (cd "$WINE_DIR/bin" && ls | grep -vE '^(wine|wineserver|wineboot)$' | xargs -r rm -f)
    echo "zcall: bundled $EXPECTED_VERSION classic installed to $WINE_DIR"
  else
    rm -rf "$WINE_DIR"
    echo "zcall: wine download failed (offline?) — the app will download it on first run"
  fi
  rm -rf "$TMP"
fi

# 3) Electron sandbox fix
if [ -f /usr/share/applications/zalo.desktop ]; then
  sed -i 's|Exec=/opt/Zalo/zalo %U|Exec=/opt/Zalo/zalo --no-sandbox %U|' /usr/share/applications/zalo.desktop 2>/dev/null || true
fi

# 4) Camera capture: nothing to install. The Ubuntu 24.04 camera bug
#    (wine capture stalls with DQBUF EPIPE) comes from the host's i386
#    GStreamer 1.24 + libv4l 1.26 stack — the app ships its own bundled
#    i386 stack (gst 1.28 + libv4l 1.32, zcall-bridge/gst-i386) and points
#    classic wine at it via applyWineEnv, so no kernel module, no DKMS and
#    no camera exclusivity are needed. (Older builds installed
#    v4l2loopback; after-remove still cleans those configs up.)

exit 0
