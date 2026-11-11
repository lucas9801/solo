#!/usr/bin/env python3
"""Renders Solo's README demo video from screenshots and a storyboard.

    python script/demo-video/render.py --lang zh [--out target/demo]
    python script/demo-video/render.py --lang zh --preview
    python script/demo-video/render.py --lang zh --frames-only [--anim webp]

Reads docs/media/video/scenes.json (the storyboard) and the screenshots in
docs/media/video/shots/, and draws each frame with Pillow: the camera's crop
of the screenshot, highlight boxes, the mouse pointer and its clicks, the
caption, and cross-fades between shots and scenes. Then:

- with ffmpeg (on PATH, given with --ffmpeg, or the one the imageio-ffmpeg
  package brings) it encodes demo-<lang>.mp4 (H.264, yuv420p, +faststart, no
  audio, under 10 MB) from frames in a temporary folder, deleted afterwards;
- with --frames-only it keeps the numbered PNG frames in
  <out>/frames-<lang>/ and prints the ffmpeg command that makes the MP4;
- with --anim webp|gif|png it also writes a smaller animated preview
  (demo-<lang>.webp, .gif, or an animated .png), Pillow only;
- with --preview it only draws each scene's middle frame, into one overview
  image, to check coordinates and captions quickly.

It always writes the poster, demo-<lang>-poster.png: the middle frame of the
first scene with a screenshot. The same inputs give the same frames.

Needs Python 3.9+ and Pillow. The font is Noto Sans SC (SIL OFL 1.1), looked
up in --font, then script/demo-video/fonts/, then the Windows fonts folder;
see README.md next to this script.
"""

from __future__ import annotations

import argparse
import json
import math
import os
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

try:
    from PIL import Image, ImageDraw, ImageFont
except ImportError:  # pragma: no cover
    sys.exit("This script needs Pillow: python -m pip install Pillow")

SCRIPT_DIR = Path(__file__).resolve().parent
REPO = SCRIPT_DIR.parent.parent
DEFAULT_SCENES = REPO / "docs" / "media" / "video" / "scenes.json"

# Look of the video (G18).
CROSSFADE = 0.4  # seconds, between scenes and between a scene's shots
HIGHLIGHT_COLOR = (0x59, 0xC2, 0xFF)
HIGHLIGHT_WIDTH = 3
HIGHLIGHT_RADIUS = 8
HIGHLIGHT_DIM = 0.35  # how much darker outside the box
HIGHLIGHT_FADE = 0.3
CLICK_DURATION = 0.5
CURSOR_FADE = 0.25
CAPTION_SIZE = 28
CAPTION_MARGIN = 36  # from the bottom
CAPTION_BAR_ALPHA = 0.62
CARD_BACKGROUND = (0x0B, 0x0E, 0x14)
CARD_TITLE_SIZE = 76
CARD_SUBTITLE_SIZE = 36
CARD_SUBTITLE_COLOR = (0xB3, 0xB1, 0xAD)
VIDEO_FADE_IN = 0.5
VIDEO_FADE_OUT = 0.6
MAX_MP4_BYTES = 10 * 1024 * 1024

FONT_FILES = ["NotoSansSC-VF.ttf", "NotoSansSC-Regular.otf", "NotoSansSC-Regular.ttf"]


class StoryboardError(Exception):
    pass


# ---------------------------------------------------------------- helpers


def ease(p: float) -> float:
    """Cubic ease-in-out of p in [0, 1]."""
    p = min(max(p, 0.0), 1.0)
    return 4 * p * p * p if p < 0.5 else 1 - (-2 * p + 2) ** 3 / 2


def clamp01(x: float) -> float:
    return min(max(x, 0.0), 1.0)


def lerp(a, b, p):
    return a + (b - a) * p


def text_for(value, lang: str) -> str:
    """A caption or card text: a string, or {"zh": ..., "en": ...}."""
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        return value.get(lang) or value.get("zh") or next(iter(value.values()), "")
    raise StoryboardError(f"text must be a string or {{lang: text}}, not {value!r}")


def keyframe_value(keyframes, t, key):
    """`key` of the keyframes (sorted by "t") at time t, eased in between."""
    if t <= keyframes[0]["t"]:
        return keyframes[0][key]
    for a, b in zip(keyframes, keyframes[1:]):
        if t <= b["t"]:
            span = b["t"] - a["t"]
            p = ease((t - a["t"]) / span) if span > 0 else 1.0
            return [lerp(x, y, p) for x, y in zip(a[key], b[key])]
    return keyframes[-1][key]


# ---------------------------------------------------------------- fonts


def find_font(explicit: str | None) -> Path:
    candidates = []
    if explicit:
        candidates.append(Path(explicit))
    if os.environ.get("SOLO_DEMO_FONT"):
        candidates.append(Path(os.environ["SOLO_DEMO_FONT"]))
    for name in FONT_FILES:
        candidates.append(SCRIPT_DIR / "fonts" / name)
    windir = os.environ.get("WINDIR", r"C:\Windows")
    for name in FONT_FILES:
        candidates.append(Path(windir) / "Fonts" / name)
    for path in candidates:
        if path.is_file():
            return path
    raise StoryboardError(
        "Noto Sans SC not found. Put NotoSansSC-VF.ttf (with its OFL.txt) in "
        f"{SCRIPT_DIR / 'fonts'}, or pass --font <path>. It's at "
        "https://github.com/notofonts/noto-cjk (Sans/Variable/TTF)."
    )


class Fonts:
    def __init__(self, path: Path):
        self.path = path
        self._cache = {}

    def get(self, size: int, weight: str = "Regular") -> ImageFont.FreeTypeFont:
        key = (size, weight)
        if key not in self._cache:
            font = ImageFont.truetype(str(self.path), size)
            try:
                # A variable font's default instance can be Thin.
                font.set_variation_by_name(weight)
            except (OSError, ValueError, AttributeError):
                pass  # not a variable font
            self._cache[key] = font
        return self._cache[key]


# ---------------------------------------------------------------- storyboard


@dataclass
class Item:
    """One screenshot or card shown in a scene, until `until` (scene time)."""

    file: str | None
    card: dict | None
    until: float | None


@dataclass
class Scene:
    index: int  # 1-based, as in the storyboard
    duration: float
    items: list
    caption: object
    camera: list | None
    highlights: list
    cursor: list | None


def load_storyboard(path: Path):
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise StoryboardError(f"{path}: {error}") from None
    size = tuple(data.get("size", [1280, 800]))
    fps = int(data.get("fps", 30))
    scenes = []
    for number, raw in enumerate(data.get("scenes", []), start=1):
        where = f"scene {number}"
        if "duration" not in raw:
            raise StoryboardError(f"{where}: no duration")
        items = []
        if "card" in raw:
            items.append(Item(None, raw["card"], None))
        elif "shot" in raw:
            items.append(Item(raw["shot"], None, None))
        elif "shots" in raw:
            for entry in raw["shots"]:
                if "card" in entry:
                    items.append(Item(None, entry["card"], entry.get("until")))
                elif "file" in entry:
                    items.append(Item(entry["file"], None, entry.get("until")))
                else:
                    raise StoryboardError(f"{where}: each of shots needs file or card")
            for item in items[:-1]:
                if item.until is None:
                    raise StoryboardError(f"{where}: every shot but the last needs until")
        else:
            raise StoryboardError(f"{where}: needs card, shot or shots")
        camera = raw.get("camera")
        if camera:
            camera = sorted(camera, key=lambda k: k["t"])
            for k in camera:
                if len(k.get("rect", [])) != 4 or k["rect"][2] <= 0 or k["rect"][3] <= 0:
                    raise StoryboardError(f"{where}: camera rect must be [x, y, width, height]")
        cursor = raw.get("cursor")
        if cursor:
            cursor = sorted(cursor, key=lambda k: k["t"])
        scenes.append(
            Scene(
                index=number,
                duration=float(raw["duration"]),
                items=items,
                caption=raw.get("caption"),
                camera=camera,
                highlights=raw.get("highlight", []),
                cursor=cursor,
            )
        )
    if not scenes:
        raise StoryboardError(f"{path}: no scenes")
    return size, fps, scenes


# ---------------------------------------------------------------- drawing


class Renderer:
    def __init__(self, size, fps, scenes, shots_dir: Path, fonts: Fonts, lang: str):
        self.w, self.h = size
        self.fps = fps
        self.scenes = scenes
        self.shots_dir = shots_dir
        self.fonts = fonts
        self.lang = lang
        self._shots = {}
        self._cards = {}
        self._captions = {}
        self._cursor = None
        starts, t = [], 0.0
        for scene in scenes:
            starts.append(t)
            t += scene.duration
        self.starts = starts
        self.total = t

    # --- inputs

    def check_shots(self):
        missing = sorted(
            {
                item.file
                for scene in self.scenes
                for item in scene.items
                if item.file and not (self.shots_dir / item.file).is_file()
            }
        )
        if missing:
            raise StoryboardError(
                f"missing screenshots in {self.shots_dir}: " + ", ".join(missing)
            )

    def shot(self, name: str) -> Image.Image:
        if name not in self._shots:
            image = Image.open(self.shots_dir / name)
            image.load()
            self._shots[name] = image.convert("RGB")
        return self._shots[name]

    # --- pieces

    def card(self, card: dict) -> Image.Image:
        key = json.dumps(card, sort_keys=True, ensure_ascii=False)
        if key in self._cards:
            return self._cards[key]
        image = Image.new("RGB", (self.w, self.h), CARD_BACKGROUND)
        draw = ImageDraw.Draw(image)
        title = text_for(card.get("title"), self.lang)
        subtitle = text_for(card.get("subtitle"), self.lang)
        title_font = self.fonts.get(CARD_TITLE_SIZE, "Bold")
        sub_font = self.fonts.get(CARD_SUBTITLE_SIZE, "Regular")
        gap = 28
        title_h = CARD_TITLE_SIZE if title else 0
        sub_lines = self.wrap(subtitle, sub_font, self.w - 200) if subtitle else []
        sub_h = len(sub_lines) * int(CARD_SUBTITLE_SIZE * 1.45)
        y = (self.h - title_h - (gap if title and sub_lines else 0) - sub_h) // 2
        if title:
            draw.text((self.w / 2, y + title_h / 2), title, font=title_font, fill="white", anchor="mm")
            y += title_h + gap
        for line in sub_lines:
            line_h = int(CARD_SUBTITLE_SIZE * 1.45)
            draw.text((self.w / 2, y + line_h / 2), line, font=sub_font, fill=CARD_SUBTITLE_COLOR, anchor="mm")
            y += line_h
        self._cards[key] = image
        return image

    @staticmethod
    def wrap(text: str, font, width: int):
        """Lines of `text` no wider than `width`: by words, or by characters
        for CJK text without spaces."""
        lines = []
        for paragraph in text.split("\n"):
            tokens = paragraph.split(" ") if " " in paragraph else list(paragraph)
            joiner = " " if " " in paragraph else ""
            line = ""
            for token in tokens:
                candidate = f"{line}{joiner}{token}" if line else token
                if font.getlength(candidate) <= width or not line:
                    line = candidate
                else:
                    lines.append(line)
                    line = token
            lines.append(line)
        return lines

    def caption(self, text: str) -> Image.Image:
        """The caption bar, RGBA, as wide as the canvas."""
        if text in self._captions:
            return self._captions[text]
        font = self.fonts.get(CAPTION_SIZE, "Medium")
        lines = self.wrap(text, font, self.w - 160)
        line_h = int(CAPTION_SIZE * 1.5)
        pad_x, pad_y = 22, 12
        text_w = max(font.getlength(line) for line in lines)
        bar_w, bar_h = int(text_w + 2 * pad_x), len(lines) * line_h + 2 * pad_y
        layer = Image.new("RGBA", (self.w, bar_h), (0, 0, 0, 0))
        draw = ImageDraw.Draw(layer)
        x0 = (self.w - bar_w) // 2
        draw.rounded_rectangle(
            (x0, 0, x0 + bar_w, bar_h), radius=10, fill=(0, 0, 0, int(255 * CAPTION_BAR_ALPHA))
        )
        for i, line in enumerate(lines):
            draw.text(
                (self.w / 2, pad_y + i * line_h + line_h / 2), line, font=font, fill="white", anchor="mm"
            )
        self._captions[text] = layer
        return layer

    def cursor_sprite(self):
        """The mouse pointer, drawn 4x and scaled down; with its hot spot."""
        if self._cursor is None:
            s = 4
            points = [(0, 0), (0, 17), (4.2, 13.2), (7, 19.5), (9.6, 18.4), (6.9, 12.3), (12.4, 12.3)]
            pad = 3
            size = (int((14 + 2 * pad) * s), int((22 + 2 * pad) * s))
            big = Image.new("RGBA", size, (0, 0, 0, 0))
            draw = ImageDraw.Draw(big)
            scaled = [((x + pad) * s, (y + pad) * s) for x, y in points]
            # A soft shadow, then the black outline and the white body.
            shadow = [(x + 1.2 * s, y + 1.6 * s) for x, y in scaled]
            draw.polygon(shadow, fill=(0, 0, 0, 70))
            draw.polygon(scaled, fill=(255, 255, 255, 255), outline=(0, 0, 0, 255), width=int(1.4 * s))
            sprite = big.resize((size[0] // s * 3 // 2, size[1] // s * 3 // 2), Image.LANCZOS)
            self._cursor = (sprite, (pad * 3 // 2, pad * 3 // 2))
        return self._cursor

    # --- a scene at a time

    def camera_rect(self, scene: Scene, t: float, image: Image.Image):
        """The part of the screenshot on screen: [x, y, w, h], widened to the
        video's aspect ratio and kept inside the screenshot."""
        if scene.camera:
            x, y, w, h = keyframe_value(scene.camera, t, "rect")
        else:
            x, y, w, h = 0, 0, image.width, image.height
        aspect = self.w / self.h
        cx, cy = x + w / 2, y + h / 2
        if w / h > aspect:
            h = w / aspect
        else:
            w = h * aspect
        w, h = min(w, image.width), min(h, image.height)
        if w / h > aspect + 1e-6:  # the screenshot is narrower than the video
            w = h * aspect
        elif w / h < aspect - 1e-6:
            h = w / aspect
        x = min(max(cx - w / 2, 0), image.width - w)
        y = min(max(cy - h / 2, 0), image.height - h)
        return x, y, w, h

    def item_at(self, scene: Scene, t: float):
        """The scene's items blended at time t: [(item, weight)]."""
        items = scene.items
        for i, item in enumerate(items[:-1]):
            if item.until is not None and t < item.until + CROSSFADE / 2:
                if t > item.until - CROSSFADE / 2:
                    p = clamp01((t - (item.until - CROSSFADE / 2)) / CROSSFADE)
                    return [(item, 1 - p), (items[i + 1], p)]
                return [(item, 1.0)]
        return [(items[-1], 1.0)]

    def draw_item(self, scene: Scene, item: Item, t: float):
        if item.card is not None:
            return self.card(item.card), None
        image = self.shot(item.file)
        rect = self.camera_rect(scene, t, image)
        x, y, w, h = rect
        frame = image.resize((self.w, self.h), Image.LANCZOS, box=(x, y, x + w, y + h))
        return frame, rect

    def render_scene(self, scene: Scene, t: float) -> Image.Image:
        t = min(max(t, 0.0), scene.duration)
        blend = self.item_at(scene, t)
        frame, rect = self.draw_item(scene, blend[0][0], t)
        if len(blend) == 2:
            other, other_rect = self.draw_item(scene, blend[1][0], t)
            frame = Image.blend(frame, other, blend[1][1])
            rect = rect or other_rect
        if rect is not None:
            self.draw_highlights(frame, scene, t, rect)
            self.draw_cursor(frame, scene, t, rect)
        caption = text_for(scene.caption, self.lang)
        if caption:
            layer = self.caption(caption)
            frame.paste(layer, (0, self.h - CAPTION_MARGIN - layer.height), layer)
        return frame

    def to_screen(self, rect, point):
        x, y, w, h = rect
        return ((point[0] - x) * self.w / w, (point[1] - y) * self.h / h)

    def draw_highlights(self, frame: Image.Image, scene: Scene, t: float, rect):
        boxes = []
        for highlight in scene.highlights:
            start, end = highlight["from"], highlight["to"]
            if not start <= t <= end:
                continue
            fade = min(HIGHLIGHT_FADE, (end - start) / 2)
            alpha = clamp01(min(t - start, end - t) / fade) if fade > 0 else 1.0
            hx, hy, hw, hh = highlight["rect"]
            x0, y0 = self.to_screen(rect, (hx, hy))
            x1, y1 = self.to_screen(rect, (hx + hw, hy + hh))
            boxes.append(((x0, y0, x1, y1), alpha))
        if not boxes:
            return
        alpha = max(a for _, a in boxes)
        # Darken everything but the boxes.
        mask = Image.new("L", frame.size, int(255 * HIGHLIGHT_DIM * alpha))
        mask_draw = ImageDraw.Draw(mask)
        for box, _ in boxes:
            mask_draw.rounded_rectangle(box, radius=HIGHLIGHT_RADIUS, fill=0)
        frame.paste((0, 0, 0), (0, 0), mask)
        # The outlines, drawn 2x for smoother corners.
        s = 2
        for (x0, y0, x1, y1), box_alpha in boxes:
            pad = HIGHLIGHT_WIDTH + 2
            bx0, by0 = int(math.floor(x0)) - pad, int(math.floor(y0)) - pad
            bx1, by1 = int(math.ceil(x1)) + pad, int(math.ceil(y1)) + pad
            layer = Image.new("RGBA", ((bx1 - bx0) * s, (by1 - by0) * s), (0, 0, 0, 0))
            ImageDraw.Draw(layer).rounded_rectangle(
                ((x0 - bx0) * s, (y0 - by0) * s, (x1 - bx0) * s, (y1 - by0) * s),
                radius=HIGHLIGHT_RADIUS * s,
                outline=HIGHLIGHT_COLOR + (int(255 * box_alpha),),
                width=HIGHLIGHT_WIDTH * s,
            )
            layer = layer.resize((bx1 - bx0, by1 - by0), Image.LANCZOS)
            frame.paste(layer, (bx0, by0), layer)

    def draw_cursor(self, frame: Image.Image, scene: Scene, t: float, rect):
        keys = scene.cursor
        if not keys or t < keys[0]["t"] - CURSOR_FADE:
            return
        alpha = clamp01((t - (keys[0]["t"] - CURSOR_FADE)) / CURSOR_FADE)
        # Clicks first, under the pointer.
        for key in keys:
            if key.get("click") and key["t"] <= t <= key["t"] + CLICK_DURATION:
                p = (t - key["t"]) / CLICK_DURATION
                cx, cy = self.to_screen(rect, key["at"])
                radius = 8 + 30 * ease(p)
                s = 2
                size = int((radius + 6) * 2 * s)
                ring = Image.new("RGBA", (size, size), (0, 0, 0, 0))
                ImageDraw.Draw(ring).ellipse(
                    (size / 2 - radius * s, size / 2 - radius * s, size / 2 + radius * s, size / 2 + radius * s),
                    outline=HIGHLIGHT_COLOR + (int(255 * (1 - p) * alpha),),
                    width=3 * s,
                )
                ring = ring.resize((size // s, size // s), Image.LANCZOS)
                frame.paste(ring, (int(cx - ring.width / 2), int(cy - ring.height / 2)), ring)
        sprite, (hx, hy) = self.cursor_sprite()
        x, y = self.to_screen(rect, keyframe_value(keys, t, "at"))
        # Pressed: a touch smaller for a moment.
        pressed = any(k.get("click") and 0 <= t - k["t"] <= 0.12 for k in keys)
        if pressed:
            sprite = sprite.resize((int(sprite.width * 0.9), int(sprite.height * 0.9)), Image.LANCZOS)
            hx, hy = int(hx * 0.9), int(hy * 0.9)
        if alpha < 1:
            sprite = sprite.copy()
            sprite.putalpha(sprite.getchannel("A").point(lambda v: int(v * alpha)))
        frame.paste(sprite, (int(round(x)) - hx, int(round(y)) - hy), sprite)

    # --- the whole video

    def frame_count(self) -> int:
        return int(round(self.total * self.fps))

    def frame(self, n: int) -> Image.Image:
        t = n / self.fps
        i = 0
        while i + 1 < len(self.scenes) and t >= self.starts[i + 1]:
            i += 1
        local = t - self.starts[i]
        frame = self.render_scene(self.scenes[i], local)
        # Cross-fade with the scene before or after, around the boundary.
        neighbour = None
        if i > 0 and local < CROSSFADE / 2:
            p = 0.5 - local / CROSSFADE  # weight of the scene before
            neighbour = (self.scenes[i - 1], self.scenes[i - 1].duration, p)
        elif i + 1 < len(self.scenes) and self.scenes[i].duration - local < CROSSFADE / 2:
            p = 0.5 - (self.scenes[i].duration - local) / CROSSFADE  # weight of the next
            neighbour = (self.scenes[i + 1], 0.0, p)
        if neighbour and neighbour[2] > 0:
            other = self.render_scene(neighbour[0], neighbour[1])
            frame = Image.blend(frame, other, neighbour[2])
        fade = min(clamp01(t / VIDEO_FADE_IN), clamp01((self.total - t) / VIDEO_FADE_OUT))
        if fade < 1:
            frame = Image.blend(Image.new("RGB", frame.size, (0, 0, 0)), frame, fade)
        return frame


# ---------------------------------------------------------------- outputs


def find_ffmpeg(explicit: str | None):
    if explicit:
        return explicit if Path(explicit).is_file() else None
    found = shutil.which("ffmpeg")
    if found:
        return found
    try:
        import imageio_ffmpeg  # type: ignore

        return imageio_ffmpeg.get_ffmpeg_exe()
    except Exception:
        return None


FFMPEG_HELP = """ffmpeg was not found. Install it, for example:
  winget install Gyan.FFmpeg
  (or: python -m pip install imageio-ffmpeg, which brings its own ffmpeg)
or pass --ffmpeg <path to ffmpeg.exe>. Without ffmpeg, --frames-only writes
the frames and prints the command that encodes them, and --anim webp writes
an animated WebP preview."""


def ffmpeg_command(ffmpeg: str, frames: Path, fps: int, crf: int, out: Path):
    return [
        ffmpeg, "-y", "-loglevel", "error",
        "-framerate", str(fps), "-i", str(frames / "%05d.png"),
        "-c:v", "libx264", "-preset", "slow", "-crf", str(crf),
        "-pix_fmt", "yuv420p", "-movflags", "+faststart", "-an",
        str(out),
    ]


def encode_mp4(ffmpeg: str, frames: Path, fps: int, out: Path):
    for crf in (20, 23, 26, 29, 32, 35):
        subprocess.run(ffmpeg_command(ffmpeg, frames, fps, crf, out), check=True)
        size = out.stat().st_size
        print(f"{out.name}: CRF {crf}, {size / 1024 / 1024:.1f} MB")
        if size <= MAX_MP4_BYTES:
            return
    print(f"warning: {out.name} is still over 10 MB", file=sys.stderr)


def save_animation(frames, path: Path, fps: int):
    duration = int(round(1000 / fps))
    kind = path.suffix.lower()
    first, rest = frames[0], frames[1:]
    if kind == ".webp":
        first.save(path, save_all=True, append_images=rest, duration=duration, loop=0, quality=72, method=4)
    elif kind == ".gif":
        palette = [f.convert("P", palette=Image.ADAPTIVE, colors=128) for f in frames]
        palette[0].save(path, save_all=True, append_images=palette[1:], duration=duration, loop=0, optimize=False)
    elif kind == ".png":
        first.save(path, save_all=True, append_images=rest, duration=duration, loop=0)
    else:
        raise StoryboardError(f"unknown animation format {kind}")
    print(f"wrote {path} ({len(frames)} frames, {path.stat().st_size / 1024 / 1024:.1f} MB)")


def poster_scene(renderer: Renderer):
    for scene in renderer.scenes:
        if any(item.file for item in scene.items):
            return scene
    return renderer.scenes[0]


def preview(renderer: Renderer, out: Path):
    columns = 3
    tile_w, tile_h = renderer.w // 2, renderer.h // 2
    label_h = 34
    rows = math.ceil(len(renderer.scenes) / columns)
    sheet = Image.new("RGB", (columns * tile_w, rows * (tile_h + label_h)), (24, 24, 28))
    draw = ImageDraw.Draw(sheet)
    font = renderer.fonts.get(20, "Regular")
    for n, scene in enumerate(renderer.scenes):
        middle = scene.duration / 2
        tile = renderer.render_scene(scene, middle).resize((tile_w, tile_h), Image.LANCZOS)
        x, y = (n % columns) * tile_w, (n // columns) * (tile_h + label_h)
        sheet.paste(tile, (x, y + label_h))
        names = " → ".join(i.file.split("-")[0] if i.file else "card" for i in scene.items)
        start = renderer.starts[n]
        draw.text(
            (x + 10, y + label_h / 2),
            f"#{scene.index}  {start:.1f}s +{scene.duration:g}s  t={middle:g}  {names}",
            font=font, fill="white", anchor="lm",
        )
    sheet.save(out)
    print(f"wrote {out}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--lang", default="zh", help="caption language (zh or en)")
    parser.add_argument("--out", default="target/demo", help="output folder (default target/demo)")
    parser.add_argument("--scenes", default=str(DEFAULT_SCENES), help="storyboard JSON")
    parser.add_argument("--shots", help="screenshot folder (default: shots/ next to the storyboard)")
    parser.add_argument("--font", help="Noto Sans SC font file")
    parser.add_argument("--only", help="render only these scenes, e.g. 1,5,10 (a sample)")
    parser.add_argument("--preview", action="store_true", help="only the overview of each scene's middle frame")
    parser.add_argument("--frames-only", action="store_true", help="keep the PNG frames instead of encoding an MP4")
    parser.add_argument("--anim", choices=["webp", "gif", "png"], help="also write an animated preview with Pillow")
    parser.add_argument("--anim-width", type=int, default=640, help="width of the animated preview (default 640)")
    parser.add_argument("--anim-fps", type=int, default=10, help="frame rate of the animated preview (default 10)")
    parser.add_argument("--ffmpeg", help="path of ffmpeg.exe")
    args = parser.parse_args(argv)

    try:
        scenes_path = Path(args.scenes)
        size, fps, scenes = load_storyboard(scenes_path)
        if args.only:
            wanted = {int(x) for x in args.only.split(",") if x.strip()}
            scenes = [s for s in scenes if s.index in wanted]
            if not scenes:
                raise StoryboardError(f"no scenes {args.only}")
        shots = Path(args.shots) if args.shots else scenes_path.parent / "shots"
        fonts = Fonts(find_font(args.font))
        renderer = Renderer(size, fps, scenes, shots, fonts, args.lang)
        renderer.check_shots()
        out = Path(args.out)
        out.mkdir(parents=True, exist_ok=True)
        stem = f"demo-{args.lang}"

        if args.preview:
            preview(renderer, out / f"{stem}-preview.png")
            return 0

        ffmpeg = None
        if not args.frames_only:
            ffmpeg = find_ffmpeg(args.ffmpeg)
            if not ffmpeg:
                print(FFMPEG_HELP, file=sys.stderr)
                return 2

        poster = poster_scene(renderer)
        renderer.render_scene(poster, poster.duration / 2).save(out / f"{stem}-poster.png")
        print(f"wrote {out / (stem + '-poster.png')}")

        if args.frames_only:
            frames_dir = out / f"frames-{args.lang}"
            if frames_dir.exists():
                shutil.rmtree(frames_dir)
            frames_dir.mkdir()
        else:
            frames_dir = Path(tempfile.mkdtemp(prefix="solo-demo-"))

        anim_frames = []
        anim_step = fps / args.anim_fps if args.anim else None
        next_anim = 0.0
        count = renderer.frame_count()
        try:
            for n in range(count):
                frame = renderer.frame(n)
                frame.save(frames_dir / f"{n:05d}.png", compress_level=1)
                if anim_step is not None and n >= next_anim - 1e-9:
                    next_anim += anim_step
                    height = round(args.anim_width * renderer.h / renderer.w)
                    anim_frames.append(frame.resize((args.anim_width, height), Image.LANCZOS))
                if n % fps == 0:
                    print(f"\rframe {n + 1}/{count}", end="", flush=True)
            print(f"\rrendered {count} frames ({renderer.total:g} s at {fps} fps, {renderer.w}x{renderer.h})")
            if ffmpeg:
                encode_mp4(ffmpeg, frames_dir, fps, out / f"{stem}.mp4")
        finally:
            if not args.frames_only:
                shutil.rmtree(frames_dir, ignore_errors=True)

        if args.frames_only:
            mp4 = out / f"{stem}.mp4"
            command = ffmpeg_command("ffmpeg", frames_dir, fps, 23, mp4)
            print(f"frames in {frames_dir}; to make the MP4 (raise -crf if it's over 10 MB):")
            print("  " + subprocess.list2cmdline(command))
        if anim_frames:
            save_animation(anim_frames, out / f"{stem}.{args.anim}", args.anim_fps)
        return 0
    except StoryboardError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
