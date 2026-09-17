# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0

"""Draw the application icon.

The icon is generated rather than drawn by hand and committed as an opaque blob, for the same
reason the Wazuh dashboard and the benchmark charts are generated: a change to a ``.ico`` reviews
as "binary files differ", which is no review at all, while a change here reads as a diff. The
committed output still goes into git, because the Windows build needs the file and must not need
Python to get it.

Three outputs, because they are consumed by three things and no two of them read the same format:

``packaging/windows/sentin-npu.ico``
    the icon resource linked into ``sentin-ui.exe`` and used by the installer and its shortcuts.

``packaging/linux/sentin-npu.png``
    what the AppImage hands the desktop - its ``.DirIcon`` and the ``Icon=`` of its ``.desktop``
    entry. It is written from the same 128 px frame that goes into the ``.ico``, byte for byte, so
    the two platforms cannot end up showing different pictures of the same program. They already
    had: this file was a 484-byte placeholder from the day the AppImage was first built, and it
    survived the release that gave Windows a real icon.

``gateway/crates/sentin-ui/assets/icon-64.rgba``
    raw pixels for the window itself. eframe wants ``IconData`` as plain RGBA, and shipping it that
    way keeps a PNG decoder out of the console's dependency tree - the Windows binaries are
    cross-compiled with mingw, and every dependency added to that path is one more thing that can
    refuse to build there.

Run it with any Python 3; it imports nothing outside the standard library.

    py -3 tools/make_icon.py
"""

from __future__ import annotations

import pathlib
import struct
import sys
import zlib

# The darkest blue of docs/architecture.svg, so the icon belongs to the same drawing as the
# diagram and the charts rather than introducing a fourth palette.
BLUE = (0x1F, 0x4E, 0x79)
WHITE = (0xFF, 0xFF, 0xFF)

# Sizes Windows actually asks for: 16 in a menu, 32 on the desktop, 48 in a large-icon view, 256
# for the preview pane. The ones between are there so the shell never has to scale one of these
# down itself, which is where a shield turns to mush.
SIZES = (256, 128, 64, 48, 32, 24, 16)

# What the AppImage installs, so it has to be one of SIZES: build-appimage.sh puts the file in
# hicolor/128x128/apps, and a 128x128 directory holding something else is a lie the desktop
# believes.
LINUX_SIZE = 128

# Each pixel is sampled this many times per axis. The shield is all curves and diagonals, and at
# 16 pixels the difference between sampled and not sampled is the difference between a shield and
# a blue smudge.
SUPERSAMPLE = 8


def shield_half_width(y: float) -> float:
    """Half the shield's width at height `y`, in a unit square, or 0.0 outside it.

    Flat across the top, straight down the sides, then an elliptical taper to a point - the shape
    stays readable when it is sixteen pixels wide, which a crest with shoulders does not.
    """
    top, shoulder, tip, half = 0.07, 0.55, 0.96, 0.29
    if y < top or y > tip:
        return 0.0
    if y <= shoulder:
        return half
    t = (y - shoulder) / (tip - shoulder)
    # `1 - t**k` and not `(1 - t**k) ** m`. The exponent outside is what decides whether the bottom
    # is a point or a curve, and anything below one makes the width collapse tangentially - a
    # parabola, so a letter U. This form reaches zero with a finite slope, which is a point, and k
    # above one keeps it wide near the shoulder so the sides read as a shield rather than a wedge.
    return half * (1.0 - t**2.0)


def sample(x: float, y: float) -> tuple[int, int, int, int]:
    """The colour at one point of the unit square."""
    half = shield_half_width(y)
    if half == 0.0 or abs(x - 0.5) > half:
        return (0, 0, 0, 0)
    # A redaction bar, which is the product: an identifier replaced by a block before the request
    # leaves. It stops short of the shield's edge on both sides, because a band running edge to
    # edge cuts the silhouette into two shapes that no longer read as one shield - which is what
    # the first two attempts did.
    if 0.45 <= y <= 0.55 and abs(x - 0.5) <= 0.155:
        return (*WHITE, 255)
    return (*BLUE, 255)


def render(size: int) -> bytes:
    """Draw the icon at `size` pixels square, as RGBA rows."""
    rows = bytearray()
    step = 1.0 / (size * SUPERSAMPLE)
    for py in range(size):
        row = bytearray()
        for px in range(size):
            r = g = b = a = 0
            for sy in range(SUPERSAMPLE):
                for sx in range(SUPERSAMPLE):
                    x = (px * SUPERSAMPLE + sx + 0.5) * step
                    y = (py * SUPERSAMPLE + sy + 0.5) * step
                    sr, sg, sb, sa = sample(x, y)
                    # Weight colour by coverage, or the transparent samples around the edge drag
                    # every border pixel towards black.
                    r += sr * sa
                    g += sg * sa
                    b += sb * sa
                    a += sa
            if a == 0:
                row += b"\x00\x00\x00\x00"
            else:
                n = SUPERSAMPLE * SUPERSAMPLE
                row += bytes((r // a, g // a, b // a, a // n))
        rows += row
    return bytes(rows)


def to_png(size: int, rgba: bytes) -> bytes:
    """Wrap raw RGBA rows in a PNG."""

    def chunk(kind: bytes, payload: bytes) -> bytes:
        return (
            struct.pack(">I", len(payload))
            + kind
            + payload
            + struct.pack(">I", zlib.crc32(kind + payload) & 0xFFFFFFFF)
        )

    # Filter byte 0 (none) in front of every scanline. Filtering would shrink the file; the whole
    # set is under 30 kB either way, and unfiltered rows are what makes this readable.
    raw = b"".join(b"\x00" + rgba[y * size * 4 : (y + 1) * size * 4] for y in range(size))
    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def to_ico(frames: list[tuple[int, bytes]]) -> bytes:
    """Pack PNG frames into an ICO.

    PNG-compressed frames rather than the older BMP ones: every Windows since Vista reads them, and
    a 256x256 BMP frame alone would be 256 kB of the file.
    """
    offset = 6 + 16 * len(frames)
    directory = bytearray(struct.pack("<HHH", 0, 1, len(frames)))
    for size, png in frames:
        # 0 means 256 in this field, which is the whole reason it is one byte wide.
        directory += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), offset)
        offset += len(png)
    return bytes(directory) + b"".join(png for _, png in frames)


def main() -> int:
    """Write all three files and say what went where."""
    repo = pathlib.Path(__file__).resolve().parent.parent
    ico_path = repo / "packaging" / "windows" / "sentin-npu.ico"
    png_path = repo / "packaging" / "linux" / "sentin-npu.png"
    rgba_path = repo / "gateway" / "crates" / "sentin-ui" / "assets" / "icon-64.rgba"
    rgba_path.parent.mkdir(parents=True, exist_ok=True)

    frames = [(size, to_png(size, render(size))) for size in SIZES]
    ico_path.write_bytes(to_ico(frames))
    print(f"{ico_path}  {ico_path.stat().st_size} bytes, {len(SIZES)} sizes")

    # The same bytes that went into the .ico, not a second render of the same drawing. Rendering it
    # again would produce identical pixels today and would be one edit away from not doing so.
    png = dict(frames)[LINUX_SIZE]
    png_path.write_bytes(png)
    print(f"{png_path}  {len(png)} bytes, {LINUX_SIZE}x{LINUX_SIZE} PNG")

    window = render(64)
    rgba_path.write_bytes(window)
    print(f"{rgba_path}  {len(window)} bytes, 64x64 RGBA")
    return 0


if __name__ == "__main__":
    sys.exit(main())
