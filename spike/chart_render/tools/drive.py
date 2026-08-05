#!/usr/bin/env python3
"""Conduce un binario TUI dentro de un pty y captura la salida.

Produce:
  <out>.raw        bytes crudos (incluye secuencias APC del protocolo Kitty)
  <out>.txt        pantalla final como texto plano (pyte)
  <out>.cells.json pantalla final con estilo por celda (char, fg, bg, reverse)

Uso: drive.py <out_prefix> <cols> <rows> <cellw_px> <cellh_px> <segundos> -- <cmd> [args...]
Las variables de entorno (CHART_PROTO, etc.) se heredan del entorno del llamador.
"""
import fcntl
import json
import os
import pty
import re
import select
import signal
import struct
import sys
import termios
import time


def main() -> None:
    out, cols, rows, cellw, cellh, secs = (
        sys.argv[1],
        int(sys.argv[2]),
        int(sys.argv[3]),
        int(sys.argv[4]),
        int(sys.argv[5]),
        float(sys.argv[6]),
    )
    cmd = sys.argv[sys.argv.index("--") + 1 :]

    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-kitty"
        os.environ["COLORTERM"] = "truecolor"
        os.execvp(cmd[0], cmd)

    # winsize con tamaño en píxeles: un kitty real también lo reporta
    fcntl.ioctl(
        fd,
        termios.TIOCSWINSZ,
        struct.pack("HHHH", rows, cols, cols * cellw, rows * cellh),
    )

    raw = bytearray()
    deadline = time.time() + secs
    quit_sent_at = None
    while True:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                data = os.read(fd, 1 << 16)
            except OSError:
                break
            if not data:
                break
            raw += data
        now = time.time()
        if quit_sent_at is None and now > deadline:
            os.write(fd, b"q")
            quit_sent_at = now
        elif quit_sent_at is not None and now - quit_sent_at > 3.0:
            os.kill(pid, signal.SIGKILL)
            break
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass
    os.close(fd)

    with open(out + ".raw", "wb") as f:
        f.write(raw)

    # pyte no entiende APC (protocolo Kitty): se filtra antes de alimentarlo
    clean = re.sub(rb"\x1b_[^\x1b]*\x1b\\", b"", bytes(raw))
    import pyte

    screen = pyte.Screen(cols, rows)
    stream = pyte.ByteStream(screen)
    stream.feed(clean)

    with open(out + ".txt", "w") as f:
        f.write("\n".join(screen.display))

    cells = []
    for y in range(rows):
        row = []
        for x in range(cols):
            ch = screen.buffer[y][x]
            row.append([ch.data, ch.fg, ch.bg, bool(ch.reverse)])
        cells.append(row)
    with open(out + ".cells.json", "w") as f:
        json.dump({"cols": cols, "rows": rows, "cells": cells}, f)
    print(f"capturado: {out}.raw ({len(raw)} bytes), .txt, .cells.json")


if __name__ == "__main__":
    main()
