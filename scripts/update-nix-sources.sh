#!/usr/bin/env bash

# Rewrite nix/sources.json (the AppImages the Nix flake installs) from the
# AppImages of a release. The release job runs it after each build, since a
# rebuild under the same tag deletes the assets the file pointed to.

set -euo pipefail

VER="${1:?Error: No VERSION given}"
DIST_DIR="${2:?Error: No DIST_DIR given}"
OUT="${3:-nix/sources.json}"

assets='{}'
shopt -s nullglob
for file in "${DIST_DIR}"/Zalo-*.AppImage; do
  name="$(basename "$file")"

  case "$name" in
    *+ZaDark-*) variant=zadark ;;
    *-Original-*) variant=original ;;
    *) echo "Skipping $name: unknown variant" >&2; continue ;;
  esac
  case "$name" in
    *-x86_64.AppImage) system=x86_64-linux ;;
    *-aarch64.AppImage) system=aarch64-linux ;;
    *) echo "Skipping $name: unknown architecture" >&2; continue ;;
  esac

  # SRI hash, the format fetchurl takes
  hash="sha256-$(openssl dgst -sha256 -binary "$file" | base64 -w0)"
  assets="$(jq --arg v "$variant" --arg s "$system" --arg n "$name" --arg h "$hash" \
    '.[$v][$s] = { name: $n, hash: $h }' <<<"$assets")"
done

if [[ "$assets" == '{}' ]]; then
  echo "Error: no AppImages in ${DIST_DIR}" >&2
  exit 1
fi

jq -n --arg ver "$VER" --argjson assets "$assets" '{ version: $ver, assets: $assets }' >"$OUT"
echo "Wrote $OUT for $VER"
