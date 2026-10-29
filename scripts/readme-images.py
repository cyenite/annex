#!/usr/bin/env python3
"""Renders the README's images from real captured output (needs Pillow: python3-pil).

    readme-images.py hero SESSION.txt SCREEN.png OUT.png
    readme-images.py terminal CAPTURE.ansi TITLE OUT.png

SESSION.txt is a plain terminal session whose command lines start with "$ ". CAPTURE.ansi may
carry ANSI colours, as `script -qc "annex setup --dry-run" /dev/null` records them. SCREEN.png
is an instance's screen from `adb exec-out screencap -p`. Images are drawn at twice their
display size so they stay sharp on high-density screens.
"""

import re
import sys
import textwrap

from PIL import Image, ImageDraw, ImageFont

SCALE = 2
FONT_SIZE = 15
WRAP_COLUMNS = 92
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
BOLD_FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"
TITLE_FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"

BACKGROUND = ((31, 138, 112), (15, 76, 92))
WINDOW = (13, 20, 28)
TITLE_BAR = (22, 31, 41)
TEXT = (214, 222, 235)
DIM = (127, 142, 163)
BRIGHT = (255, 255, 255)
COLOURS = {31: (239, 107, 115), 32: (127, 216, 143), 33: (230, 192, 123), 36: (111, 211, 230)}
BUTTONS = ((255, 95, 87), (254, 188, 46), (40, 200, 64))

SGR = re.compile(r"\x1b\[([0-9;]*)m")


def px(value):
    return int(value * SCALE)


def fonts():
    return (
        ImageFont.truetype(FONT, px(FONT_SIZE)),
        ImageFont.truetype(BOLD_FONT, px(FONT_SIZE)),
        ImageFont.truetype(TITLE_FONT, px(13)),
    )


def parse(text):
    """Turns text with ANSI colours into lines of (segment, colour, is_bold) runs."""
    lines = []
    for raw in text.replace("\r", "").rstrip("\n").split("\n"):
        if raw.startswith("$ "):
            lines.append([("$ ", COLOURS[32], True), (raw[2:], BRIGHT, True)])
            continue
        plain = SGR.sub("", raw)
        indent = len(plain) - len(plain.lstrip(" "))
        wrapped = textwrap.wrap(plain, WRAP_COLUMNS, subsequent_indent=" " * (indent + 2)) or [""]
        if len(wrapped) > 1:
            for part in wrapped:
                bullet = part.lstrip(" ")
                if bullet.startswith("• "):
                    cut = len(part) - len(bullet) + 1
                    lines.append([(part[:cut], DIM, False), (part[cut:], TEXT, False)])
                else:
                    lines.append([(part, TEXT, False)])
            continue
        runs, colour, is_bold, position = [], TEXT, False, 0
        for match in SGR.finditer(raw):
            if match.start() > position:
                runs.append((raw[position:match.start()], colour, is_bold))
            for code in [int(c) for c in (match.group(1) or "0").split(";") if c]:
                if code == 0:
                    colour, is_bold = TEXT, False
                elif code == 1:
                    is_bold, colour = True, BRIGHT if colour == TEXT else colour
                elif code == 2:
                    colour = DIM
                elif code in COLOURS:
                    colour = COLOURS[code]
            position = match.end()
        runs.append((raw[position:], colour, is_bold))
        lines.append(runs)
    return lines


def terminal(lines, title, columns=None):
    regular, bold, title_font = fonts()
    char_width = regular.getlength("M")
    line_height = px(FONT_SIZE * 1.55)
    columns = columns or max(sum(len(text) for text, _, _ in line) for line in lines)
    padding, bar = px(22), px(38)
    width = int(columns * char_width) + 2 * padding
    height = bar + padding + len(lines) * line_height + padding
    window = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(window)
    draw.rounded_rectangle((0, 0, width - 1, height - 1), px(12), fill=WINDOW)
    draw.rounded_rectangle((0, 0, width - 1, bar), px(12), fill=TITLE_BAR)
    draw.rectangle((0, bar - px(12), width - 1, bar), fill=TITLE_BAR)
    for index, colour in enumerate(BUTTONS):
        x, y = px(20 + index * 20), bar // 2
        draw.ellipse((x - px(6), y - px(6), x + px(6), y + px(6)), fill=colour)
    draw.text((width / 2, bar / 2), title, font=title_font, fill=DIM, anchor="mm")
    y = bar + padding
    for line in lines:
        x = padding
        for text, colour, is_bold in line:
            draw.text((x, y), text, font=bold if is_bold else regular, fill=colour)
            x += char_width * len(text)
        y += line_height
    return window


def phone(screen_path, height):
    screen = Image.open(screen_path).convert("RGBA")
    bezel = px(10)
    screen_height = height - 2 * bezel
    screen_width = round(screen.width * screen_height / screen.height)
    screen = screen.resize((screen_width, screen_height), Image.LANCZOS)
    mask = Image.new("L", screen.size, 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, *screen.size), px(22), fill=255)
    body = Image.new("RGBA", (screen_width + 2 * bezel, height), (0, 0, 0, 0))
    ImageDraw.Draw(body).rounded_rectangle((0, 0, body.width - 1, height - 1), px(30), fill=(17, 21, 26))
    body.paste(screen, (bezel, bezel), mask)
    return body


def backdrop(width, height):
    canvas = Image.new("RGBA", (width, height))
    draw = ImageDraw.Draw(canvas)
    (r1, g1, b1), (r2, g2, b2) = BACKGROUND
    for y in range(height):
        t = y / max(height - 1, 1)
        draw.line(
            [(0, y), (width, y)],
            fill=(round(r1 + (r2 - r1) * t), round(g1 + (g2 - g1) * t), round(b1 + (b2 - b1) * t)),
        )
    return canvas


def shadowed(canvas, image, position):
    shadow = Image.new("RGBA", image.size, (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((0, 0, *image.size), px(14), fill=(0, 0, 0, 70))
    canvas.alpha_composite(shadow, (position[0] + px(6), position[1] + px(10)))
    canvas.alpha_composite(image, position)


def hero(session, screen, out):
    window = terminal(parse(open(session).read()), "annex")
    device = phone(screen, max(window.height, px(560)))
    margin, gap = px(40), px(36)
    width = margin + window.width + gap + device.width + margin
    height = margin + max(window.height, device.height) + margin
    canvas = backdrop(width, height)
    shadowed(canvas, window, (margin, (height - window.height) // 2))
    shadowed(canvas, device, (margin + window.width + gap, (height - device.height) // 2))
    canvas.convert("RGB").save(out, optimize=True)


def framed_terminal(capture, title, out):
    window = terminal(parse(open(capture).read()), title)
    margin = px(32)
    canvas = backdrop(window.width + 2 * margin, window.height + 2 * margin)
    shadowed(canvas, window, (margin, margin))
    canvas.convert("RGB").save(out, optimize=True)


if __name__ == "__main__":
    if sys.argv[1:2] == ["hero"] and len(sys.argv) == 5:
        hero(*sys.argv[2:])
    elif sys.argv[1:2] == ["terminal"] and len(sys.argv) == 5:
        framed_terminal(*sys.argv[2:])
    else:
        sys.exit(__doc__)
