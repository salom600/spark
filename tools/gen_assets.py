#!/usr/bin/env python3
"""Generate all demo assets for spark: sprites (PNG), sounds (WAV), fonts none.

Everything is procedural — no binary assets in the repo, fully reproducible.

  demos/ember_run/assets/      player, coin, tile, hazard, background, sky
  demos/ember_run/assets/sfx/  jump, coin, hurt, win
  demos/playground/assets/     checker texture, sky gradient
"""

import math
import struct
import wave
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
EMBER = ROOT / "demos/ember_run/assets"
PLAY = ROOT / "demos/playground/assets"


def save(img: Image.Image, path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path)
    print(f"  {path.relative_to(ROOT)}")


# ---------------------------------------------------------------------------
# Sprites
# ---------------------------------------------------------------------------

def player_sprite():
    img = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Body: warm ember character.
    d.ellipse([16, 12, 48, 44], fill=(255, 120, 40, 255), outline=(120, 40, 10, 255), width=3)
    # Eyes.
    d.ellipse([24, 22, 31, 29], fill=(40, 20, 10, 255))
    d.ellipse([37, 22, 44, 29], fill=(40, 20, 10, 255))
    # Smile.
    d.arc([26, 28, 40, 40], 20, 160, fill=(120, 40, 10, 255), width=2)
    # Feet.
    d.ellipse([18, 42, 30, 52], fill=(200, 80, 20, 255))
    d.ellipse([36, 42, 48, 52], fill=(200, 80, 20, 255))
    # Ember sparks.
    for (x, y, r) in [(8, 16, 3), (54, 20, 2), (12, 38, 2), (52, 36, 3)]:
        d.ellipse([x - r, y - r, x + r, y + r], fill=(255, 200, 60, 255))
    save(img, EMBER / "sprites/player.png")


def coin_sprite():
    img = Image.new("RGBA", (32, 32), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.ellipse([2, 2, 30, 30], fill=(255, 200, 40, 255), outline=(180, 130, 20, 255), width=2)
    d.ellipse([8, 8, 24, 24], outline=(255, 240, 150, 255), width=2)
    d.text((13, 9), "c", fill=(160, 110, 10, 255))
    save(img, EMBER / "sprites/coin.png")


def tile_sprite():
    img = Image.new("RGBA", (48, 48), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rectangle([0, 0, 48, 48], fill=(70, 60, 80, 255))
    d.rectangle([0, 0, 48, 6], fill=(110, 95, 130, 255))
    for x in range(0, 48, 12):
        d.line([x, 6, x, 48], fill=(55, 45, 65, 255), width=1)
    save(img, EMBER / "sprites/tile.png")


def hazard_sprite():
    img = Image.new("RGBA", (48, 32), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Spikes.
    for i in range(4):
        x = i * 12
        d.polygon([(x, 32), (x + 6, 2), (x + 12, 32)], fill=(200, 50, 60, 255), outline=(120, 20, 30, 255))
    save(img, EMBER / "sprites/spikes.png")


def goal_sprite():
    img = Image.new("RGBA", (64, 96), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # Flag pole + flag.
    d.rectangle([8, 4, 12, 92], fill=(60, 50, 60, 255))
    d.polygon([(12, 4), (58, 18), (12, 32)], fill=(80, 220, 120, 255))
    save(img, EMBER / "sprites/goal.png")


def ember_sky():
    """Vertical gradient sky background (1280x720)."""
    w, h = 1280, 720
    img = Image.new("RGBA", (w, h))
    top = (30, 20, 50)
    bottom = (255, 140, 70)
    px = img.load()
    for y in range(h):
        t = y / h
        r = int(top[0] + (bottom[0] - top[0]) * t)
        g = int(top[1] + (bottom[1] - top[1]) * t)
        b = int(top[2] + (bottom[2] - top[2]) * t)
        for x in range(w):
            px[x, y] = (r, g, b, 255)
    # Stars in the top third.
    d = ImageDraw.Draw(img)
    import random
    rng = random.Random(7)
    for _ in range(90):
        x = rng.randint(0, w - 1)
        y = rng.randint(0, h // 3)
        s = rng.choice([1, 1, 2])
        d.rectangle([x, y, x + s, y + s], fill=(255, 255, 220, rng.randint(140, 230)))
    save(img, EMBER / "sprites/sky.png")


def checker_texture():
    img = Image.new("RGBA", (256, 256))
    d = ImageDraw.Draw(img)
    a, b = (200, 200, 205, 255), (150, 150, 160, 255)
    for y in range(8):
        for x in range(8):
            d.rectangle([x * 32, y * 32, x * 32 + 31, y * 32 + 31], fill=a if (x + y) % 2 == 0 else b)
    save(img, PLAY / "textures/checker.png")


def grid_texture():
    img = Image.new("RGBA", (256, 256), (60, 65, 75, 255))
    d = ImageDraw.Draw(img)
    for i in range(0, 256, 32):
        d.line([i, 0, i, 256], fill=(90, 95, 110, 255), width=2)
        d.line([0, i, 256, i], fill=(90, 95, 110, 255), width=2)
    save(img, PLAY / "textures/grid.png")


# ---------------------------------------------------------------------------
# Sounds (WAV, 22050 Hz mono 16-bit)
# ---------------------------------------------------------------------------

RATE = 22050


def write_wav(path: Path, samples):
    path.parent.mkdir(parents=True, exist_ok=True)
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        frames = b"".join(struct.pack("<h", max(-32767, min(32767, int(s * 32767)))) for s in samples)
        w.writeframes(frames)
    print(f"  {path.relative_to(ROOT)}")


def jump_sfx():
    """Rising blip."""
    n = int(RATE * 0.18)
    out = []
    for i in range(n):
        t = i / RATE
        f = 300 + 700 * (i / n)
        env = math.exp(-t * 12) * min(1.0, i / (RATE * 0.005))
        out.append(0.5 * env * math.sin(2 * math.pi * f * t))
    write_wav(EMBER / "sfx/jump.wav", out)


def coin_sfx():
    """Two-tone sparkle."""
    n = int(RATE * 0.22)
    out = []
    for i in range(n):
        t = i / RATE
        f = 880 if t < 0.09 else 1320
        env = math.exp(-t * 14)
        out.append(0.4 * env * math.sin(2 * math.pi * f * t))
    write_wav(EMBER / "sfx/coin.wav", out)


def hurt_sfx():
    """Falling buzz."""
    n = int(RATE * 0.3)
    out = []
    for i in range(n):
        t = i / RATE
        f = 400 - 250 * (i / n)
        env = math.exp(-t * 8)
        saw = 2.0 * ((f * t) % 1.0) - 1.0
        out.append(0.35 * env * saw)
    write_wav(EMBER / "sfx/hurt.wav", out)


def win_sfx():
    """Arpeggio."""
    out = []
    for k, freq in enumerate([523, 659, 784, 1047]):
        n = int(RATE * 0.14)
        for i in range(n):
            t = i / RATE
            env = math.exp(-t * 6)
            out.append(0.4 * env * math.sin(2 * math.pi * freq * t))
    write_wav(EMBER / "sfx/win.wav", out)


def pop_sfx():
    """Short click for spawns (playground)."""
    n = int(RATE * 0.08)
    out = []
    for i in range(n):
        t = i / RATE
        env = math.exp(-t * 40)
        out.append(0.5 * env * math.sin(2 * math.pi * 600 * t))
    write_wav(PLAY / "sfx/pop.wav", out)


# ---------------------------------------------------------------------------

if __name__ == "__main__":
    print("Generating spark demo assets…")
    player_sprite()
    coin_sprite()
    tile_sprite()
    hazard_sprite()
    goal_sprite()
    ember_sky()
    checker_texture()
    grid_texture()
    jump_sfx()
    coin_sfx()
    hurt_sfx()
    win_sfx()
    pop_sfx()
    print("Done.")
