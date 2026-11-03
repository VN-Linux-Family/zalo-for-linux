#!/usr/bin/env bash
set -euo pipefail
shopt -s nullglob
distro="${1:?distro required}"
package_dir="$(realpath "${2:?package directory required}")"

case "$distro" in
  ubuntu|debian)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update
    apt-get install -y desktop-file-utils findutils xz-utils
    packages=("$package_dir"/*-deb-*.deb)
    install_package() { apt-get install -y "$1"; }
    remove_package() { apt-get remove -y "$1"; }
    ;;
  fedora)
    dnf install -y desktop-file-utils findutils xz
    packages=("$package_dir"/*-fedora-*.rpm)
    install_package() { dnf install -y "$1"; }
    remove_package() { dnf remove -y "$1"; }
    ;;
  opensuse)
    zypper --non-interactive install desktop-file-utils findutils xz
    packages=("$package_dir"/*-opensuse-*.rpm)
    # Local CI artifacts are unsigned; do not disable verification for repositories.
    install_package() { zypper --non-interactive install --allow-unsigned-rpm "$1"; }
    remove_package() { zypper --non-interactive remove "$1"; }
    ;;
  arch)
    pacman -Syu --noconfirm desktop-file-utils findutils xz
    packages=("$package_dir"/*-arch-*.pkg.tar.xz)
    install_package() { pacman -U --noconfirm "$1"; }
    remove_package() { pacman -R --noconfirm "$1"; }
    ;;
  *) echo "Unsupported test distro: $distro" >&2; exit 1 ;;
esac

check_payload() {
  local root="$1"
  test -x "$root/zalo"
  test -f "$root/app/bootstrap.js"
  test -f "$root/zcall-native/native-engine/zcall-native.js"
  test -f "$root/zcall-native/tools/opus_play.py"
  ldd "$root/zalo" > /tmp/zalo-libraries.txt
  find "$root" -name '*.node' -exec ldd {} \; >> /tmp/zalo-libraries.txt
  cat /tmp/zalo-libraries.txt
  if grep -q 'not found' /tmp/zalo-libraries.txt; then return 1; fi
  python3 -c 'import ctypes; ctypes.CDLL("libopus.so.0")'
  command -v parec
  command -v pacat
}

# Both Original and ZaDark must be produced and install independently.
test "${#packages[@]}" -eq 2
for package in "${packages[@]}"; do
  install_package "$package"
  check_payload /opt/Zalo
  command -v zalo
  desktop-file-validate /usr/share/applications/zalo.desktop

  # Archives are relocatable, but rely on the same system libraries as native
  # packages. Check with those dependencies installed, before removing Zalo.
  archives=("$package_dir"/*-portable-*.tar.xz)
  test "${#archives[@]}" -eq 2
  for archive in "${archives[@]}"; do
    archive_dir="$(mktemp -d)"
    tar -xJf "$archive" -C "$archive_dir"
    binary="$(find "$archive_dir" -type f -name zalo -print -quit)"
    test -n "$binary"
    check_payload "$(dirname "$binary")"
    rm -rf "$archive_dir"
  done

  installed_name=zalo-for-linux
  if [[ "$package" == *+ZaDark-* ]]; then installed_name=zalo-for-linux-zadark; fi
  remove_package "$installed_name"
  test ! -e /usr/share/applications/zalo.desktop
  test ! -e /opt/Zalo/zalo
done
