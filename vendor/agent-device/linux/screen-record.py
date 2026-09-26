#!/usr/bin/env python3
"""X11 recording worker. Its owning runtime supplies a window XID or requests the root screen.

Requires ffmpeg (x11grab/libx264), ffprobe and xwininfo. Status is published atomically after
an encoded frame, then after MP4 finalization. Native files remain separate from exports.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import time

MAX_BYTES = 1024 ** 3
MAX_DURATION_MS = 30 * 60 * 1000


def options():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', required=True, type=Path)
    parser.add_argument('--status', required=True, type=Path)
    parser.add_argument('--window-id', type=lambda value: int(value, 0))
    parser.add_argument('--fps', type=int, default=30)
    parser.add_argument('--max-bytes', type=int, default=MAX_BYTES)
    parser.add_argument('--max-duration-ms', type=int, default=MAX_DURATION_MS)
    args = parser.parse_args()
    if not (1 <= args.fps <= 60):
        parser.error('fps must be between 1 and 60')
    if not (1024 ** 2 <= args.max_bytes <= MAX_BYTES):
        parser.error('max-bytes must be between 1 MiB and 1 GiB')
    if not (100 <= args.max_duration_ms <= MAX_DURATION_MS):
        parser.error('max-duration-ms must be between 100 and 1800000')
    if not args.out.is_absolute() or not args.status.is_absolute():
        parser.error('out and status must be absolute paths')
    if args.out.resolve() == args.status.resolve():
        parser.error('out and status must be distinct paths')
    if args.out.exists() or args.status.exists():
        parser.error('out and status must not already exist')
    if args.window_id is not None and not (0 < args.window_id <= 0xffffffff):
        parser.error('window-id must be a nonzero X11 window identifier')
    if sys.platform != 'linux':
        parser.error('this worker requires Linux')
    if os.environ.get('WAYLAND_DISPLAY') or os.environ.get('XDG_SESSION_TYPE') == 'wayland':
        parser.error('Wayland requires the ScreenCast portal; XWayland is not a desktop capture source')
    if not os.environ.get('DISPLAY'):
        parser.error('recording requires an X11 display')
    for tool in ['ffmpeg', 'ffprobe', 'xwininfo']:
        if not shutil.which(tool):
            parser.error(f'{tool} is required for X11 recording')
    return args


def publish(path, **state):
    with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, prefix=path.name, delete=False) as stream:
        temp = Path(stream.name)
        try:
            json.dump(dict(pid=os.getpid(), **state), stream)
            stream.flush()
            os.fsync(stream.fileno())
            os.replace(temp, path)
        finally:
            temp.unlink(missing_ok=True)


def geometry(window_id):
    selection = ['-root'] if window_id is None else ['-id', str(window_id)]
    result = subprocess.run(['xwininfo', *selection], capture_output=True, text=True,
                            check=True, timeout=5, env=dict(os.environ, LC_ALL='C'))
    dimensions = [re.search(rf'^\s*{key}:\s*(\d+)\s*$', result.stdout, re.M) for key in ['Width', 'Height']]
    if not all(dimensions) or 'Map State: IsViewable' not in result.stdout:
        raise RuntimeError('the requested X11 window is not viewable')
    return tuple(int(match.group(1)) for match in dimensions)


def run(args):
    width, height = geometry(args.window_id)
    if width < 1 or height < 1:
        raise RuntimeError('the requested X11 window has no pixels')
    reserve = min(16 * 1024 ** 2, args.max_bytes // 4)
    threshold = args.max_bytes - reserve
    command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-n',
               '-progress', 'pipe:1', '-stats_period', '0.1', '-f', 'x11grab',
               '-framerate', str(args.fps), '-video_size', f'{width}x{height}', '-draw_mouse', '1']
    if args.window_id is not None:
        command += ['-window_id', str(args.window_id)]
    command += ['-i', os.environ['DISPLAY'], '-an', '-vf', 'pad=ceil(iw/2)*2:ceil(ih/2)*2',
                '-c:v', 'libx264', '-preset', 'veryfast', '-tune', 'zerolatency',
                '-pix_fmt', 'yuv420p', '-b:v', '8M', '-maxrate', '8M', '-bufsize', '8M',
                '-t', str(args.max_duration_ms / 1000), '-fs', str(threshold),
                '-movflags', '+faststart', '-f', 'mp4', str(args.out)]
    reason = None

    def requested_stop(_number, _frame):
        nonlocal reason
        reason = reason or 'stopped'

    for sig in [signal.SIGINT, signal.SIGTERM, signal.SIGHUP]:
        signal.signal(sig, requested_stop)
    parent = os.getppid()
    supervisor = os.getpid()
    # Even an uncatchable supervisor exit must stop its encoder. This process has one thread.
    libc = ctypes.CDLL(None, use_errno=True)

    def encoder_parent_death():
        if libc.prctl(1, signal.SIGINT, 0, 0, 0) != 0:  # PR_SET_PDEATHSIG
            os._exit(126)
        if os.getppid() != supervisor:
            os._exit(126)

    os.umask(0o077)
    with tempfile.TemporaryFile() as errors:
        child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=errors,
                                 preexec_fn=encoder_parent_death)
        first_frame = False
        started = time.monotonic()
        stop_at = None
        frames = 0
        buffer = b''
        try:
            with selectors.DefaultSelector() as poller:
                poller.register(child.stdout, selectors.EVENT_READ)
                while child.poll() is None:
                    now = time.monotonic()
                    if os.getppid() != parent:
                        reason = reason or 'owner-exited'
                    if not first_frame and now - started > 10:
                        reason = reason or 'startup-timeout'
                    if now - started > args.max_duration_ms / 1000 + 10:
                        reason = reason or 'duration-timeout'
                    if reason and stop_at is None:
                        child.send_signal(signal.SIGINT)
                        stop_at = now
                    if stop_at is not None and now - stop_at > 5:
                        child.kill()
                        raise RuntimeError('the encoder did not finalize within five seconds')
                    for key, _ in poller.select(.05):
                        chunk = os.read(key.fd, 65536)
                        if not chunk:
                            poller.unregister(key.fileobj)
                            continue
                        buffer += chunk
                        while b'\n' in buffer:
                            line, buffer = buffer.split(b'\n', 1)
                            if line.startswith(b'frame='):
                                frames = int(line.split(b'=', 1)[1])
                                if frames > 0 and not first_frame:
                                    first_frame = True
                                    publish(args.status, state='recording', encoderPid=child.pid,
                                            width=width + width % 2, height=height + height % 2, fps=args.fps)
            child.wait(timeout=1)
            errors.seek(0)
            error = errors.read(8192).decode('utf-8', errors='replace')
            if child.returncode not in (0, 255) or not first_frame:
                raise RuntimeError(f'encoder exited with {child.returncode}: {error}')
            probe = subprocess.run(['ffprobe', '-v', 'error', '-show_streams', '-show_format',
                                    '-of', 'json', str(args.out)], capture_output=True, text=True,
                                   check=True, timeout=10)
            info = json.loads(probe.stdout)
            duration = float(info['format']['duration'])
            if not any(s.get('codec_name') == 'h264' for s in info['streams']) or duration <= 0:
                raise RuntimeError('encoder produced no playable H.264 frames')
            size = args.out.stat().st_size
            if size > args.max_bytes:
                args.out.unlink()
                raise RuntimeError('finalized recording exceeded its file limit')
            if reason in ('startup-timeout', 'duration-timeout'):
                raise RuntimeError(reason)
            if reason is None:
                if duration >= args.max_duration_ms / 1000 - 1 / args.fps:
                    reason = 'duration-limit'
                elif size >= threshold:
                    reason = 'size-limit'
                else:
                    reason = 'source-ended'
            publish(args.status, state='completed', reason=reason, frames=frames,
                    durationMs=round(duration * 1000), bytes=size)
        finally:
            if child.poll() is None:
                child.send_signal(signal.SIGINT)
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    child.kill()
                    child.wait()
            child.stdout.close()


def main():
    args = options()
    try:
        run(args)
        return 0
    except Exception as error:
        publish(args.status, state='failed', error=str(error))
        print(str(error), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
