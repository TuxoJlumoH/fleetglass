#!/usr/bin/env python3
import ctypes
import sys

x11 = ctypes.CDLL("libX11.so.6")
x11.XOpenDisplay.restype = ctypes.c_void_p
x11.XDefaultRootWindow.restype = ctypes.c_ulong
x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
x11.XQueryTree.argtypes = [
    ctypes.c_void_p, ctypes.c_ulong,
    ctypes.POINTER(ctypes.c_ulong), ctypes.POINTER(ctypes.c_ulong),
    ctypes.POINTER(ctypes.POINTER(ctypes.c_ulong)), ctypes.POINTER(ctypes.c_uint)
]
x11.XMoveResizeWindow.argtypes = [
    ctypes.c_void_p, ctypes.c_ulong,
    ctypes.c_int, ctypes.c_int, ctypes.c_uint, ctypes.c_uint
]
x11.XFlush.argtypes = [ctypes.c_void_p]

d = x11.XOpenDisplay(None)
if not d:
    sys.exit("no display")

root = x11.XDefaultRootWindow(d)
root_ret = ctypes.c_ulong()
parent_ret = ctypes.c_ulong()
children_ret = ctypes.POINTER(ctypes.c_ulong)()
nchildren_ret = ctypes.c_uint()

if x11.XQueryTree(d, root, ctypes.byref(root_ret), ctypes.byref(parent_ret),
                  ctypes.byref(children_ret), ctypes.byref(nchildren_ret)):
    print(f"Found {nchildren_ret.value} top windows")
    for i in range(nchildren_ret.value):
        w = children_ret[i]
        x11.XMoveResizeWindow(d, w, 0, 0, 1920, 1000)
    x11.XFlush(d)
    print("Resized all to 1920x1000")
