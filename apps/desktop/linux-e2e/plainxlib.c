/* A plain Xlib app for record-hung-e2e.py: one top-level window titled "Plain Xlib Probe", no
   WM_PROTOCOLS (so no _NET_WM_PING), which paints solid red on every Expose, synthetic or not.
   --bg gives the window a background pixel; without it the background is None (the
   XCreateWindow default, which GLFW, SDL and many toolkits keep to avoid flicker), so the X server
   never repaints it: after a covering window moves away, it shows that window until it redraws. */
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
  int bg = argc > 1 && !strcmp(argv[1], "--bg");
  Display *d = XOpenDisplay(NULL);
  if (!d) return 1;
  int s = DefaultScreen(d);
  XSetWindowAttributes a;
  unsigned long mask = CWEventMask;
  a.event_mask = ExposureMask | StructureNotifyMask;
  if (bg) { a.background_pixel = 0x3050a0; mask |= CWBackPixel; }
  Window w = XCreateWindow(d, RootWindow(d, s), 0, 0, 400, 300, 0, CopyFromParent, InputOutput,
                           CopyFromParent, mask, &a);
  XClassHint ch = {"plainxlib", "plainxlib"};
  XSetClassHint(d, w, &ch);
  XStoreName(d, w, "Plain Xlib Probe");
  XMapWindow(d, w);
  GC gc = XCreateGC(d, w, 0, NULL);
  XSetForeground(d, gc, 0xd02020);
  for (;;) {
    XEvent e;
    XNextEvent(d, &e);
    if (e.type == Expose) {
      XFillRectangle(d, w, gc, 0, 0, 400, 300);
      XFlush(d);
      printf("painted\n");
      fflush(stdout);
    }
  }
}
