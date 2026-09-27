#!/usr/bin/env python3
"""App capture never records another window's pixels, and never refuses an app that redraws.

When an X11 window is first redirected off-screen, the X server seeds its new off-screen copy
from what is on screen at that spot. With no compositor (no window manager, a non-reparenting
one, or openbox), the parts another window covers start out holding that window's pixels until
the app, or the server, repaints them. An app that stops handling events while covered keeps
showing the cover after it leaves. Outside a shaped window's bounding shape the copy holds
whatever the screen shows there, and nobody ever repaints it.

The recorder therefore trusts none of the seeded copy: it clears each of the app's windows with
exposures (the server repaints any background, the app gets Expose events for all of it) and
reads no frame until every pixel has been drawn again. Every case checks every pixel of every frame for the green peer window, not an
average:
  gtk               GTK window that repaints every 80 ms (-static: only when exposed)
  gtk-native-child  the same, drawing into a native child window that fills its client area
  xmessage          Xaw app whose widgets are bordered child windows (public --app-id path)
  xev               Xlib app with one bordered 50x50 child window
  plain             plain Xlib window with no background (None) and no _NET_WM_PING (plainxlib.c;
                    plain-bg: with a background pixel)
covered by the peer, uncovered, or left by it (covered while the app stops, then uncovered),
responding or not (GTK hangs its main loop and stops answering _NET_WM_PING; the others get
SIGSTOP):
  recorded        every frame shows the app and never the peer
  not responding  refused because the app does not answer a ping: no video is written at all
  did not redraw  refused because nobody redrew the whole window: no video is written
A stopped app is recorded only where the X server itself repaints every pixel, that is where its
windows have backgrounds (xmessage, xev, plain-bg: the frames then show those backgrounds); a
stopped window with no background (plain) is refused, since X11 can't show it is the app's own,
and the refusal says to record the whole screen instead. A shaped GTK window above the peer is
recorded with the part outside its shape black.

Run by record-e2e.sh with RECORD_LANE=record-hung-e2e.py. RECORD_WM=openbox adds a reparenting
window manager; RECORD_WORKER points at another screen-record.py to compare; RECORD_CASES runs
only the named cases (comma-separated).
"""
import ctypes
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time

root = Path('/src')
out = Path(tempfile.mkdtemp(prefix='cover-recording-', dir='/tmp/out'))
fixture_script = root / 'apps/desktop/linux-e2e/record-fixture.py'
worker = Path(os.environ.get('RECORD_WORKER', root / 'vendor/extend-engine/linux/screen-record.py'))
window_manager = os.environ.get('RECORD_WM', '')
print('Artifacts:', out, '| window manager:', window_manager or 'none', '| worker:', worker, flush=True)


def wait(predicate, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError('condition not reached')


def find(*query):
    result = subprocess.run(['xdotool', 'search', '--onlyvisible', *query], capture_output=True, text=True)
    windows = result.stdout.split()
    return windows[0] if result.returncode == 0 and windows else None


def geometry(xid):
    info = subprocess.check_output(['xwininfo', '-id', xid], text=True, env=dict(os.environ, LC_ALL='C'))
    return tuple(int(re.search(rf'{key}:\s*(-?\d+)', info).group(1))
                 for key in ['Absolute upper-left X', 'Absolute upper-left Y', 'Width', 'Height'])


def screen(x, y, width, height):
    return subprocess.check_output(['ffmpeg', '-v', 'error', '-f', 'x11grab', '-video_size', f'{width}x{height}',
                                    '-i', f':99+{x},{y}', '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'])


def peer_pixels(raw):
    """Pixels of the green peer. Chroma subsampling pales thin green lines, so the test is loose."""
    return sum(1 for r, g, b in zip(raw[0::3], raw[1::3], raw[2::3]) if g - max(r, b) > 60)


def mean(raw):
    count = len(raw) // 3
    return tuple(sum(raw[channel::3]) // count for channel in range(3))


def is_target(color):
    return 50 < color[1] < 110 and color[2] > 125


def is_red(color):
    """plainxlib paints 0xd02020."""
    return color[0] > 150 and color[1] < 90 and color[2] < 90


def is_plain_background(color):
    """plainxlib --bg's background, 0x3050a0, which the server paints while the app is stopped."""
    return color[0] < 90 and 50 < color[1] < 120 and color[2] > 120


plain_app = Path('/tmp/plainxlib')
subprocess.run(['cc', '-O1', '-o', str(plain_app), str(root / 'apps/desktop/linux-e2e/plainxlib.c'), '-lX11'],
               check=True)


def frames(video):
    info = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-select_streams', 'v:0', '-show_entries',
                                               'stream=width,height', '-of', 'json', str(video)]))['streams'][0]
    raw = subprocess.check_output(['ffmpeg', '-v', 'error', '-i', str(video), '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'])
    size = info['width'] * info['height'] * 3
    return info['width'], info['height'], [raw[index:index + size] for index in range(0, len(raw), size)]


def half(raw, width, height, right):
    """One vertical half of a packed RGB frame, leaving a 16-pixel margin at the seam and edges."""
    rows = []
    for row in range(16, height - 16):
        start = row * width + (width // 2 + 16 if right else 16)
        rows.append(raw[start * 3:(row * width + (width - 16 if right else width // 2 - 16)) * 3])
    return b''.join(rows)


def shape_left_half(xid):
    """Sets the window's bounding shape to its left half, as a shaped app (xeyes, a splash screen) does."""
    x11, xext = ctypes.CDLL('libX11.so.6'), ctypes.CDLL('libXext.so.6')

    class Rectangle(ctypes.Structure):
        _fields_ = [('x', ctypes.c_short), ('y', ctypes.c_short),
                    ('width', ctypes.c_ushort), ('height', ctypes.c_ushort)]

    x11.XOpenDisplay.restype, x11.XOpenDisplay.argtypes = ctypes.c_void_p, [ctypes.c_char_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    xext.XShapeCombineRectangles.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int, ctypes.c_int,
                                             ctypes.c_int, ctypes.POINTER(Rectangle), ctypes.c_int,
                                             ctypes.c_int, ctypes.c_int]
    _, _, width, height = geometry(xid)
    display = x11.XOpenDisplay(None)
    rectangle = Rectangle(0, 0, width // 2, height)
    xext.XShapeCombineRectangles(display, int(xid), 0, 0, 0, ctypes.byref(rectangle), 1, 0, 0)  # Bounding, Set
    x11.XSync(display, 0)
    x11.XCloseDisplay(display)


def launch(kind, hung):
    if kind.startswith('gtk'):
        command = ['python3', str(fixture_script)] + (['--native-child'] if 'native-child' in kind else [])
        command += (['--static'] if kind.endswith('-static') else []) + (['--hang-after', '1500'] if hung else [])
        process = subprocess.Popen(command, stdout=subprocess.PIPE, text=True)
        return process, wait(lambda: find('--name', '^Extend Recording Fixture$'))
    if kind.startswith('plain'):
        process = subprocess.Popen([str(plain_app)] + (['--bg'] if kind == 'plain-bg' else []),
                                   stdout=subprocess.PIPE, text=True)
        xid = wait(lambda: find('--name', '^Plain Xlib Probe$'))
        assert process.stdout.readline().strip() == 'painted'
        return process, xid
    if kind == 'xmessage':
        process = subprocess.Popen(['xmessage', '-buttons', 'Okay,Cancel',
                                    'Silicon Extend: this text is recorded, never the window above it'])
        return process, wait(lambda: find('--class', '^xmessage$'))
    process = subprocess.Popen(['xev', '-geometry', '320x240'], stdout=subprocess.DEVNULL)
    return process, wait(lambda: find('--name', '^Event Tester$'))


def record(name, target, seconds):
    output, status = out / f'{name}.mp4', out / f'{name}.status.json'
    selection = ['--app-id', target] if not target.isdecimal() else ['--window-id', target]
    recorder = subprocess.Popen(['python3', str(worker), '--out', str(output), '--status', str(status),
                                 *selection, '--fps', '12'], stderr=subprocess.PIPE, text=True)
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline and recorder.poll() is None:
        if status.exists() and json.loads(status.read_text()).get('state') == 'recording':
            break
        time.sleep(.05)
    time.sleep(1.5)  # Record a little after the first frame, including the first frames themselves.
    if recorder.poll() is None:
        recorder.terminate()
    code = recorder.wait(timeout=20)
    return code, recorder.stderr.read(), output, json.loads(status.read_text()) if status.exists() else None


def stop(*processes):
    for process in processes:
        if process is not None and process.poll() is None:
            process.send_signal(signal.SIGCONT)
            process.kill()
            process.wait(timeout=5)


def run_case(name, kind, cover, hung, expected):
    """cover: 'covered' by the peer, 'uncovered', or 'left' (covered while the app hangs, then uncovered)."""
    target = peer = None
    try:
        if kind == 'gtk-shaped':
            peer = subprocess.Popen(['python3', str(fixture_script), '--peer'])
            peer_id = wait(lambda: find('--name', '^Extend Recording Peer$'))
            subprocess.run(['xdotool', 'windowmove', '--sync', peer_id, '0', '0'], check=True)
        target, xid = launch(kind, hung)
        subprocess.run(['xdotool', 'windowmove', xid, '0', '0'], check=True)
        if cover in ('covered', 'left'):
            peer = subprocess.Popen(['python3', str(fixture_script), '--peer'])
            peer_id = wait(lambda: find('--name', '^Extend Recording Peer$'))
            subprocess.run(['xdotool', 'windowmove', peer_id, '0', '0', 'windowraise', peer_id], check=True)
        elif peer is not None:
            subprocess.run(['xdotool', 'windowraise', xid], check=True)
        time.sleep(1)
        if kind == 'gtk-shaped':
            shape_left_half(xid)
            time.sleep(.5)
        if hung and kind.startswith('gtk'):
            assert target.stdout.readline().strip() == 'fixture stopped responding'
        elif hung:
            target.send_signal(signal.SIGSTOP)
        if cover == 'left':
            subprocess.run(['xdotool', 'windowmove', peer_id, '700', '0'], check=True)
            time.sleep(.5)
        x, y, width, height = geometry(xid)
        on_screen = screen(x, y, width, height)
        if cover == 'left' and (kind.startswith('gtk') or (kind == 'plain' and hung)):
            # GTK windows and plainxlib's have no X11 background, so the screen still shows the
            # peer where it was until the app redraws (a stopped one never does).
            assert peer_pixels(on_screen) > .5 * width * height, (name, 'no leftovers of the peer on screen')
        elif cover == 'covered':
            # Covered on screen (GTK leaves a few corner pixels), so any leak shows up as green.
            covering = peer_pixels(on_screen)
            assert covering > .99 * width * height, (name, 'the peer does not cover the target on screen',
                                                      (x, y, width, height), covering)
        elif kind == 'gtk-shaped':
            assert peer_pixels(half(on_screen, width, height, right=True)) > 0, (name, 'the peer is not behind the cut-out')
        else:
            assert peer_pixels(on_screen) == 0, name
        code, error, output, state = record(name.replace(' ', '-').replace(',', ''),
                                            'xmessage' if kind == 'xmessage' else xid, 12)
        if expected != 'recorded':
            leaked = [peer_pixels(frame) for frame in frames(output)[2]] if output.exists() else []
            assert not any(leaked), (name, 'recorded the covering window', code, error, leaked[:5])
            assert code != 0 and not output.exists(), (name, 'a target that cannot redraw was recorded', code, error)
            assert state and state['state'] == 'failed' and expected in state['error'], (name, state)
            assert expected in error and '--scope device' in error, (name, error)
            print(f'PASS {name}: refused before any frame, no video written: {state["error"]}', flush=True)
            return
        assert code == 0 and state and state['state'] == 'completed', (name, code, error, state)
        width, height, video = frames(output)
        assert len(video) >= 3, (name, 'too few frames', len(video))
        leaked = [index for index, frame in enumerate(video) if peer_pixels(frame)]
        assert not leaked, (name, 'frames show the peer', leaked[:5], [peer_pixels(video[index]) for index in leaked[:5]])
        for index in (0, -1):
            frame = video[index]
            if kind == 'gtk-shaped':
                assert is_target(mean(half(frame, width, height, right=False))), (name, index, mean(frame))
                assert max(mean(half(frame, width, height, right=True))) < 16, (name, 'outside the shape is not black', index)
            elif kind.startswith('gtk'):
                assert is_target(mean(frame)), (name, 'frame does not show the target', index, mean(frame))
            elif kind.startswith('plain'):
                shown = mean(frame)
                assert is_red(shown) or (kind == 'plain-bg' and hung and is_plain_background(shown)), (
                    name, 'frame does not show the app', index, shown)
            else:
                assert min(mean(frame)) > 120, (name, 'frame does not show the app', index, mean(frame))
        print(f'PASS {name}: {len(video)} frames, every pixel of every frame is the app, none the peer', flush=True)
    finally:
        stop(peer, target)
        time.sleep(.3)


if window_manager:
    manager = subprocess.Popen([window_manager], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    assert manager.poll() is None, f'{window_manager} did not start'
cases = [
    # name, app, cover, hung, expected ('recorded', or the refusal's reason)
    ('gtk covered', 'gtk', 'covered', False, 'recorded'),
    ('gtk covered static', 'gtk-static', 'covered', False, 'recorded'),
    ('gtk covered not responding', 'gtk', 'covered', True, 'not responding'),
    ('gtk uncovered not responding', 'gtk', 'uncovered', True, 'not responding'),
    ('gtk left by its cover not responding', 'gtk', 'left', True, 'not responding'),
    ('gtk native child uncovered', 'gtk-native-child', 'uncovered', False, 'recorded'),
    ('gtk native child covered', 'gtk-native-child', 'covered', False, 'recorded'),
    ('gtk native child uncovered static', 'gtk-native-child-static', 'uncovered', False, 'recorded'),
    ('gtk native child covered static', 'gtk-native-child-static', 'covered', False, 'recorded'),
    ('gtk native child covered not responding', 'gtk-native-child', 'covered', True, 'not responding'),
    ('xmessage uncovered', 'xmessage', 'uncovered', False, 'recorded'),
    ('xmessage covered', 'xmessage', 'covered', False, 'recorded'),
    # Stopped, but the X server repaints all of these windows itself (their backgrounds and
    # borders), so every pixel is drawn again and nothing is refused.
    ('xmessage covered stopped', 'xmessage', 'covered', True, 'recorded'),
    ('xmessage left by its cover stopped', 'xmessage', 'left', True, 'recorded'),
    ('xev uncovered', 'xev', 'uncovered', False, 'recorded'),
    ('xev covered', 'xev', 'covered', False, 'recorded'),
    ('xev covered stopped', 'xev', 'covered', True, 'recorded'),
    ('plain uncovered', 'plain', 'uncovered', False, 'recorded'),
    ('plain covered', 'plain', 'covered', False, 'recorded'),
    ('plain left by its cover', 'plain', 'left', False, 'recorded'),
    # The leak this lane exists for: no background, no ping, stopped while its cover left.
    ('plain left by its cover stopped', 'plain', 'left', True, 'did not redraw'),
    ('plain covered stopped', 'plain', 'covered', True, 'did not redraw'),
    # Stopped, but its window has a background, which the server paints: recorded as that.
    ('plain with a background left by its cover stopped', 'plain-bg', 'left', True, 'recorded'),
    ('plain uncovered stopped', 'plain', 'uncovered', True, 'did not redraw'),
    ('gtk shaped above the peer', 'gtk-shaped', 'uncovered', False, 'recorded'),
]
only = os.environ.get('RECORD_CASES')
failures = []
for case in cases:
    if only and case[0] not in only.split(','):
        continue
    try:
        run_case(*case)
    except AssertionError as failure:
        failures.append(case[0])
        print(f'FAIL {case[0]}: {failure}', flush=True)
assert not failures, f'{len(failures)} case(s) failed: {", ".join(failures)}'
print(f'PASS all cases ({window_manager or "no window manager"})', flush=True)
