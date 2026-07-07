#!/usr/bin/env python3
"""Generate the application icon (CAN differential signal motif).

Outputs:
  assets/icon-256.png  preview / docs
  assets/icon.ico      multi-size Windows icon (16..256)
  assets/icon_64.rgba  raw RGBA bytes embedded as the window icon

Requires Pillow:  pip install pillow
"""

from pathlib import Path

from PIL import Image, ImageDraw

SIZE = 1024  # master canvas, downscaled for every output

BG_TOP = (13, 31, 51)  # deep navy
BG_BOTTOM = (23, 64, 105)  # blue
BORDER = (86, 156, 214, 90)
CAN_H = (74, 222, 128)  # green trace (CAN-H)
CAN_L = (56, 189, 248)  # sky-blue trace (CAN-L)
TRACE_W = 76
RADIUS = 224


def rounded_gradient(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    gradient = Image.new("RGBA", (size, size))
    for y in range(size):
        t = y / (size - 1)
        r = int(BG_TOP[0] + (BG_BOTTOM[0] - BG_TOP[0]) * t)
        g = int(BG_TOP[1] + (BG_BOTTOM[1] - BG_TOP[1]) * t)
        b = int(BG_TOP[2] + (BG_BOTTOM[2] - BG_TOP[2]) * t)
        for x in range(size):
            gradient.putpixel((x, y), (r, g, b, 255))
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle(
        [0, 0, size - 1, size - 1], radius=RADIUS, fill=255
    )
    img.paste(gradient, (0, 0), mask)
    # subtle inner border
    ImageDraw.Draw(img).rounded_rectangle(
        [10, 10, size - 11, size - 11], radius=RADIUS - 10, outline=BORDER, width=14
    )
    return img


def trace_points(mirror: bool) -> list[tuple[int, int]]:
    """CAN differential pair: near the midline when recessive, apart when
    dominant. The two traces are vertically offset so they never overlap."""
    y_rec = 512 - 62
    y_dom = 512 - 236
    pts = [
        (150, y_rec),
        (356, y_rec),
        (356, y_dom),
        (600, y_dom),
        (600, y_rec),
        (724, y_rec),
        (724, y_dom),
        (874, y_dom),
    ]
    if mirror:
        pts = [(x, 1024 - y) for (x, y) in pts]
    return pts


def draw_traces(img: Image.Image) -> None:
    draw = ImageDraw.Draw(img)
    for mirror, color in ((True, CAN_L), (False, CAN_H)):
        pts = trace_points(mirror)
        draw.line(pts, fill=color, width=TRACE_W, joint="curve")
        # round the line ends
        for x, y in (pts[0], pts[-1]):
            r = TRACE_W // 2 - 1
            draw.ellipse([x - r, y - r, x + r, y + r], fill=color)


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    assets = root / "assets"
    assets.mkdir(exist_ok=True)

    master = rounded_gradient(SIZE)
    draw_traces(master)

    png256 = master.resize((256, 256), Image.LANCZOS)
    png256.save(assets / "icon-256.png")

    ico_sizes = [16, 24, 32, 48, 64, 128, 256]
    master.save(
        assets / "icon.ico",
        format="ICO",
        sizes=[(s, s) for s in ico_sizes],
    )

    rgba64 = master.resize((64, 64), Image.LANCZOS)
    (assets / "icon_64.rgba").write_bytes(rgba64.tobytes())

    print(f"wrote {assets / 'icon-256.png'}")
    print(f"wrote {assets / 'icon.ico'} (sizes {ico_sizes})")
    print(f"wrote {assets / 'icon_64.rgba'} (64x64 raw RGBA)")


if __name__ == "__main__":
    main()
