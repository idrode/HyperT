#!/usr/bin/env python3
"""Rasteriza una pantalla capturada (.cells.json de drive.py) a PNG, imitando
cómo pinta Kitty cada celda: braille y bloques se dibujan procedurales (igual
que hace Kitty, que los sintetiza en vez de usar la fuente) y el texto normal
con JetBrains Mono. Sirve para VER el render de caracteres con sus colores.

Uso: render_screen.py <captura.cells.json> <salida.png> [escala]
"""
import json
import sys

from PIL import Image, ImageDraw, ImageFont

CW, CH = 10, 20  # celda en px, igual que la usada en las capturas

NAMED = {
    "black": (30, 33, 39), "red": (224, 108, 117), "green": (152, 195, 121),
    "brown": (229, 192, 123), "yellow": (229, 192, 123), "blue": (97, 175, 239),
    "magenta": (198, 120, 221), "cyan": (86, 182, 194), "white": (200, 204, 210),
    "brightblack": (92, 99, 112), "brightred": (240, 128, 137),
    "brightgreen": (172, 215, 141), "brightbrown": (245, 208, 139),
    "brightyellow": (245, 208, 139), "brightblue": (117, 195, 255),
    "brightmagenta": (218, 140, 241), "brightcyan": (106, 202, 214),
    "brightwhite": (220, 224, 230),
}
DEF_FG = (201, 209, 217)
DEF_BG = (13, 17, 23)


def color(v, default):
    if v in (None, "default"):
        return default
    if v in NAMED:
        return NAMED[v]
    try:
        return tuple(int(v[i : i + 2], 16) for i in (0, 2, 4))
    except (ValueError, IndexError):
        return default


def draw_cell(d, px, py, ch, fg, bg):
    d.rectangle([px, py, px + CW - 1, py + CH - 1], fill=bg)
    if ch == " " or not ch:
        return
    cp = ord(ch[0])
    # braille U+2800..U+28FF: rejilla 2x4 de puntos
    if 0x2800 <= cp <= 0x28FF:
        bits = cp - 0x2800
        pos = {0: (0, 0), 1: (0, 1), 2: (0, 2), 3: (1, 0), 4: (1, 1), 5: (1, 2), 6: (0, 3), 7: (1, 3)}
        for b, (cx, cy) in pos.items():
            if bits & (1 << b):
                x0 = px + cx * (CW // 2) + 1
                y0 = py + cy * (CH // 4) + 1
                d.ellipse([x0, y0, x0 + CW // 2 - 3, y0 + CH // 4 - 3], fill=fg)
        return True
    # bloques inferiores ▁..█ (U+2581..2588)
    if 0x2581 <= cp <= 0x2588:
        k = cp - 0x2580
        h = CH * k // 8
        d.rectangle([px, py + CH - h, px + CW - 1, py + CH - 1], fill=fg)
        return True
    # bloques izquierdos ▉..▏ (U+2589..258F, de 7/8 a 1/8)
    if 0x2589 <= cp <= 0x258F:
        w = CW * (0x2590 - cp) // 8
        d.rectangle([px, py, px + w - 1, py + CH - 1], fill=fg)
        return True
    if ch == "▀":
        d.rectangle([px, py, px + CW - 1, py + CH // 2 - 1], fill=fg)
        return True
    if ch == "▐":
        d.rectangle([px + CW // 2, py, px + CW - 1, py + CH - 1], fill=fg)
        return True
    # cuadrantes U+2596..259F
    QUAD = {
        0x2596: "bl", 0x2597: "br", 0x2598: "tl", 0x2599: "tl bl br",
        0x259A: "tl br", 0x259B: "tl tr bl", 0x259C: "tl tr br",
        0x259D: "tr", 0x259E: "tr bl", 0x259F: "tr bl br",
    }
    if cp in QUAD:
        for q in QUAD[cp].split():
            x0 = px if "l" in q else px + CW // 2
            y0 = py if "t" in q else py + CH // 2
            d.rectangle([x0, y0, x0 + CW // 2 - 1, y0 + CH // 2 - 1], fill=fg)
        return True
    # sombras ░▒▓: mezcla fg/bg
    SHADE = {0x2591: 0.25, 0x2592: 0.5, 0x2593: 0.75}
    if cp in SHADE:
        a = SHADE[cp]
        mix = tuple(int(f * a + b * (1 - a)) for f, b in zip(fg, bg))
        d.rectangle([px, py, px + CW - 1, py + CH - 1], fill=mix)
        return True
    # líneas de caja básicas
    mx, my = px + CW // 2, py + CH // 2
    BOX = {
        "─": "h", "━": "h", "│": "v", "┃": "v",
        "┌": "r b", "┐": "l b", "└": "r t", "┘": "l t",
        "├": "v r", "┤": "v l", "┬": "h b", "┴": "h t", "┼": "h v",
        "╭": "r b", "╮": "l b", "╰": "r t", "╯": "l t",
    }
    if ch in BOX:
        for seg in BOX[ch].split():
            if seg == "h":
                d.line([px, my, px + CW - 1, my], fill=fg, width=1)
            elif seg == "v":
                d.line([mx, py, mx, py + CH - 1], fill=fg, width=1)
            elif seg == "l":
                d.line([px, my, mx, my], fill=fg, width=1)
            elif seg == "r":
                d.line([mx, my, px + CW - 1, my], fill=fg, width=1)
            elif seg == "t":
                d.line([mx, py, mx, my], fill=fg, width=1)
            elif seg == "b":
                d.line([mx, my, mx, py + CH - 1], fill=fg, width=1)
        return True
    return False  # que lo pinte la fuente


def main() -> None:
    data = json.load(open(sys.argv[1]))
    out = sys.argv[2]
    scale = int(sys.argv[3]) if len(sys.argv) > 3 else 1
    cols, rows = data["cols"], data["rows"]
    img = Image.new("RGB", (cols * CW, rows * CH), DEF_BG)
    d = ImageDraw.Draw(img)
    try:
        font = ImageFont.truetype(
            "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf", 15
        )
    except OSError:
        font = ImageFont.load_default()

    for y, row in enumerate(data["cells"]):
        for x, (ch, fg, bg, rev) in enumerate(row):
            f, b = color(fg, DEF_FG), color(bg, DEF_BG)
            if rev:
                f, b = b, f
            px, py = x * CW, y * CH
            if not draw_cell(d, px, py, ch, f, b):
                d.text((px, py + 2), ch, font=font, fill=f)

    if scale > 1:
        img = img.resize((img.width * scale, img.height * scale), Image.NEAREST)
    img.save(out)
    print(f"{out}: {img.width}x{img.height} px")


if __name__ == "__main__":
    main()
