#!/usr/bin/env python3
"""Extrae las imágenes transmitidas por protocolo gráfico Kitty de una captura cruda.

Reensambla los chunks APC (\\x1b_G...;payload\\x1b\\), decodifica base64 (+zlib si o=z)
y guarda cada imagen como PNG: soporta f=32 (RGBA crudo), f=24 (RGB) y f=100 (PNG).

Uso: decode_kitty.py <captura.raw> <prefijo_salida>
"""
import base64
import io
import re
import sys
import zlib

from PIL import Image


def main() -> None:
    raw = open(sys.argv[1], "rb").read()
    prefix = sys.argv[2]
    seqs = re.findall(rb"\x1b_G([^;\x1b]*)(?:;([^\x1b]*))?\x1b\\", raw)

    images = []
    cur = None
    for params_b, payload in seqs:
        params = dict(
            p.split(b"=", 1) for p in params_b.split(b",") if b"=" in p
        )
        payload = payload or b""
        starts = b"f" in params or b"s" in params or params.get(b"a") in (b"T", b"t")
        if starts and (cur is None or b"a" in params):
            cur = {"params": params, "data": bytearray()}
        if cur is None:
            continue  # comandos sin datos (p.ej. a=p colocación)
        cur["data"] += payload
        if int(params.get(b"m", b"0")) == 0:
            if cur["data"]:
                images.append(cur)
            cur = None

    if not images:
        print("no se encontraron transmisiones de imagen")
        return
    for i, im in enumerate(images):
        p = im["params"]
        data = base64.b64decode(bytes(im["data"]))
        if p.get(b"o") == b"z":
            data = zlib.decompress(data)
        fmt = p.get(b"f", b"32")
        if fmt == b"100":
            img = Image.open(io.BytesIO(data))
        else:
            w, h = int(p[b"s"]), int(p[b"v"])
            mode = "RGBA" if fmt == b"32" else "RGB"
            img = Image.frombytes(mode, (w, h), data)
        out = f"{prefix}_{i}.png"
        img.convert("RGB").save(out)
        print(f"{out}: {img.size[0]}x{img.size[1]} px, params={dict(p)}")


if __name__ == "__main__":
    main()
