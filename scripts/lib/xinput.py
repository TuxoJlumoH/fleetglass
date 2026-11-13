#!/usr/bin/env python3
"""Tiny XTest input simulator for headless Xvfb testing (no xdotool needed).

Usage:
  xinput.py click X Y [double]
  xinput.py move X Y                  pointer only, no click (without a window manager Xvfb's
                                      focus follows the pointer, so this focuses the window under it)
  xinput.py key KEYSYM [KEYSYM...]   e.g. Return, Escape, j, Tab
  xinput.py type TEXT
"""

import ctypes
import sys
import time

x11 = ctypes.CDLL("libX11.so.6")
xtst = ctypes.CDLL("libXtst.so.6")

x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XStringToKeysym.restype = ctypes.c_ulong
x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
x11.XFlush.argtypes = [ctypes.c_void_p]

xtst.XTestFakeMotionEvent.argtypes = [
    ctypes.c_void_p,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_int,
    ctypes.c_ulong,
]
xtst.XTestFakeButtonEvent.argtypes = [
    ctypes.c_void_p,
    ctypes.c_uint,
    ctypes.c_int,
    ctypes.c_ulong,
]
xtst.XTestFakeKeyEvent.argtypes = [
    ctypes.c_void_p,
    ctypes.c_uint,
    ctypes.c_int,
    ctypes.c_ulong,
]

d = x11.XOpenDisplay(None)
if not d:
    sys.exit("cannot open display")


def click(x: int, y: int, n: int = 1) -> None:
    xtst.XTestFakeMotionEvent(d, -1, x, y, 0)
    x11.XFlush(d)
    time.sleep(0.05)
    for _ in range(n):
        xtst.XTestFakeButtonEvent(d, 1, 1, 0)
        xtst.XTestFakeButtonEvent(d, 1, 0, 0)
        x11.XFlush(d)
        time.sleep(0.05)


def key(name: str) -> None:
    """KEYSYM or a combo like Control_L+d (modifiers pressed in order)."""
    codes = [
        x11.XKeysymToKeycode(d, x11.XStringToKeysym(p.encode()))
        for p in name.split("+")
    ]
    for c in codes:
        xtst.XTestFakeKeyEvent(d, c, 1, 0)
    for c in reversed(codes):
        xtst.XTestFakeKeyEvent(d, c, 0, 0)
    x11.XFlush(d)
    time.sleep(0.1)


SYMS = {
    "-": "minus",
    ".": "period",
    "/": "slash",
    "_": "Shift_L+underscore",
    " ": "space",
}


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit("usage: xinput.py <click|move|key|type> [args...]")

    cmd = sys.argv[1]
    if cmd == "click":
        if len(sys.argv) < 4:
            sys.exit("usage: xinput.py click X Y [double]")
        x = int(sys.argv[2])
        y = int(sys.argv[3])
        double = len(sys.argv) > 4 and sys.argv[4] == "double"
        click(x, y, 2 if double else 1)
    elif cmd == "move":
        if len(sys.argv) != 4:
            sys.exit("usage: xinput.py move X Y")
        xtst.XTestFakeMotionEvent(d, -1, int(sys.argv[2]), int(sys.argv[3]), 0)
        x11.XFlush(d)
        time.sleep(0.05)
    elif cmd == "key":
        for k in sys.argv[2:]:
            key(k)
    elif cmd == "type":
        for ch in " ".join(sys.argv[2:]):
            key(SYMS.get(ch, ch))
    else:
        sys.exit(f"unknown command: {cmd}")


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        # The calling script handles SIGINT itself; no traceback.
        sys.exit(130)
