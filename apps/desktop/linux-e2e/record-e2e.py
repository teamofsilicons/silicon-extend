#!/usr/bin/env python3
"""Native recording tests, run on the isolated Linux desktop by record-e2e.sh."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import tempfile

root = Path('/src')
worker = root / 'vendor/agent-device/linux/screen-record.py'
out = Path(tempfile.mkdtemp(prefix='recording-', dir='/tmp/out'))
print('Artifacts:', out, flush=True)

def wait(predicate, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.05)
    raise AssertionError('condition not reached')

fixture = subprocess.Popen(['python3', str(root/'apps/desktop/linux-e2e/record-fixture.py'), '--noise'])
try:
    def find_window():
        result = subprocess.run(['xdotool', 'search', '--name', '^Extend Recording Fixture$'],capture_output=True,text=True)
        return result.stdout.strip().splitlines()[0] if result.returncode == 0 else None
    xid = wait(find_window)
    for mode in ['manual', 'duration-limit', 'size-limit', 'owner-exited', 'device', 'worker-killed']:
        output = out / f'{mode}.mp4'
        status = out / f'{mode}.status.json'
        command = ['python3', str(worker), '--out', str(output), '--status', str(status), '--fps', '12']
        if mode != 'device':
            command += ['--window-id', xid]
        command += ['--max-duration-ms', '1800' if mode in ('duration-limit', 'device') else '15000']
        if mode == 'size-limit':
            command += ['--max-bytes', str(1024 ** 2)]
        if mode == 'owner-exited':
            owner = """import json,os,pathlib,subprocess,sys,time
status=pathlib.Path(sys.argv[1]); child=subprocess.Popen(sys.argv[2:]); deadline=time.monotonic()+10
while time.monotonic()<deadline:
    if status.exists() and json.loads(status.read_text()).get('state')=='recording':
        time.sleep(.5);os._exit(0)
    if child.poll() is not None: raise RuntimeError('worker exited before readiness')
    time.sleep(.05)
child.terminate();child.wait();raise RuntimeError('startup timeout')
"""
            command = ['python3', '-c', owner, str(status), *command]
        child = subprocess.Popen(command)
        try:
            def state():
                return json.loads(status.read_text()) if status.exists() else {}
            wait(lambda: state().get('state') in ('recording', 'completed', 'failed'))
            if mode == 'worker-killed':
                identity = state()
                assert identity['state'] == 'recording', identity
                time.sleep(.5)
                child.kill()
                assert child.wait(timeout=5) == -signal.SIGKILL
                encoder = Path(f"/proc/{identity['encoderPid']}/stat")
                wait(lambda: not encoder.exists() or encoder.read_text().split(') ')[1].startswith('Z'))
                subprocess.run(['ffmpeg','-v','error','-i',str(output),'-f','null','-'],check=True)
                print('PASS supervisor SIGKILL stops its encoder and preserves a decodable MP4',flush=True)
                continue
            if mode == 'manual':
                assert state()['state'] == 'recording', state()
                time.sleep(.5)
                child.send_signal(signal.SIGTERM)
            wait(lambda: state().get('state') in ('completed', 'failed'), timeout=25)
            complete = state()
            assert complete['state'] == 'completed', complete
            expected = 'stopped' if mode == 'manual' else 'duration-limit' if mode == 'device' else mode
            assert complete['reason'] == expected, complete
            assert child.wait(timeout=5) == 0
            info = json.loads(subprocess.check_output(['ffprobe','-v','error','-show_streams','-show_format','-of','json',str(output)]))
            video = next(s for s in info['streams'] if s.get('codec_type') == 'video')
            assert video['codec_name'] == 'h264'
            assert (video['width'], video['height']) == ((1280, 800) if mode == 'device' else (642, 482)), video
            assert float(info['format']['duration']) > 0
            if mode in ('duration-limit', 'device'):
                assert 1.7 <= float(info['format']['duration']) <= 1.9
            if mode == 'size-limit':
                assert output.stat().st_size <= 1024 ** 2
            subprocess.run(['ffmpeg','-v','error','-i',str(output),'-f','null','-'],check=True)
            pixels = subprocess.check_output(['ffmpeg','-v','error','-i',str(output),'-frames:v','1','-f','rawvideo','-pix_fmt','rgb24','-'])
            assert pixels and sum(value > 24 for value in pixels) > len(pixels) * .05, 'blank recording'
            print('PASS', mode, complete, flush=True)
        finally:
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=10)
    # The daemon can die in the moment between spawning the worker and the worker's first look at
    # its parent. The worker is told its owner, so it never adopts the reaper as its owner.
    owner = """import os,subprocess,sys
child=subprocess.Popen([*sys.argv[1:],'--owner-pid',str(os.getpid())]);print(child.pid,flush=True);os._exit(0)
"""
    output, status = out / 'owner-gone.mp4', out / 'owner-gone.status.json'
    spawned = subprocess.run(['python3', '-c', owner, 'python3', str(worker), '--out', str(output), '--status', str(status),
                              '--window-id', xid, '--fps', '12', '--max-duration-ms', '15000'], capture_output=True, text=True, check=True)
    orphan = Path(f'/proc/{int(spawned.stdout)}/stat')
    wait(lambda: not orphan.exists() or orphan.read_text().split(') ')[1].startswith('Z'), timeout=5)
    time.sleep(.5)
    assert not output.exists() and not status.exists(), 'an orphaned worker recorded or published'
    print('PASS a worker whose owner died before it started exits without recording or publishing', flush=True)

    # Capture that cannot keep up with --fps must not speed the video up (frames used to be stamped
    # by count, so a 6 s recording at half speed played back in 3 s).
    large = subprocess.Popen(['python3', str(root/'apps/desktop/linux-e2e/record-fixture.py'), '--size', '3000x2000', '--static'])
    try:
        def find_large():
            result = subprocess.run(['xdotool', 'search', '--name', '^Extend Recording Fixture$'], capture_output=True, text=True)
            ids = [value for value in result.stdout.split() if value != xid]
            return ids[0] if ids else None
        large_id = wait(find_large)
        output, status = out / 'slow-capture.mp4', out / 'slow-capture.status.json'
        child = subprocess.Popen(['python3', str(worker), '--out', str(output), '--status', str(status), '--window-id', large_id,
                                  '--fps', '60', '--max-duration-ms', '15000'])
        wait(lambda: status.exists() and json.loads(status.read_text()).get('state') == 'recording')
        began = time.monotonic()
        time.sleep(5)
        child.send_signal(signal.SIGTERM)
        ended = time.monotonic()
        assert child.wait(timeout=20) == 0
        complete = json.loads(status.read_text())
        info = json.loads(subprocess.check_output(['ffprobe','-v','error','-count_frames','-show_streams','-show_format','-of','json',str(output)]))
        video = next(s for s in info['streams'] if s.get('codec_type') == 'video')
        duration, wall = float(info['format']['duration']), ended - began
        assert (video['width'], video['height']) == (3000, 2000), video
        assert video['r_frame_rate'] == '60/1', video
        assert wall * .9 <= duration <= wall + 1, ('video time does not follow wall time', duration, wall, complete)
        print(f'PASS slow capture keeps real time: {wall:.2f} s recorded, {duration:.2f} s of video, '
              f'{video["nb_read_frames"]} frames at a constant 60 fps', flush=True)
    finally:
        large.terminate()
        large.wait(timeout=5)

    # `open` returns before the app's window is mapped; a recording started right away waits for it.
    output, status = out / 'app-just-opened.mp4', out / 'app-just-opened.status.json'
    opened = subprocess.Popen(['python3', str(root/'apps/desktop/linux-e2e/record-fixture.py'), '--peer'])
    try:
        recorder = subprocess.run(['python3', str(worker), '--out', str(output), '--status', str(status), '--app-id',
                                   'extend-recording-peer', '--fps', '12', '--max-duration-ms', '1500'],
                                  capture_output=True, text=True, timeout=30)
        assert recorder.returncode == 0, recorder.stderr
        assert json.loads(status.read_text())['reason'] == 'duration-limit', status.read_text()
        print('PASS app recording started before the app mapped its window waits for the window', flush=True)
    finally:
        opened.terminate()
        opened.wait(timeout=5)

    # Invalid input must not truncate an existing artifact or leave a recorder behind.
    kept = out / 'manual.mp4'
    original = kept.read_bytes()
    rejected = subprocess.run(['python3',str(worker),'--out',str(kept),'--status',str(out/'invalid.json')],capture_output=True)
    assert rejected.returncode != 0 and kept.read_bytes() == original
    for arguments, env in [(['--fps','0'],os.environ), (['--max-bytes',str(2*1024**3)],os.environ), ([],dict(os.environ,WAYLAND_DISPLAY='wayland-0')), ([],{key: value for key,value in os.environ.items() if key != 'DISPLAY'})]:
        output = out/'rejected.mp4'; status=out/'rejected.json'
        result=subprocess.run(['python3',str(worker),'--out',str(output),'--status',str(status),*arguments],env=env,capture_output=True)
        assert result.returncode != 0 and not output.exists() and not status.exists()
    print('PASS invalid arguments, existing output protection, headless refusal, and Wayland refusal',flush=True)
finally:
    fixture.terminate()
    fixture.wait(timeout=5)
