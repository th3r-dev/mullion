#!/usr/bin/env python3
"""Build the icon set from brand/mark.svg into crates/app/assets/icons/.

Rasterizes with headless Chrome (exact SVG rendering, no extra deps) at 1024 px,
then derives: favicon.ico (16/32/48), favicon.svg, apple-touch-icon.png (180),
icon-192.png, icon-512.png, icon-maskable-512.png, and manifest.webmanifest.

The raster set is the light variant (ink pane dark, blue pane accent) on a
transparent background; favicons do not follow the OS theme reliably, and the
mark reads on both light and dark tabs. The SVG favicon keeps the theme switch.
Maskable icons get the brand paper background and safe-zone padding.

For the Mac app (crates/host/macos/): Mullion.icns, the mark on a paper
rounded-square tile with a soft shadow (the macOS icon grid: 824 px tile in a
1024 px canvas), and MenuBarIcon.png, a 36 px template glyph for the menu bar
at 18 pt: a laptop with a tablet beside it, "this Mac and another screen".
The menu bar dims it while nothing is connected.

  python3 tools/make-icons.py             everything (needs Chrome)
  python3 tools/make-icons.py --menubar   only MenuBarIcon.png
"""
import base64, os, re, shutil, subprocess, sys, tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "brand" / "mark.svg"
OUT = ROOT / "crates" / "app" / "assets" / "icons"
MAC = ROOT / "crates" / "host" / "macos"
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
PAPER = (247, 247, 245, 255)


def render_svg(svg_text: str, size: int) -> Image.Image:
    html = f"""<!doctype html><meta charset=utf-8><style>html,body{{margin:0;background:transparent}}svg{{display:block;width:{size}px;height:{size}px}}</style>{svg_text}"""
    with tempfile.TemporaryDirectory() as td:
        page = Path(td) / "i.html"; page.write_text(html)
        png = Path(td) / "i.png"
        subprocess.run(
            [CHROME, "--headless=new", "--no-sandbox", "--disable-gpu", f"--user-data-dir={td}/prof", "--no-first-run",
             "--hide-scrollbars", "--default-background-color=00000000", f"--window-size={size},{size}",
             f"--screenshot={png}", page.as_uri()],
            check=False, capture_output=True, env={**os.environ, "HOME": td},
        )
        # Chrome sometimes exits non-zero in teardown after writing the file;
        # the screenshot is what counts.
        if not png.exists() or png.stat().st_size == 0:
            raise RuntimeError("Chrome did not write the screenshot")
        return Image.open(png).convert("RGBA")


def main():
    svg = SRC.read_text()
    # Light variant only for raster: drop the dark-scheme media block.
    svg_light = re.sub(r"@media \(prefers-color-scheme: dark\)\{[^}]*\}\s*\}", "", svg, flags=re.S)
    OUT.mkdir(parents=True, exist_ok=True)

    base = render_svg(svg_light, 1024)
    # Trim to the mark's own bounds then re-pad to a square with ~6% margin, so
    # the small sizes use the pixels.
    bbox = base.getbbox()
    mark = base.crop(bbox)
    side = max(mark.size)
    pad = int(side * 0.06)
    sq = Image.new("RGBA", (side + 2 * pad, side + 2 * pad), (0, 0, 0, 0))
    sq.paste(mark, (pad + (side - mark.width) // 2, pad + (side - mark.height) // 2), mark)

    def sized(n):
        return sq.resize((n, n), Image.LANCZOS)

    sized(180).save(OUT / "apple-touch-icon.png")
    sized(192).save(OUT / "icon-192.png")
    sized(512).save(OUT / "icon-512.png")
    # Maskable: paper background, mark inside the 80% safe zone.
    m = Image.new("RGBA", (512, 512), PAPER)
    inner = sized(int(512 * 0.72))
    m.paste(inner, ((512 - inner.width) // 2, (512 - inner.height) // 2), inner)
    m.save(OUT / "icon-maskable-512.png")
    # ICO with three sizes.
    sized(48).save(OUT / "favicon.ico", sizes=[(16, 16), (32, 32), (48, 48)])
    shutil.copy(SRC, OUT / "favicon.svg")

    (OUT / "manifest.webmanifest").write_text(
        '{\n'
        '  "name": "Mullion",\n'
        '  "short_name": "Mullion",\n'
        '  "description": "One desk, every screen.",\n'
        '  "start_url": "/receiver",\n'
        '  "display": "fullscreen",\n'
        '  "orientation": "landscape",\n'
        '  "background_color": "#0F1113",\n'
        '  "theme_color": "#111418",\n'
        '  "icons": [\n'
        '    { "src": "/icons/icon-192.png", "sizes": "192x192", "type": "image/png" },\n'
        '    { "src": "/icons/icon-512.png", "sizes": "512x512", "type": "image/png" },\n'
        '    { "src": "/icons/icon-maskable-512.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable" }\n'
        '  ]\n'
        '}\n'
    )
    make_mac(sq)
    for f in sorted(list(OUT.iterdir()) + list(MAC.glob("*.icns")) + list(MAC.glob("*.png"))):
        print(f"{f.name:28s} {f.stat().st_size:>7} B")


def make_mac(sq: Image.Image):
    MAC.mkdir(parents=True, exist_ok=True)
    # App icon: paper tile on the macOS grid, soft shadow, mark at ~62%.
    S = 1024
    tile, r = 824, 185
    x0 = (S - tile) // 2
    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((x0, x0 + 12, x0 + tile, x0 + tile + 12), r, fill=(0, 0, 0, 90))
    icon.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(18)))
    ImageDraw.Draw(icon).rounded_rectangle((x0, x0, x0 + tile, x0 + tile), r, fill=PAPER)
    m = sq.resize((640, 640), Image.LANCZOS)
    icon.alpha_composite(m, ((S - 640) // 2, (S - 640) // 2))
    with tempfile.TemporaryDirectory() as td:
        iconset = Path(td) / "Mullion.iconset"
        iconset.mkdir()
        for n in (16, 32, 128, 256, 512):
            icon.resize((n, n), Image.LANCZOS).save(iconset / f"icon_{n}x{n}.png")
            icon.resize((2 * n, 2 * n), Image.LANCZOS).save(iconset / f"icon_{n}x{n}@2x.png")
        subprocess.run(["iconutil", "-c", "icns", str(iconset), "-o", str(MAC / "Mullion.icns")], check=True)
    icon.resize((256, 256), Image.LANCZOS).save(MAC / "AppIcon-256.png")

    menubar_glyph()


def menubar_glyph():
    """Laptop + tablet, a template image (only alpha counts), drawn 8x then
    reduced to 36 px (18 pt @2x). Units below are 36 px grid units."""
    k = 8
    g = Image.new("RGBA", (36 * k, 36 * k), (0, 0, 0, 0))
    d = ImageDraw.Draw(g)
    ink = (0, 0, 0, 255)
    box = lambda *v: [x * k for x in v]
    d.rounded_rectangle(box(3, 9, 22, 23), 2 * k, outline=ink, width=int(2.4 * k))     # laptop screen
    d.rounded_rectangle(box(0.5, 24, 24.5, 27), 1.2 * k, fill=ink)                     # laptop base
    d.rounded_rectangle(box(25, 12, 34.5, 27), 2 * k, outline=ink, width=int(2.4 * k))  # tablet
    d.ellipse(box(28.9, 24, 30.6, 25.7), fill=ink)                                       # home button
    g.resize((36, 36), Image.LANCZOS).save(MAC / "MenuBarIcon.png")


if __name__ == "__main__":
    if sys.argv[1:] == ["--menubar"]:
        sys.exit(menubar_glyph())
    sys.exit(main())
