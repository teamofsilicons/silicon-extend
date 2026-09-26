"""Read one redirected X11 window's pixels without capturing its covering siblings.

XCompositeNameWindowPixmap owns a reference to the window's off-screen storage. Each
frame rechecks geometry; an unmapped, destroyed or resized source ends capture instead
of substituting desktop pixels. See the Xcomposite(3) and XGetImage(3) contracts.

Silicon Extend fork: when a window is first redirected, the X server seeds its new off-screen
copy from what is on screen at that spot (compNewPixmap copies the parent with IncludeInferiors).
With no compositor, the parts another window covered, or that lay off screen, start out holding
someone else's pixels. The server repaints window borders and backgrounds there itself and sends
Expose events for exactly those parts; everything else in the copy is what the app showed. So
the recorder grabs the server while it redirects (nothing can move in between), selects Expose on
the window and on every viewable window inside it (an Expose goes only to the window it names,
never to its inferiors), and creates its XDamage object before the redirect, so the server's own
repaint counts. No frame is read until damage covers every exposed part; an app that does not
redraw them in time, such as one that is not responding, is refused rather than recorded. A
window that nothing covered starts at once. An app that stopped handling events while covered
still shows the cover after it leaves, with no Expose left to reveal that, so an app that
advertises _NET_WM_PING must also answer a ping before the first frame (Xt, Motif and plain Xlib
apps do not advertise it, but the server paints their window backgrounds on every exposure).
Pixels outside a shaped window's bounding shape are seeded from the screen too and nobody
repaints them, so they are recorded black.
The copy is reseeded whenever the window is unmapped, remapped, resized or reparented, so any
of those, however brief, ends capture (StructureNotify catches changes between two frames).
"""
import ctypes as C
import select
import threading
import time

REDRAW_TIMEOUT_S = 5.0
MAX_WATCHED_WINDOWS = 2048
EXPOSURE_MASK = 1 << 15
STRUCTURE_NOTIFY_MASK = 1 << 17
SUBSTRUCTURE_NOTIFY_MASK = 1 << 19
EXPOSE, CLIENT_MESSAGE = 12, 33
DESTROY_NOTIFY, UNMAP_NOTIFY, MAP_NOTIFY, REPARENT_NOTIFY, CONFIGURE_NOTIFY = 17, 18, 19, 21, 22
DAMAGE_REPORT_NON_EMPTY = 3
INPUT_OUTPUT, IS_VIEWABLE = 1, 2
WINDOW_REGION_BOUNDING = 0


class XImage(C.Structure):
    _fields_ = [(name, C.c_int) for name in ['width', 'height', 'xoffset', 'format']] + [
        ('data', C.c_void_p),
    ] + [(name, C.c_int) for name in ['byte_order', 'bitmap_unit', 'bitmap_bit_order', 'bitmap_pad',
                                     'depth', 'bytes_per_line', 'bits_per_pixel']] + [
        ('red_mask', C.c_ulong), ('green_mask', C.c_ulong), ('blue_mask', C.c_ulong),
    ]


class Visual(C.Structure):
    _fields_ = [('ext_data', C.c_void_p), ('visualid', C.c_ulong), ('visual_class', C.c_int),
                ('red_mask', C.c_ulong), ('green_mask', C.c_ulong), ('blue_mask', C.c_ulong),
                ('bits_per_rgb', C.c_int), ('map_entries', C.c_int)]


class Attributes(C.Structure):
    _fields_ = [(name, C.c_int) for name in ['x', 'y', 'width', 'height', 'border_width', 'depth']] + [
        ('visual', C.POINTER(Visual)), ('root', C.c_ulong),
    ] + [(name, C.c_int) for name in ['window_class', 'bit_gravity', 'win_gravity', 'backing_store']] + [
        ('backing_planes', C.c_ulong), ('backing_pixel', C.c_ulong), ('save_under', C.c_int),
        ('colormap', C.c_ulong), ('map_installed', C.c_int), ('map_state', C.c_int),
        ('all_event_masks', C.c_long), ('your_event_mask', C.c_long), ('do_not_propagate_mask', C.c_long),
        ('override_redirect', C.c_int), ('screen', C.c_void_p),
    ]


class XError(C.Structure):
    _fields_ = [('type', C.c_int), ('display', C.c_void_p), ('resourceid', C.c_ulong),
                ('serial', C.c_ulong), ('error_code', C.c_ubyte), ('request_code', C.c_ubyte),
                ('minor_code', C.c_ubyte)]


class XRectangle(C.Structure):
    _fields_ = [('x', C.c_short), ('y', C.c_short), ('width', C.c_ushort), ('height', C.c_ushort)]


class XConfigureEvent(C.Structure):
    _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int), ('display', C.c_void_p),
                ('event', C.c_ulong), ('window', C.c_ulong), ('x', C.c_int), ('y', C.c_int),
                ('width', C.c_int), ('height', C.c_int), ('border_width', C.c_int),
                ('above', C.c_ulong), ('override_redirect', C.c_int)]


class XExposeEvent(C.Structure):
    _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int), ('display', C.c_void_p),
                ('window', C.c_ulong), ('x', C.c_int), ('y', C.c_int), ('width', C.c_int), ('height', C.c_int),
                ('count', C.c_int)]


class XStructureEvent(C.Structure):
    """The prefix every *Notify structure event shares: the selecting window, then the changed one."""
    _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int), ('display', C.c_void_p),
                ('event', C.c_ulong), ('window', C.c_ulong)]


class XClientMessageEvent(C.Structure):
    _fields_ = [('type', C.c_int), ('serial', C.c_ulong), ('send_event', C.c_int), ('display', C.c_void_p),
                ('window', C.c_ulong), ('message_type', C.c_ulong), ('format', C.c_int), ('data', C.c_long * 5)]


class XEvent(C.Union):
    _fields_ = [('type', C.c_int), ('xstructure', XStructureEvent), ('xconfigure', XConfigureEvent),
                ('xexpose', XExposeEvent), ('xclient', XClientMessageEvent), ('pad', C.c_long * 24)]


def load_library(name, package):
    try:
        return C.CDLL(name)
    except OSError:
        raise RuntimeError(f'app recording needs {name}, which is not installed (Debian and Ubuntu package '
                           f'{package}); install it, or record the whole screen with --scope device') from None


class WindowPixels:
    def __init__(self, window, redraw_timeout=REDRAW_TIMEOUT_S):
        self.window = window
        self.display = None
        self.redirected = False
        self.error = None
        self.masked, self.blank = [], memoryview(b'')
        self.root, self.ping, self.answered = None, None, False
        self.x = load_library('libX11.so.6', 'libx11-6')
        self.composite = load_library('libXcomposite.so.1', 'libxcomposite1')
        self.damage = load_library('libXdamage.so.1', 'libxdamage1')
        self.fixes = load_library('libXfixes.so.3', 'libxfixes3')
        self._bind(self.x, 'XOpenDisplay', C.c_void_p, [C.c_char_p])
        self._bind(self.x, 'XCloseDisplay', C.c_int, [C.c_void_p])
        self._bind(self.x, 'XSync', C.c_int, [C.c_void_p, C.c_int])
        self._bind(self.x, 'XGrabServer', C.c_int, [C.c_void_p])
        self._bind(self.x, 'XUngrabServer', C.c_int, [C.c_void_p])
        self._bind(self.x, 'XSetErrorHandler', C.c_void_p, [C.c_void_p])
        self._bind(self.x, 'XGetWindowAttributes', C.c_int, [C.c_void_p, C.c_ulong, C.POINTER(Attributes)])
        self._bind(self.x, 'XQueryTree', C.c_int, [C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong), C.POINTER(C.c_ulong),
                   C.POINTER(C.POINTER(C.c_ulong)), C.POINTER(C.c_uint)])
        self._bind(self.x, 'XSelectInput', C.c_int, [C.c_void_p, C.c_ulong, C.c_long])
        self._bind(self.x, 'XInternAtom', C.c_ulong, [C.c_void_p, C.c_char_p, C.c_int])
        self._bind(self.x, 'XGetWMProtocols', C.c_int, [C.c_void_p, C.c_ulong, C.POINTER(C.POINTER(C.c_ulong)),
                   C.POINTER(C.c_int)])
        self._bind(self.x, 'XSendEvent', C.c_int, [C.c_void_p, C.c_ulong, C.c_int, C.c_long, C.POINTER(XEvent)])
        self._bind(self.x, 'XPending', C.c_int, [C.c_void_p])
        self._bind(self.x, 'XNextEvent', C.c_int, [C.c_void_p, C.POINTER(XEvent)])
        self._bind(self.x, 'XConnectionNumber', C.c_int, [C.c_void_p])
        self._bind(self.x, 'XFree', C.c_int, [C.c_void_p])
        self._bind(self.x, 'XFreePixmap', C.c_int, [C.c_void_p, C.c_ulong])
        self._bind(self.x, 'XGetGeometry', C.c_int, [C.c_void_p, C.c_ulong, C.POINTER(C.c_ulong),
                   C.POINTER(C.c_int), C.POINTER(C.c_int), *([C.POINTER(C.c_uint)] * 4)])
        self._bind(self.x, 'XGetImage', C.POINTER(XImage), [C.c_void_p, C.c_ulong, C.c_int,
                   C.c_int, C.c_uint, C.c_uint, C.c_ulong, C.c_int])
        self._bind(self.x, 'XDestroyImage', C.c_int, [C.POINTER(XImage)])
        self._bind(self.composite, 'XCompositeQueryExtension', C.c_int, [C.c_void_p, C.POINTER(C.c_int), C.POINTER(C.c_int)])
        self._bind(self.composite, 'XCompositeRedirectWindow', None, [C.c_void_p, C.c_ulong, C.c_int])
        self._bind(self.composite, 'XCompositeUnredirectWindow', None, [C.c_void_p, C.c_ulong, C.c_int])
        self._bind(self.composite, 'XCompositeNameWindowPixmap', C.c_ulong, [C.c_void_p, C.c_ulong])
        self._bind(self.damage, 'XDamageQueryExtension', C.c_int, [C.c_void_p, C.POINTER(C.c_int), C.POINTER(C.c_int)])
        self._bind(self.damage, 'XDamageCreate', C.c_ulong, [C.c_void_p, C.c_ulong, C.c_int])
        self._bind(self.damage, 'XDamageSubtract', None, [C.c_void_p, C.c_ulong, C.c_ulong, C.c_ulong])
        self._bind(self.damage, 'XDamageDestroy', None, [C.c_void_p, C.c_ulong])
        self._bind(self.fixes, 'XFixesQueryExtension', C.c_int, [C.c_void_p, C.POINTER(C.c_int), C.POINTER(C.c_int)])
        self._bind(self.fixes, 'XFixesCreateRegion', C.c_ulong, [C.c_void_p, C.POINTER(XRectangle), C.c_int])
        self._bind(self.fixes, 'XFixesCreateRegionFromWindow', C.c_ulong, [C.c_void_p, C.c_ulong, C.c_int])
        self._bind(self.fixes, 'XFixesDestroyRegion', None, [C.c_void_p, C.c_ulong])
        self._bind(self.fixes, 'XFixesUnionRegion', None, [C.c_void_p, C.c_ulong, C.c_ulong, C.c_ulong])
        self._bind(self.fixes, 'XFixesSubtractRegion', None, [C.c_void_p, C.c_ulong, C.c_ulong, C.c_ulong])
        self._bind(self.fixes, 'XFixesFetchRegion', C.POINTER(XRectangle), [C.c_void_p, C.c_ulong, C.POINTER(C.c_int)])
        self.handler = C.CFUNCTYPE(C.c_int, C.c_void_p, C.POINTER(XError))(self._error)
        self.previous_handler = self.x.XSetErrorHandler(C.cast(self.handler, C.c_void_p))
        self.event = XEvent()
        try:
            self.display = self.x.XOpenDisplay(None)
            if not self.display:
                raise RuntimeError('cannot open the X11 display')
            for library, name, extension in [(self.composite, 'XCompositeQueryExtension', 'XComposite'),
                                              (self.damage, 'XDamageQueryExtension', 'XDamage'),
                                              (self.fixes, 'XFixesQueryExtension', 'XFixes')]:
                event, error = C.c_int(), C.c_int()
                if not getattr(library, name)(self.display, C.byref(event), C.byref(error)):
                    raise RuntimeError(f'app recording requires the {extension} extension, which this X server '
                                       'does not offer; record the whole screen with --scope device')
            self._redirect(redraw_timeout)
            self.pixel_format = None
            self.frame()  # Establish the encoder geometry and pixel format before it starts.
        except BaseException:
            self.close()
            raise

    @staticmethod
    def _bind(library, name, result, arguments):
        function = getattr(library, name)
        function.restype, function.argtypes = result, arguments

    def _error(self, _display, error):
        self.error = error.contents.error_code
        return 0

    def _sync(self):
        self.x.XSync(self.display, 0)
        if self.error is not None:
            code, self.error = self.error, None
            raise RuntimeError(f'the app window is unavailable (X11 error {code})')

    def _attributes(self):
        attributes = Attributes()
        valid = self.x.XGetWindowAttributes(self.display, self.window, C.byref(attributes))
        self._sync()
        if not valid or attributes.map_state != IS_VIEWABLE:
            raise RuntimeError('the app window is no longer mapped')
        return attributes

    def _check_event(self, event):
        """Notes a ping reply, and ends capture on any change that makes the server reseed the copy."""
        kind = event.type
        if kind == CLIENT_MESSAGE and self.ping:
            reply = event.xclient
            # data holds C longs; X11 atoms, window IDs and timestamps are 32-bit.
            echoed = tuple(value & 0xffffffff for value in reply.data[:3])
            self.answered = self.answered or (reply.message_type, *echoed) == self.ping
        if kind not in (DESTROY_NOTIFY, UNMAP_NOTIFY, MAP_NOTIFY, REPARENT_NOTIFY, CONFIGURE_NOTIFY):
            return
        if event.xstructure.window != self.window:
            return  # Another root child, selected only while waiting for a ping reply.
        if kind != CONFIGURE_NOTIFY:
            raise RuntimeError('the app window is no longer mapped')
        change = event.xconfigure
        if (change.width, change.height, change.border_width) != (self.width, self.height, self.border):
            raise RuntimeError('the app window was resized; start a new recording for its new size')

    def _drain_events(self):
        while self.x.XPending(self.display):
            self.x.XNextEvent(self.display, C.byref(self.event))
            self._check_event(self.event)

    def _send_ping(self):
        """Pings an app that advertises _NET_WM_PING, which answers it only while it handles events.

        An app that stopped handling events while covered keeps showing what covered it after the
        cover leaves, both on screen and in the off-screen copy, with no Expose left to reveal it.
        """
        protocols, ping = (self.x.XInternAtom(self.display, name, 0) for name in (b'WM_PROTOCOLS', b'_NET_WM_PING'))
        advertised, count = C.POINTER(C.c_ulong)(), C.c_int()
        if not self.x.XGetWMProtocols(self.display, self.window, C.byref(advertised), C.byref(count)):
            return
        try:
            if ping not in [advertised[index] for index in range(count.value)]:
                return
        finally:
            if advertised:
                self.x.XFree(advertised)
        # The app answers by sending the message back to the root window (EWMH _NET_WM_PING).
        self.x.XSelectInput(self.display, self.root, SUBSTRUCTURE_NOTIFY_MASK)
        token = int(time.monotonic() * 1000) & 0x7fffffff
        message = XEvent()
        request = message.xclient
        request.type, request.window, request.message_type, request.format = CLIENT_MESSAGE, self.window, protocols, 32
        request.data[0], request.data[1], request.data[2] = ping, token, self.window
        self.ping = (protocols, ping, token, self.window)
        self.x.XSendEvent(self.display, self.window, 0, 0, C.byref(message))
        self._sync()

    def _inner_windows(self):
        """The window and every viewable window inside it, each with its origin in window coordinates."""
        found, pending = {}, [(self.window, 0, 0)]
        while pending:
            window, x, y = pending.pop()
            found[window] = (x, y)
            if len(found) > MAX_WATCHED_WINDOWS:
                raise RuntimeError(f'the app window holds more than {MAX_WATCHED_WINDOWS} windows, too many to check '
                                   'that each has redrawn; record the whole screen with --scope device')
            root, parent, count = C.c_ulong(), C.c_ulong(), C.c_uint()
            children = C.POINTER(C.c_ulong)()
            if not self.x.XQueryTree(self.display, window, C.byref(root), C.byref(parent), C.byref(children),
                                     C.byref(count)):
                raise RuntimeError('the app window is no longer mapped')
            try:
                inner = [children[index] for index in range(count.value)]
            finally:
                if children:
                    self.x.XFree(children)
            for child in inner:
                attributes = Attributes()
                if (self.x.XGetWindowAttributes(self.display, child, C.byref(attributes))
                        and attributes.map_state == IS_VIEWABLE and attributes.window_class == INPUT_OUTPUT):
                    border = attributes.border_width
                    pending.append((child, x + attributes.x + border, y + attributes.y + border))
        self._sync()
        return found

    def _rectangles(self, region):
        count = C.c_int()
        rectangles = self.fixes.XFixesFetchRegion(self.display, region, C.byref(count))
        try:
            return [(rectangles[index].x, rectangles[index].y, rectangles[index].width, rectangles[index].height)
                    for index in range(count.value)]
        finally:
            if rectangles:
                self.x.XFree(rectangles)

    def _exposed(self, watched):
        """The queued Expose rectangles of the watched windows, clipped to the window's interior."""
        exposed = []
        while self.x.XPending(self.display):
            self.x.XNextEvent(self.display, C.byref(self.event))
            if self.event.type != EXPOSE:
                self._check_event(self.event)
                continue
            expose = self.event.xexpose
            if expose.window not in watched:
                continue
            origin_x, origin_y = watched[expose.window]
            left, top = max(0, origin_x + expose.x), max(0, origin_y + expose.y)
            right = min(self.width, origin_x + expose.x + expose.width)
            bottom = min(self.height, origin_y + expose.y + expose.height)
            if right > left and bottom > top:
                exposed.append(XRectangle(left, top, right - left, bottom - top))
        return exposed

    def _mask_spans(self, rectangles):
        """Byte ranges of a packed frame that hold the given rectangles, merged where they touch."""
        rows = sorted((((top + row) * self.width + left) * 4, width * 4)
                      for left, top, width, height in rectangles for row in range(height))
        merged = []
        for start, length in rows:
            if merged and merged[-1][0] + merged[-1][1] == start:
                merged[-1][1] += length
            else:
                merged.append([start, length])
        return merged

    def _redirect(self, timeout):
        """Redirects the window off-screen and returns once every pixel of the copy is the app's own."""
        regions, damage, watched = [], None, {}

        def region(rectangles=()):
            made = self.fixes.XFixesCreateRegion(self.display, (XRectangle * max(1, len(rectangles)))(*rectangles),
                                                 len(rectangles))
            regions.append(made)
            return made

        try:
            # Held until the redirect is done, so nothing can move, map or draw between the steps.
            self.x.XGrabServer(self.display)
            try:
                attributes = self._attributes()
                if attributes.window_class != INPUT_OUTPUT:
                    raise RuntimeError('the selected X11 window is input-only and has no pixels to record')
                self.width, self.height, self.border = attributes.width, attributes.height, attributes.border_width
                self.root = attributes.root
                watched = self._inner_windows()
                for window in watched:
                    self.x.XSelectInput(self.display, window,
                                        EXPOSURE_MASK | (STRUCTURE_NOTIFY_MASK if window == self.window else 0))
                damage = self.damage.XDamageCreate(self.display, self.window, DAMAGE_REPORT_NON_EMPTY)
                # A new damage object reports the whole window at once (for compositing managers),
                # which says nothing about what anyone drew.
                self.damage.XDamageSubtract(self.display, damage, 0, 0)
                interior = region([XRectangle(0, 0, self.width, self.height)])
                shape = self.fixes.XFixesCreateRegionFromWindow(self.display, self.window, WINDOW_REGION_BOUNDING)
                regions.append(shape)
                outside = region()
                self.fixes.XFixesSubtractRegion(self.display, outside, interior, shape)
                self.composite.XCompositeRedirectWindow(self.display, self.window, 0)  # Automatic
                self._sync()
                self.redirected = True
                hidden = region(self._exposed(watched))
                self.masked = self._mask_spans(self._rectangles(outside))
                self.blank = memoryview(bytes(max((length for _, length in self.masked), default=0)))
                self._sync()
            finally:
                self.x.XUngrabServer(self.display)
                self.x.XSync(self.display, 0)
            self._send_ping()
            painted, parts, missing = region(), region(), region()
            deadline = time.monotonic() + timeout
            connection = self.x.XConnectionNumber(self.display)
            while True:
                self._drain_events()
                self.damage.XDamageSubtract(self.display, damage, 0, parts)
                self.fixes.XFixesUnionRegion(self.display, painted, painted, parts)
                self.fixes.XFixesSubtractRegion(self.display, missing, hidden, painted)
                unpainted = self._rectangles(missing)
                self._sync()
                if not unpainted and (self.ping is None or self.answered):
                    return
                remaining = deadline - time.monotonic()
                if remaining > 0:
                    select.select([connection], [], [], min(remaining, .1))
                elif self.ping is not None and not self.answered:
                    raise RuntimeError(
                        f'the app is not responding: it did not answer an X11 ping within {timeout:g} seconds of '
                        'recording start, so its window could still show whatever last covered it; wait until the '
                        'app responds and record again, or record the whole screen with --scope device')
                else:
                    raise RuntimeError(
                        'part of the app window was hidden when recording started (covered by another window '
                        f'or off screen) and the app did not redraw it within {timeout:g} seconds, so the '
                        'recording would show whatever was there instead; make sure the app is responding and '
                        'record again, or record the whole screen with --scope device')
        finally:
            if self.display:
                if self.ping is not None:
                    self.x.XSelectInput(self.display, self.root, 0)
                    self.ping = None
                for window in watched:
                    self.x.XSelectInput(self.display, window, STRUCTURE_NOTIFY_MASK if window == self.window else 0)
                if damage is not None:
                    self.damage.XDamageDestroy(self.display, damage)
                for made in regions:
                    self.fixes.XFixesDestroyRegion(self.display, made)
                self.x.XSync(self.display, 0)
                # Errors here come from windows destroyed since the grab ended (their event selection
                # and damage went with them); the next frame reports the target itself if it is gone.
                self.error = None

    def frame(self):
        self._drain_events()
        attributes = self._attributes()
        if (attributes.width, attributes.height, attributes.border_width) != (self.width, self.height, self.border):
            raise RuntimeError('the app window was resized; start a new recording for its new size')
        visual = attributes.visual.contents
        pixmap = self.composite.XCompositeNameWindowPixmap(self.display, self.window)
        self._sync()
        image = None
        try:
            root, x, y = C.c_ulong(), C.c_int(), C.c_int()
            width, height, border, depth = (C.c_uint() for _ in range(4))
            valid = self.x.XGetGeometry(self.display, pixmap, C.byref(root), C.byref(x), C.byref(y),
                                       C.byref(width), C.byref(height), C.byref(border), C.byref(depth))
            self._sync()
            if not valid or not width.value or not height.value:
                raise RuntimeError('the app window has no pixels')
            # The pixmap includes the window border; a different size means it was reallocated.
            if (width.value, height.value) != (self.width + 2 * self.border, self.height + 2 * self.border):
                raise RuntimeError('the app window was resized; start a new recording for its new size')
            image = self.x.XGetImage(self.display, pixmap, self.border, self.border, self.width, self.height,
                                     C.c_ulong(-1), 2)
            self._sync()
            if not image:
                raise RuntimeError('XComposite returned no window image')
            frame = image.contents
            if frame.bits_per_pixel != 32 or visual.visual_class != 4 or (visual.red_mask, visual.green_mask, visual.blue_mask) != (0xff0000, 0xff00, 0xff):
                raise RuntimeError('app recording requires a 24/32-bit TrueColor X11 visual')
            pixel_format = 'bgr0' if frame.byte_order == 0 else '0rgb'
            if self.pixel_format is not None and self.pixel_format != pixel_format:
                raise RuntimeError('the app window pixel format changed')
            self.pixel_format = pixel_format
            data = C.string_at(frame.data, frame.bytes_per_line * frame.height)
            stride = frame.width * 4
            if frame.bytes_per_line != stride:
                data = b''.join(data[row * frame.bytes_per_line:row * frame.bytes_per_line + stride]
                                for row in range(frame.height))
            if not self.masked:
                return data
            # Outside a shaped window's bounding shape the copy holds what the screen showed there.
            packed = bytearray(data)
            for start, length in self.masked:
                packed[start:start + length] = self.blank[:length]
            return packed
        finally:
            if image:
                self.x.XDestroyImage(image)
            self.x.XFreePixmap(self.display, pixmap)

    def close(self):
        if self.display:
            if self.redirected:
                self.composite.XCompositeUnredirectWindow(self.display, self.window, 0)
            self.x.XCloseDisplay(self.display)
            self.display = None
        self.x.XSetErrorHandler(self.previous_handler)


class WindowFeed:
    """One writer owns Xlib and the encoder pipe until it has stopped.

    Silicon Extend fork: the encoder stamps each frame with the wall-clock time it arrives and
    keeps a constant frame rate by repeating or dropping frames, so a capture that cannot keep up
    with the requested fps loses smoothness, never timing. The feed therefore never bursts to
    catch up on missed ticks; it just takes the next frame as soon as it can.
    """
    def __init__(self, pixels, pipe, fps):
        self.pixels, self.pipe, self.fps = pixels, pipe, fps
        self.failure = None
        self.stopping = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _run(self):
        try:
            deadline = time.monotonic()
            while not self.stopping.is_set():
                self.pipe.write(self.pixels.frame())
                self.pipe.flush()
                deadline = max(deadline + 1 / self.fps, time.monotonic())
                self.stopping.wait(max(0, deadline - time.monotonic()))
        except (BrokenPipeError, OSError, RuntimeError) as error:
            if not self.stopping.is_set():
                self.failure = str(error)
        finally:
            try:
                self.pipe.close()
            except BrokenPipeError:
                pass
            self.pixels.close()

    def stop(self):
        self.stopping.set()

    def join(self):
        self.thread.join(timeout=2)
        if self.thread.is_alive():
            raise RuntimeError('window frame writer did not stop after encoder exit')
