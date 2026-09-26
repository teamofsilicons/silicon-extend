#!/usr/bin/env python3
"""Owned animated GTK window for native recorder verification inside the test desktop."""
import os
import sys
import gi
gi.require_version('Gtk', '3.0')
from gi.repository import Gtk, GdkPixbuf, GLib

window = Gtk.Window(title='Bridge Recording Fixture')
window.set_default_size(641, 481)
image = Gtk.Image()
window.add(image)
window.connect('destroy', Gtk.main_quit)
frame = 0

def tick():
    global frame
    frame += 1
    # A fresh noisy picture exercises the size cap without depending on an external video.
    data = os.urandom(641 * 481 * 3) if '--noise' in sys.argv else bytes([frame % 256, 80, 160]) * (641 * 481)
    pixels = GdkPixbuf.Pixbuf.new_from_bytes(GLib.Bytes.new(data), GdkPixbuf.Colorspace.RGB, False, 8, 641, 481, 641 * 3)
    image.set_from_pixbuf(pixels)
    return True

tick()
GLib.timeout_add(80, tick)
window.show_all()
Gtk.main()
