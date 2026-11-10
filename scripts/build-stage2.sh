#!/usr/bin/env bash

set -euo pipefail

# Directory of this adapter, so helper scripts resolve no matter which cwd the
# caller used before the packs below change directory.
SCRIPT_DIR="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")" && pwd)"

# Adapter between scripts/build.js and the AppImage toolchain (quick-sharun +
# appimagetool). All naming decisions live in build.js so there is only one
# source of truth; this script refuses to guess them.
#
# Arguments:
#   $1  VERSION      Zalo version, embedded in the AppImage metadata
#   $2  OUTNAME      final AppImage filename, produced by build.js
#   $3  DIST_DIR     output directory (also holds the AppDir and appimagetool)
#
# Required environment (set by build.js):
#   VARIANT          original | zadark
#   ARCH             x86_64 | aarch64 (uname style, used by appimagetool)
#   ARCH_SUFFIX      -x86_64 | -aarch64
#   UPINFO           zsync update information string
# Optional environment:
#   REUSE_APPDIR     1 = pack dist/squashfs-root as-is instead of extracting
#   KEEP_APPDIR      1 = leave the AppDir behind for the next build phase

VER="${1:-unknown}"
OUTNAME="${2:?Error: No OUTNAME given}"
DIST_DIR="${3:?Error: No DIST_DIR given}"

: "${VARIANT:?Error: VARIANT is required (original|zadark)}"
: "${ARCH:?Error: ARCH is required (x86_64|aarch64)}"
: "${ARCH_SUFFIX:?Error: ARCH_SUFFIX is required (-x86_64|-aarch64)}"
: "${UPINFO:?Error: UPINFO is required}"

case "$VARIANT" in
original)
  [[ "$OUTNAME" == *-Original-* ]] || {
    echo "Error: OUTNAME is not an Original build name: $OUTNAME" >&2
    exit 1
  }
  ;;
zadark)
  [[ "$OUTNAME" == *+ZaDark-* ]] || {
    echo "Error: OUTNAME is not a ZaDark build name: $OUTNAME" >&2
    exit 1
  }
  ;;
*)
  echo "Error: VARIANT must be 'original' or 'zadark', got: $VARIANT" >&2
  exit 1
  ;;
esac

[[ "$OUTNAME" == *"${ARCH_SUFFIX}.AppImage" ]] || {
  echo "Error: OUTNAME does not end with ${ARCH_SUFFIX}.AppImage: $OUTNAME" >&2
  exit 1
}

export APP_NAME="Zalo"
export DESKTOP="zalo.desktop"
export ICON="zalo.png"
export STARTUPWMCLASS="zalo"
export OUTPATH="${DIST_DIR}"
export VERSION="$VER"
export VARIANT ARCH ARCH_SUFFIX UPINFO
export OUTNAME

APPDIR="${DIST_DIR}/squashfs-root"
APPIMAGETOOL="${DIST_DIR}/appimagetool"

# REUSE_APPDIR=1: pack an AppDir that already exists instead of extracting one
# from a freshly built AppImage (used to derive the ZaDark variant from the
# Original AppDir without a second electron-builder run).
REUSE_APPDIR="${REUSE_APPDIR:-0}"
# KEEP_APPDIR=1: leave $APPDIR behind after packing so the next phase can reuse
# it.
KEEP_APPDIR="${KEEP_APPDIR:-0}"

export APPDIR

echo "=== Building ${VARIANT} AppImage for ${ARCH} ==="

# Prepare appimagetool (which is quick-sharun.sh in this case)
if [[ ! -f "$APPIMAGETOOL" ]]; then
  echo "Downloading appimagetool..."
  wget -q https://raw.githubusercontent.com/pkgforge-dev/Anylinux-AppImages/refs/heads/main/useful-tools/quick-sharun.sh -O "$APPIMAGETOOL"
  chmod +x "$APPIMAGETOOL"
fi

if [[ "${REUSE_APPDIR}" == "1" ]]; then
  echo "Reusing existing AppDir: $APPDIR"
  if [[ ! -d "$APPDIR" ]]; then
    echo "Error: Cannot find ${APPDIR} to reuse." >&2
    exit 1
  fi
else
  # Check if the original AppImage exists
  if [[ ! -f "${DIST_DIR}/${OUTNAME}" ]]; then
    echo "Error: Cannot find ${OUTNAME}, please run the builder first." >&2
    exit 1
  fi

  # Extract the original AppImage
  echo "Extracting AppImage..."
  chmod +x "${DIST_DIR}/${OUTNAME}"
  cd "$DIST_DIR"
  ./"$OUTNAME" --appimage-extract >/dev/null 2>&1

  if [[ ! -d "$APPDIR" ]]; then
    echo "Error: Cannot find ${APPDIR}, extraction failed." >&2
    exit 1
  fi
fi

# Remove any previous output before repacking
cd "$DIST_DIR"
rm -f "$OUTNAME"

# Trim the Electron runtime payload before repacking. Runs for both the fresh
# build and the reused ZaDark AppDir, and is idempotent.
node "${SCRIPT_DIR}/trim-appdir.js" "$APPDIR"

if [[ -z "${DWARFS_COMP:-}" ]]; then
  # Run on GitHub Actions AND (main branch OR tag) AND not a PR
  if [[ "${GITHUB_ACTIONS:-}" == "true" && ( "${GITHUB_REF:-}" == "refs/heads/main" || "${GITHUB_REF_TYPE:-}" == "tag" ) && "${GITHUB_EVENT_NAME:-}" != "pull_request" ]]; then
    export DWARFS_COMP="zstd:level=19 -B2 -S26"
  else
    export DWARFS_COMP="zstd:level=1 -B2 -S24"
  fi
fi

# Repack it with quick-sharun
echo "Packaging $OUTNAME..."
"$APPIMAGETOOL" --make-appimage

# Cleanup and chmod the output file
if [[ "${KEEP_APPDIR}" != "1" ]]; then
  rm -rf "$APPDIR"
fi
rm -rf "${DIST_DIR}/appinfo"
# quick-sharun already marks the AppImage executable, but it may pick a stale
# artifact from the glob in $OUTPATH. Set the mode on the file we just built so
# the output does not depend on what else is in dist/.
chmod +x "$OUTNAME"

echo "=== Build completed: ${DIST_DIR}/${OUTNAME} ==="
exit

