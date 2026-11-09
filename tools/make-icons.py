#!/usr/bin/env python3
"""Build the icon set from brand/mark.svg into crates/app/assets/icons/.

Rasterizes with headless Chrome (exact SVG rendering, no extra deps) at 1024 px,
then derives: favicon.ico (16/32/48), favicon.svg, apple-touch-icon.png (180),
icon-192.png, icon-512.png, icon-maskable-512.png, and manifest.webmanifest.

The raster set is the light variant (ink pane dark, blue pane accent) on a
transparent background; favicons do not follow the OS theme reliably, and the
mark reads on both light and dark tabs. The SVG favicon keeps the theme switch.
Maskable icons get the brand paper background and safe-zone padding.
"""
import base64, os, re, shutil, subprocess, sys, tempfile
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "brand" / "mark.svg"
OUT = ROOT / "crates" / "app" / "assets" / "icons"
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
            check=True, capture_output=True, env={**os.environ, "HOME": td},
        )
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
    for f in sorted(OUT.iterdir()):
        print(f"{f.name:28s} {f.stat().st_size:>7} B")


if __name__ == "__main__":
    sys.exit(main())
