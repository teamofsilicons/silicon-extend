#!/usr/bin/env python3
"""Owned animated GTK window for native recorder verification inside the test desktop.

  --peer             a solid green window titled 'Extend Recording Peer' (the covering app)
  --noise            random pixels each tick (exercises the size cap)
  --size WxH         window size (default 641x481)
  --static           paint once instead of every 80 ms
  --hang-after MS    stop handling X events after MS milliseconds, like an app that is not
                     responding: it never repaints again, not even when exposed
  --native-child     draw into a native X11 child window that fills the client area, like
                     toolkits whose widgets or video and GL surfaces are windows of their own
"""
import os
import sys
import time
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, Gdk, GdkPixbuf, GLib


def option(name, default=None):
    return sys.argv[sys.argv.index(name) + 1] if name in sys.argv else default


peer = '--peer' in sys.argv
width, height = (int(value) for value in option('--size', '641x481').split('x'))
GLib.set_prgname('extend-recording-peer' if peer else 'extend-recording-fixture')
Gdk.set_program_class('extend-recording-peer' if peer else 'extend-recording-fixture')
if os.environ.get('EXTEND_RECORD_FIXTURE_PID_FILE') and not peer:
    from pathlib import Path
    Path(os.environ['EXTEND_RECORD_FIXTURE_PID_FILE']).write_text(str(os.getpid()))
window = Gtk.Window(title='Extend Recording Peer' if peer else 'Extend Recording Fixture')
window.set_default_size(width, height)
image = Gtk.Image()
native_child = Gtk.EventBox() if '--native-child' in sys.argv else None
if native_child:
    native_child.add(image)
window.add(native_child or image)
window.connect('destroy', Gtk.main_quit)
frame = 0


def tick():
    global frame
    frame += 1
    # A fresh noisy picture exercises the size cap without depending on an external video.
    data = bytes([5, 245, 5]) * (width * height) if peer else os.urandom(width * height * 3) if '--noise' in sys.argv else bytes([frame % 256, 80, 160]) * (width * height)
    pixels = GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(data), GdkPixbuf.Colorspace.RGB, False, 8, width, height, width * 3)
    image.set_from_pixbuf(pixels)
    return True


def hang():
    print('fixture stopped responding', flush=True)
    while True:
        time.sleep(3600)


tick()
if '--static' not in sys.argv:
    GLib.timeout_add(80, tick)
if option('--hang-after') is not None:
    GLib.timeout_add(int(option('--hang-after')), hang)
window.show_all()
if native_child:
    native_child.get_window().ensure_native()
Gtk.main()
