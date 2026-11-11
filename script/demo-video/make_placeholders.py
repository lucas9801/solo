#!/usr/bin/env python3
"""Writes placeholder screenshots for the demo video, to try the storyboard
before the real screenshots exist.

    python script/demo-video/make_placeholders.py <folder> [--size 1600x1000]

Each is a grey image with its name, a coordinate grid every 100 px (to read
off the coordinates scenes.json uses) and the rough places of Solo's title
bar, terminal tabs and input box. Not for the README: the real screenshots
go in docs/media/video/shots/.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from PIL import Image, ImageDraw

sys.path.insert(0, str(Path(__file__).resolve().parent))
from render import Fonts, find_font  # noqa: E402

SHOTS = [
    "v01-overview.png",
    "v02-agent-working.png",
    "v03-other-waiting.png",
    "v04-replied.png",
    "v05-path-in-output.png",
    "v06-editor-open.png",
    "v07-mermaid-output.png",
    "v08-mermaid-rendered.png",
    "v09-turn-done.png",
    "v10-review.png",
    "v11-hunk-rejected.png",
    "v12-reopened.png",
]
# Where the editor is open (the right half), as in the storyboard.
EDITOR_OPEN = {"v06-editor-open.png", "v08-mermaid-rendered.png", "v10-review.png", "v11-hunk-rejected.png"}


def placeholder(name: str, size, fonts: Fonts) -> Image.Image:
    w, h = size
    image = Image.new("RGB", size, (58, 60, 66))
    draw = ImageDraw.Draw(image)
    small = fonts.get(14, "Regular")
    # Title bar with workspace tabs, terminal tabs, the input box.
    draw.rectangle((0, 0, w, 35), fill=(36, 38, 43))
    for i in range(3):
        x = 40 + i * 250
        draw.rounded_rectangle((x, 4, x + 240, 31), radius=6, fill=(70, 74, 82) if i else (96, 102, 114))
        draw.text((x + 12, 17), f"workspace {i + 1}", font=small, fill=(220, 220, 220), anchor="lm")
    draw.rectangle((0, 36, w, 74), fill=(46, 48, 54))
    draw.rounded_rectangle((8, 40, 308, 70), radius=5, fill=(80, 86, 98))
    draw.text((20, 55), "terminal tab", font=small, fill=(230, 230, 230), anchor="lm")
    split = w // 2 + 20 if name in EDITOR_OPEN else w
    draw.rounded_rectangle((20, h - 100, min(split, w) - 20, h - 40), radius=6, fill=(48, 50, 56))
    draw.text((32, h - 70), "input", font=small, fill=(180, 180, 180), anchor="lm")
    if split < w:
        draw.rectangle((split, 36, w, h), fill=(40, 44, 52))
        draw.line((split, 36, split, h), fill=(110, 110, 120), width=2)
        draw.text((split + 20, 90), "editor", font=small, fill=(200, 200, 200), anchor="lm")
    # The grid.
    for x in range(0, w, 100):
        draw.line((x, 0, x, h), fill=(90, 92, 100), width=1)
        draw.text((x + 3, h - 18), str(x), font=small, fill=(150, 150, 160))
    for y in range(0, h, 100):
        draw.line((0, y, w, y), fill=(90, 92, 100), width=1)
        draw.text((3, y + 3), str(y), font=small, fill=(150, 150, 160))
    # The name, large.
    label = name.split("-")[0]
    draw.text((w / 2, h / 2 - 30), label, font=fonts.get(160, "Bold"), fill=(235, 235, 240), anchor="mm")
    draw.text((w / 2, h / 2 + 80), name, font=fonts.get(32, "Regular"), fill=(200, 200, 210), anchor="mm")
    draw.text((w / 2, h / 2 + 130), "占位图 · placeholder", font=fonts.get(28, "Regular"), fill=(170, 170, 180), anchor="mm")
    return image


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("folder")
    parser.add_argument("--size", default="1600x1000")
    parser.add_argument("--font")
    args = parser.parse_args()
    size = tuple(int(v) for v in args.size.lower().split("x"))
    fonts = Fonts(find_font(args.font))
    folder = Path(args.folder)
    folder.mkdir(parents=True, exist_ok=True)
    for name in SHOTS:
        placeholder(name, size, fonts).save(folder / name)
    print(f"wrote {len(SHOTS)} placeholders to {folder}")


if __name__ == "__main__":
    main()
