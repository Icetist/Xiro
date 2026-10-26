"""Builds every icon from logo/logo.png: the README logo, the iPad home-screen and browser
icons, and the raw pixels Xiro.exe draws its window and tray icons from.

Usage: python scripts/make-icons.py   (needs Pillow)
"""
import base64
import io
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

ROOT = Path(__file__).resolve().parent.parent
TILE = (0x11, 0x11, 0x12, 255)


def mark(bold=0):
    logo = Image.open(ROOT / "logo" / "logo.png").convert("RGBA")
    alpha = logo.split()[3].point(lambda v: v if v >= 16 else 0)
    if bold:
        alpha = alpha.filter(ImageFilter.MaxFilter(bold))
    white = Image.new("RGBA", logo.size, (255, 255, 255, 0))
    white.putalpha(alpha)
    return white.crop(alpha.getbbox())


def tile(size, bold=0, fill=0.66, radius=0.225):
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    mask = Image.new("L", (size * 4, size * 4), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, size * 4 - 1, size * 4 - 1), radius=int(size * 4 * radius), fill=255)
    out.paste(Image.new("RGBA", (size, size), TILE), (0, 0), mask.resize((size, size), Image.LANCZOS))
    logo = mark(bold)
    width = int(size * fill)
    logo = logo.resize((width, round(logo.height * width / logo.width)), Image.LANCZOS)
    out.alpha_composite(logo, ((size - logo.width) // 2, (size - logo.height) // 2))
    return out


def png(image):
    buf = io.BytesIO()
    image.save(buf, "PNG", optimize=True)
    return buf.getvalue()


def main():
    assets = ROOT / ".github" / "assets"
    assets.mkdir(parents=True, exist_ok=True)
    tile(512).save(assets / "logo.png", optimize=True)
    tile(180, radius=0).save(ROOT / "web" / "apple-touch-icon.png", optimize=True)
    data = base64.b64encode(png(tile(128))).decode()
    svg = f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 128 128"><image width="128" height="128" href="data:image/png;base64,{data}"/></svg>\n'
    (ROOT / "web" / "icon.svg").write_text(svg, encoding="utf-8")
    # Straight-alpha BGRA, top row first: what CreateIconIndirect wants. The small one is
    # drawn bolder so the tray icon's lines survive at 16 pixels.
    out = ROOT / "crates" / "xiro-shell" / "assets"
    out.mkdir(parents=True, exist_ok=True)
    for size, bold in ((256, 0), (32, 15)):
        r, g, b, a = tile(size, bold=bold, fill=0.74 if size == 32 else 0.66).split()
        (out / f"logo-{size}.bgra").write_bytes(Image.merge("RGBA", (b, g, r, a)).tobytes())


if __name__ == "__main__":
    main()
