"""Read one redirected X11 window's pixels without capturing its covering siblings.

XCompositeNameWindowPixmap owns a reference to the window's off-screen storage. Each
frame rechecks geometry; an unmapped, destroyed or resized source ends capture instead
of substituting desktop pixels. See the Xcomposite(3) and XGetImage(3) contracts.
"""
import ctypes as C
import threading
import time


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


class WindowPixels:
    def __init__(self, window):
        self.window = window
        self.display = None
        self.redirected = False
        self.error = None
        self.x = C.CDLL('libX11.so.6')
        self.composite = C.CDLL('libXcomposite.so.1')
        self._bind(self.x, 'XOpenDisplay', C.c_void_p, [C.c_char_p])
        self._bind(self.x, 'XCloseDisplay', C.c_int, [C.c_void_p])
        self._bind(self.x, 'XSync', C.c_int, [C.c_void_p, C.c_int])
        self._bind(self.x, 'XSetErrorHandler', C.c_void_p, [C.c_void_p])
        self._bind(self.x, 'XGetWindowAttributes', C.c_int, [C.c_void_p, C.c_ulong, C.POINTER(Attributes)])
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
        self.handler = C.CFUNCTYPE(C.c_int, C.c_void_p, C.POINTER(XError))(self._error)
        self.previous_handler = self.x.XSetErrorHandler(C.cast(self.handler, C.c_void_p))
        try:
            self.display = self.x.XOpenDisplay(None)
            if not self.display:
                raise RuntimeError('cannot open the X11 display')
            event, error = C.c_int(), C.c_int()
            if not self.composite.XCompositeQueryExtension(self.display, C.byref(event), C.byref(error)):
                raise RuntimeError('app capture requires the XComposite extension')
            self.composite.XCompositeRedirectWindow(self.display, window, 0)  # Automatic
            self._sync()
            self.redirected = True
            self.width = self.height = None
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

    def frame(self):
        attributes = Attributes()
        valid = self.x.XGetWindowAttributes(self.display, self.window, C.byref(attributes))
        self._sync()
        if not valid or attributes.map_state != 2:
            raise RuntimeError('the app window is no longer mapped')
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
            if self.width is not None and (width.value, height.value) != (self.width, self.height):
                raise RuntimeError('the app window was resized; start a new recording for its new size')
            image = self.x.XGetImage(self.display, pixmap, 0, 0, width, height, C.c_ulong(-1), 2)
            self._sync()
            if not image:
                raise RuntimeError('XComposite returned no window image')
            frame = image.contents
            if frame.bits_per_pixel != 32 or visual.visual_class != 4 or (visual.red_mask, visual.green_mask, visual.blue_mask) != (0xff0000, 0xff00, 0xff):
                raise RuntimeError('app recording requires a 24/32-bit TrueColor X11 visual')
            pixel_format = 'bgr0' if frame.byte_order == 0 else '0rgb'
            if self.pixel_format is not None and self.pixel_format != pixel_format:
                raise RuntimeError('the app window pixel format changed')
            self.width, self.height, self.pixel_format = width.value, height.value, pixel_format
            data = C.string_at(frame.data, frame.bytes_per_line * frame.height)
            stride = frame.width * 4
            if frame.bytes_per_line == stride:
                return data
            return b''.join(data[row * frame.bytes_per_line:row * frame.bytes_per_line + stride] for row in range(frame.height))
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
    """One writer owns Xlib and the encoder pipe until it has stopped."""
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
                deadline += 1 / self.fps
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
