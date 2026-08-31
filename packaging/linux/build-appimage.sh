#!/bin/bash
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Build the Sentin-NPU AppImage from an already staged Linux bundle.
#
#   ./build-appimage.sh <staged-bundle-dir> <version> [output-dir]
#
# The staged directory is what scripts/make-release.sh produces: binaries, lib/, models/, wazuh/.
# Everything goes inside the AppImage, so the result runs on any glibc 2.31 or newer with no Rust,
# no Python and no OpenVINO installed.
#
# appimagetool is fetched once into the output directory and reused. It is run with
# --appimage-extract-and-run because CI runners have no FUSE, and without that flag the tool exits
# with a mount error that reads like a build failure.

set -euo pipefail

STAGE=${1:?usage: build-appimage.sh <staged-bundle-dir> <version> [output-dir]}
VERSION=${2:?usage: build-appimage.sh <staged-bundle-dir> <version> [output-dir]}
OUT=${3:-$(cd "$(dirname "$0")/../../dist" 2>/dev/null && pwd || echo dist)}
HERE=$(cd "$(dirname "$0")" && pwd)

[ -d "$STAGE" ] || { echo "no staged bundle at $STAGE" >&2; exit 1; }
mkdir -p "$OUT"

APPDIR="$OUT/Sentin-NPU.AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" "$APPDIR/usr/share/sentin-npu" \
         "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/128x128/apps"

echo "== payload"
for binary in sentin-gateway sentin-doctor sentin-bench; do
    if [ -f "$STAGE/$binary" ]; then
        install -m 755 "$STAGE/$binary" "$APPDIR/usr/bin/$binary"
    else
        echo "  missing $binary in the staged bundle" >&2; exit 1
    fi
done
cp -a "$STAGE/lib/." "$APPDIR/usr/lib/"
cp -a "$STAGE/models" "$APPDIR/usr/share/sentin-npu/"
[ -d "$STAGE/wazuh" ] && cp -a "$STAGE/wazuh" "$APPDIR/usr/share/sentin-npu/"

# dlopen looks for unversioned sonames and the OpenVINO wheel ships only versioned ones. The
# staged bundle already has these links, but cp -a through a filesystem that drops them (or a
# staging step that copied rather than archived) would leave the AppImage failing at startup with
# "Unable to find the openvino_c library", which names no cause.
( cd "$APPDIR/usr/lib"
  for f in *.so.*; do
      [ -e "$f" ] || continue
      base="${f%%.so.*}.so"
      [ -e "$base" ] || ln -sf "$f" "$base"
  done )

echo "== metadata"
install -m 755 "$HERE/AppRun" "$APPDIR/AppRun"
install -m 644 "$HERE/sentin-npu.png" "$APPDIR/sentin-npu.png"
install -m 644 "$HERE/sentin-npu.png" "$APPDIR/usr/share/icons/hicolor/128x128/apps/sentin-npu.png"
cp "$APPDIR/sentin-npu.png" "$APPDIR/.DirIcon"

cat > "$APPDIR/sentin-npu.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Sentin-NPU
GenericName=LLM privacy gateway
Comment=Detects and masks identifiers before a prompt leaves this machine
Exec=AppRun
Icon=sentin-npu
Terminal=true
Categories=Utility;Security;Network;
Keywords=DLP;privacy;LLM;NPU;OpenVINO;
DESKTOP
cp "$APPDIR/sentin-npu.desktop" "$APPDIR/usr/share/applications/"

echo "== appimagetool"
# Cached in a dot directory rather than beside the artefacts. The release workflow globs
# dist/*.AppImage, and v0.0.0.11 duly published appimagetool as though it were ours.
TOOLS="$OUT/.tools"
mkdir -p "$TOOLS"
TOOL="$TOOLS/appimagetool-x86_64.AppImage"
if [ ! -x "$TOOL" ]; then
    curl -fsSL -o "$TOOL" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
    chmod +x "$TOOL"
fi

echo "== packing"
TARGET="$OUT/Sentin-NPU-${VERSION}-x86_64.AppImage"
rm -f "$TARGET"
ARCH=x86_64 "$TOOL" --appimage-extract-and-run "$APPDIR" "$TARGET" >/dev/null
chmod +x "$TARGET"
rm -rf "$APPDIR"

printf '  %s (%s)\n' "$TARGET" "$(du -h "$TARGET" | cut -f1)"
