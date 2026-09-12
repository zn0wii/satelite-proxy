#!/usr/bin/env python3
"""Generate Satelite icons: app icon from assets/icon/ + satellite mark (tray).

  pip install pillow
  python3 scripts/generate-icons.py           # app + tray
  python3 scripts/generate-icons.py --tray     # tray only

The app icon (icon.png / .ico / .icns / Square*Logo) is resampled from
assets/icon/ic_launcher-web.png (512px smiley tile, Android web icon).
The .icns writer is pure Python (PNG payloads), so no iconutil / macOS needed.

Previous flat marks are kept in src-tauri/icons/tray-legacy/ (copy once, never overwritten).
"""
from __future__ import annotations

import argparse
import math
import shutil
import struct
from io import BytesIO
from pathlib import Path

from PIL import Image, ImageDraw, ImageChops

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "src-tauri" / "icons"
APP_ICON_SOURCE = ROOT / "assets" / "icon" / "ic_launcher-web.png"

# Tray badge (preview 16): black rounded tile, white mark off, mid mint on.
TRAY_RUNNING = (46, 190, 132, 255)  # #2EBE84
TRAY_STOPPED = (208, 208, 208, 255)  # #D0D0D0 preview B
TRAY_BADGE_BG = (12, 12, 14, 255)
TRAY_GHOST_EYE_ON = (143, 232, 192, 255)  # #8FE8C0
TRAY_DIR = OUT / "tray"
TRAY_LEGACY = OUT / "tray-legacy"
TRAY_PNGS = (
    "tray-icon.png",
    "tray-icon-template.png",
    "tray-icon-running.png",
    "tray-icon-32.png",
    "tray-icon-22.png",
    "tray-icon-template-32.png",
    "tray-icon-template-22.png",
)


_APP_ICON_SRC: Image.Image | None = None


def app_icon_source() -> Image.Image:
    global _APP_ICON_SRC
    if _APP_ICON_SRC is None:
        _APP_ICON_SRC = Image.open(APP_ICON_SOURCE).convert("RGBA")
    return _APP_ICON_SRC


def make_app_icon(size: int) -> Image.Image:
    """Resample the smiley tile to `size`. Halving steps keep small sizes crisp."""
    im = app_icon_source()
    while im.size[0] // 2 >= size:
        im = im.resize((im.size[0] // 2,) * 2, Image.Resampling.LANCZOS)
    if im.size != (size, size):
        im = im.resize((size, size), Image.Resampling.LANCZOS)
    return im


def _rot45(x: float, y: float, cx: float, cy: float) -> tuple[float, float]:
    dx, dy = x - cx, y - cy
    s = math.sqrt(2) / 2
    return cx + (dx - dy) * s, cy + (dx + dy) * s


def _rounded_square(cx: float, cy: float, half: float, radius: float, n_arc: int = 18):
    """Clockwise rounded square, y-down. Includes side samples so joins stay smooth."""
    r = min(radius, half * 0.95)
    # TR, BR, BL, TL — each arc then the outgoing side.
    specs = (
        (cx + half - r, cy - half + r, 270, 360, (cx + half, cy - half + r), (cx + half, cy + half - r)),
        (cx + half - r, cy + half - r, 0, 90, (cx + half - r, cy + half), (cx - half + r, cy + half)),
        (cx - half + r, cy + half - r, 90, 180, (cx - half, cy + half - r), (cx - half, cy - half + r)),
        (cx - half + r, cy - half + r, 180, 270, (cx - half + r, cy - half), (cx + half - r, cy - half)),
    )
    pts: list[tuple[float, float]] = []
    n_side = 12
    for ax, ay, a0, a1, s0, s1 in specs:
        for i in range(n_arc + 1):
            t = math.radians(a0 + (a1 - a0) * i / n_arc)
            pts.append((ax + r * math.cos(t), ay + r * math.sin(t)))
        for i in range(1, n_side):
            t = i / n_side
            pts.append((s0[0] + (s1[0] - s0[0]) * t, s0[1] + (s1[1] - s0[1]) * t))
    return pts


def _diamond(cx: float, cy: float, half: float, corner: float):
    return [_rot45(x, y, cx, cy) for x, y in _rounded_square(cx, cy, half, corner, n_arc=48)]


def _punch(img: Image.Image, mask: Image.Image) -> Image.Image:
    r, g, b, a = img.split()
    return Image.merge("RGBA", (r, g, b, ImageChops.subtract(a, mask)))


def draw_satellite_mark(
    size: int, color: tuple[int, int, int, int]
) -> Image.Image:
    """Rounded diamond + inner diamond + gap satellite. From assets/tray.png."""
    hi = 1024
    img = Image.new("RGBA", (hi, hi), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    cx = cy = hi / 2.0

    half = hi * 0.270
    corner = half * 0.22
    stroke = hi * 0.074

    d.polygon(_diamond(cx, cy, half + stroke / 2, corner + stroke / 2), fill=color)
    hole = Image.new("L", (hi, hi), 0)
    ImageDraw.Draw(hole).polygon(
        _diamond(cx, cy, half - stroke / 2, max(2.0, corner - stroke / 2)),
        fill=255,
    )
    img = _punch(img, hole)

    # NE edge runs N(t=0) → E(t=1). Punch just before the east hook.
    r_diag = half * math.sqrt(2)
    t_gap = 0.78
    gx = cx + t_gap * r_diag
    gy = cy - (1.0 - t_gap) * r_diag
    gap_r = stroke * 1.70
    gap = Image.new("L", (hi, hi), 0)
    ImageDraw.Draw(gap).ellipse(
        [gx - gap_r, gy - gap_r, gx + gap_r, gy + gap_r], fill=255
    )
    img = _punch(img, gap)

    d = ImageDraw.Draw(img)
    inner_half = half * 0.42
    d.polygon(
        [
            (cx, cy - inner_half),
            (cx + inner_half, cy),
            (cx, cy + inner_half),
            (cx - inner_half, cy),
        ],
        fill=color,
    )

    # Outward normal of the NE edge is (1, -1).
    sat_r = stroke * 0.50
    n = math.sqrt(2)
    sx = gx + (stroke * 0.55) * (1.0 / n)
    sy = gy + (stroke * 0.55) * (-1.0 / n)
    d.ellipse([sx - sat_r, sy - sat_r, sx + sat_r, sy + sat_r], fill=color)

    return img.resize((size, size), Image.Resampling.LANCZOS)


def make_tray(size: int, color: tuple[int, int, int, int]) -> Image.Image:
    return draw_satellite_mark(size, color)


def draw_tray_badge(
    size: int,
    mark_color: tuple[int, int, int, int],
    bg: tuple[int, int, int, int] = TRAY_BADGE_BG,
) -> Image.Image:
    """Rounded tile + satellite mark."""
    hi = 1024
    img = Image.new("RGBA", (hi, hi), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    pad = int(hi * 0.10)
    radius = int((hi - 2 * pad) * 0.28)
    d.rounded_rectangle(
        [pad, pad, hi - 1 - pad, hi - 1 - pad], radius=radius, fill=bg
    )
    mark = draw_satellite_mark(int(hi * 0.78), mark_color)
    ox = (hi - mark.size[0]) // 2
    oy = (hi - mark.size[1]) // 2
    img.paste(mark, (ox, oy), mark)
    return img.resize((size, size), Image.Resampling.LANCZOS)


def draw_ghost(size: int, eye_color: tuple[int, int, int, int]) -> Image.Image:
    """Pac-Man sheet ghost from assets/ghost.png. Eyes recolored."""
    from collections import deque

    src = Image.open(ROOT / "assets" / "ghost.png").convert("RGBA")
    w, h = src.size
    pix = src.load()

    def light(x: int, y: int) -> bool:
        r, g, b, _ = pix[x, y]
        return r + g + b > 560

    bg = [[False] * w for _ in range(h)]
    q: deque[tuple[int, int]] = deque()
    for x in range(w):
        for y in (0, h - 1):
            if light(x, y):
                bg[y][x] = True
                q.append((x, y))
    for y in range(h):
        for x in (0, w - 1):
            if light(x, y) and not bg[y][x]:
                bg[y][x] = True
                q.append((x, y))
    while q:
        x, y = q.popleft()
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            if 0 <= nx < w and 0 <= ny < h and not bg[ny][nx] and light(nx, ny):
                bg[ny][nx] = True
                q.append((nx, ny))

    cut = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    outp = cut.load()
    for y in range(h):
        for x in range(w):
            if bg[y][x]:
                continue
            r, g, b, _ = pix[x, y]
            lum = (r + g + b) // 3
            if lum > 186:
                outp[x, y] = eye_color
            else:
                outp[x, y] = (0, 0, 0, 255 if lum < 48 else max(0, 255 - lum))

    box = cut.getbbox()
    if box:
        cut = cut.crop(box)
    hi = 1024
    canvas = Image.new("RGBA", (hi, hi), (0, 0, 0, 0))
    pad = int(hi * 0.10)
    inner = hi - 2 * pad
    fitted = cut.resize((inner, inner), Image.Resampling.LANCZOS)
    canvas.paste(fitted, (pad, pad), fitted)
    return canvas.resize((size, size), Image.Resampling.LANCZOS)


def _fit_cutout(cut: Image.Image, size: int) -> Image.Image:
    box = cut.getbbox()
    if box:
        cut = cut.crop(box)
    hi = 1024
    canvas = Image.new("RGBA", (hi, hi), (0, 0, 0, 0))
    pad = int(hi * 0.08)
    inner = hi - 2 * pad
    fitted = cut.resize((inner, inner), Image.Resampling.LANCZOS)
    canvas.paste(fitted, (pad, pad), fitted)
    return canvas.resize((size, size), Image.Resampling.LANCZOS)


def draw_buddy(size: int, glasses_color: tuple[int, int, int, int] | None) -> Image.Image:
    """head.jpg on transparent. `glasses_color` None = black shades; else recolor lenses."""
    from collections import deque

    src = Image.open(ROOT / "assets" / "head.jpg").convert("RGBA")
    w, h = src.size
    pix = src.load()

    def light(x: int, y: int) -> bool:
        r, g, b, _ = pix[x, y]
        return r + g + b > 560

    def ink(x: int, y: int) -> bool:
        r, g, b, _ = pix[x, y]
        return r + g + b < 360

    bg = [[False] * w for _ in range(h)]
    q: deque[tuple[int, int]] = deque()
    for x in range(w):
        for y in (0, h - 1):
            if light(x, y):
                bg[y][x] = True
                q.append((x, y))
    for y in range(h):
        for x in (0, w - 1):
            if light(x, y) and not bg[y][x]:
                bg[y][x] = True
                q.append((x, y))
    while q:
        x, y = q.popleft()
        for nx, ny in ((x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)):
            if 0 <= nx < w and 0 <= ny < h and not bg[ny][nx] and light(nx, ny):
                bg[ny][nx] = True
                q.append((nx, ny))

    shades: set[tuple[int, int]] = set()
    if glasses_color is not None:
        x0, x1 = int(w * 0.40), int(w * 0.85)
        y0, y1 = int(h * 0.38), int(h * 0.52)
        seen = [[False] * w for _ in range(h)]
        best: list[tuple[int, int]] = []
        for y in range(y0, y1):
            for x in range(x0, x1):
                if seen[y][x] or not ink(x, y):
                    continue
                nq = deque([(x, y)])
                seen[y][x] = True
                pts: list[tuple[int, int]] = []
                while nq:
                    cx, cy = nq.popleft()
                    pts.append((cx, cy))
                    for nx, ny in ((cx - 1, cy), (cx + 1, cy), (cx, cy - 1), (cx, cy + 1)):
                        if x0 <= nx < x1 and y0 <= ny < y1 and not seen[ny][nx] and ink(nx, ny):
                            seen[ny][nx] = True
                            nq.append((nx, ny))
                if len(pts) > len(best):
                    best = pts
        shades = set(best)

    cut = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    outp = cut.load()
    for y in range(h):
        for x in range(w):
            if bg[y][x]:
                continue
            r, g, b, _ = pix[x, y]
            lum = (r + g + b) // 3
            if lum > 200:
                continue
            if glasses_color is not None and (x, y) in shades:
                outp[x, y] = glasses_color
            else:
                outp[x, y] = (0, 0, 0, 255 if lum < 48 else max(0, 255 - lum))
    return _fit_cutout(cut, size)


def backup_tray_legacy() -> None:
    """Copy live tray PNGs once. Never overwrite an existing backup."""
    TRAY_LEGACY.mkdir(parents=True, exist_ok=True)
    for name in TRAY_PNGS:
        src, dst = OUT / name, TRAY_LEGACY / name
        if src.exists() and not dst.exists():
            shutil.copy2(src, dst)


def write_ico(path: Path) -> None:
    sizes = [16, 24, 32, 48, 64, 128, 256]
    entries, blobs = [], []
    for s in sizes:
        buf = BytesIO()
        make_app_icon(s).save(buf, format="PNG")
        data = buf.getvalue()
        entries.append((s, len(data)))
        blobs.append(data)
    offset = 6 + 16 * len(sizes)
    header = struct.pack("<HHH", 0, 1, len(sizes))
    dire = body = b""
    for (s, sz), data in zip(entries, blobs):
        w = h = 0 if s >= 256 else s
        dire += struct.pack("<BBBBHHII", w, h, 0, 0, 1, 32, sz, offset)
        body += data
        offset += sz
    path.write_bytes(header + dire + body)


def write_icns() -> None:
    """Pure-Python .icns: PNG payloads for the modern retin@2x types."""
    types = [
        ("ic11", 32),   # 16x16@2x
        ("ic12", 64),   # 32x32@2x
        ("ic07", 128),  # 128x128
        ("ic13", 256),  # 128x128@2x
        ("ic08", 256),  # 256x256
        ("ic14", 512),  # 256x256@2x
        ("ic09", 512),  # 512x512
        ("ic10", 1024), # 512x512@2x
    ]
    body = b""
    for typ, s in types:
        buf = BytesIO()
        make_app_icon(s).save(buf, format="PNG")
        blob = buf.getvalue()
        body += typ.encode("ascii") + struct.pack(">I", len(blob) + 8) + blob
    (OUT / "icon.icns").write_bytes(b"icns" + struct.pack(">I", len(body) + 8) + body)


def _save_png(img: Image.Image, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path, format="PNG")


def write_tray_icons() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    backup_tray_legacy()
    TRAY_DIR.mkdir(parents=True, exist_ok=True)

    sets = {
        "badge": (
            draw_tray_badge(64, TRAY_STOPPED),
            draw_tray_badge(64, TRAY_RUNNING),
        ),
        "mark": (
            (
                Image.open(TRAY_LEGACY / "tray-icon-template.png")
                if (TRAY_LEGACY / "tray-icon-template.png").exists()
                else draw_satellite_mark(64, (0, 0, 0, 255))
            ),
            (
                Image.open(TRAY_LEGACY / "tray-icon-running.png")
                if (TRAY_LEGACY / "tray-icon-running.png").exists()
                else draw_satellite_mark(64, TRAY_RUNNING)
            ),
        ),
        "ghost": (
            draw_ghost(64, TRAY_STOPPED),
            draw_ghost(64, TRAY_GHOST_EYE_ON),
        ),
        "buddy": (
            draw_buddy(64, None),
            draw_buddy(64, TRAY_RUNNING),
        ),
    }
    for name, (off, on) in sets.items():
        _save_png(off, TRAY_DIR / f"{name}-off.png")
        _save_png(on, TRAY_DIR / f"{name}-on.png")

    # Live tray-icon* stay the badge set (current default).
    badge_off, badge_on = sets["badge"]
    _save_png(badge_off, OUT / "tray-icon.png")
    _save_png(badge_off, OUT / "tray-icon-template.png")
    _save_png(badge_on, OUT / "tray-icon-running.png")
    for size, white, black in [
        (32, "tray-icon-32.png", "tray-icon-template-32.png"),
        (22, "tray-icon-22.png", "tray-icon-template-22.png"),
    ]:
        _save_png(draw_tray_badge(size, TRAY_STOPPED), OUT / white)
        _save_png(draw_tray_badge(size, TRAY_STOPPED), OUT / black)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--tray", action="store_true", help="regenerate tray icons only"
    )
    args = parser.parse_args()

    OUT.mkdir(parents=True, exist_ok=True)
    write_tray_icons()
    if args.tray:
        print(f"Tray icons written → {OUT}")
        return

    make_app_icon(1024).save(OUT / "icon.png", format="PNG")
    for name, sz in [
        ("32x32.png", 32),
        ("128x128.png", 128),
        ("128x128@2x.png", 256),
        ("Square30x30Logo.png", 30),
        ("Square44x44Logo.png", 44),
        ("Square71x71Logo.png", 71),
        ("Square89x89Logo.png", 89),
        ("Square107x107Logo.png", 107),
        ("Square142x142Logo.png", 142),
        ("Square150x150Logo.png", 150),
        ("Square284x284Logo.png", 284),
        ("Square310x310Logo.png", 310),
        ("StoreLogo.png", 50),
    ]:
        make_app_icon(sz).save(OUT / name, format="PNG")

    write_ico(OUT / "icon.ico")
    write_icns()
    print(f"App + tray icons written → {OUT}")


if __name__ == "__main__":
    main()
