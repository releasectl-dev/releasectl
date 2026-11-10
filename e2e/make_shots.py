#!/usr/bin/env python3
"""E2E 用の掲載画像を作る（標準ライブラリだけ。単色 + 帯で 1 枚ずつ中身を変える）。

ストアの規定サイズ:
  App Store APP_IPHONE_67      1290x2796
  Play phoneScreenshots         1080x1920
  Play featureGraphic           1024x500
  Play icon                     512x512（32bit PNG）
  App Store 検索結果のクリエイティブ 3840x2560（3:2）

usage: make_shots.py [VARIANT]   VARIANT を変えると全画像の中身（ハッシュ）が変わる（差し替えの確認用）
"""
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent / "shots"
VARIANT = int(sys.argv[1]) if len(sys.argv) > 1 else 0


def png(path: Path, w: int, h: int, rgb: tuple, band: int, alpha: bool = False) -> None:
    ch = 4 if alpha else 3
    px = bytes(rgb) + (b"\xff" if alpha else b"")
    dark = bytes(c // 2 for c in rgb) + (b"\xff" if alpha else b"")
    rows = []
    for y in range(h):
        # 上から band 番目の帯だけ暗くする（画像ごとに位置を変えてハッシュを変える）
        line = dark * w if (y // 40) == band else px * w
        rows.append(b"\x00" + line)
    raw = b"".join(rows)

    def chunk(t: bytes, d: bytes) -> bytes:
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    color_type = 6 if alpha else 2
    data = (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, color_type, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw, 9))
            + chunk(b"IEND", b""))
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


COLORS = {"en": (40, 110, 200), "ja": (200, 70, 60)}
for lang, rgb in COLORS.items():
    for i in range(2):
        band = 2 + i * 3 + VARIANT
        png(ROOT / "appstore_iphone" / lang / f"0{i + 1}.png", 1290, 2796, rgb, band)
        png(ROOT / "play_phone" / lang / f"0{i + 1}.png", 1080, 1920, rgb, band)
    png(ROOT / "play_feature" / lang / "feature.png", 1024, 500, rgb, 1 + VARIANT)
    png(ROOT / "play_icon" / lang / "icon.png", 512, 512, rgb, 1 + VARIANT, alpha=True)
    png(ROOT / "search" / lang / "search.png", 3840, 2560, rgb, 3 + VARIANT)
print(f"wrote {ROOT} (variant {VARIANT})")
