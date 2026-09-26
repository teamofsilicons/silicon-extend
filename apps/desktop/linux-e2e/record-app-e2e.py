#!/usr/bin/env python3
"""Public named-app recording and isolation on the owned X11 test desktop."""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

root=Path('/src')
work=Path(tempfile.mkdtemp(prefix='public-app-recording-',dir='/tmp/out'))
fixture_script=root/'apps/desktop/linux-e2e/record-fixture.py'
launcher=work/'extend-recording-fixture'
launcher.write_text('#!/bin/sh\nexec python3 /src/apps/desktop/linux-e2e/record-fixture.py\n')
launcher.chmod(0o755)
cli_path=str(root/'vendor/agent-device/bin/agent-device.mjs')
env=dict(os.environ,PATH=str(work)+':'+os.environ['PATH'],AGENT_DEVICE_STATE_DIR=str(work/'daemon'),EXTEND_RECORD_FIXTURE_PID_FILE=str(work/'fixture.pid'))
cli=['node',cli_path,'--state-dir',str(work/'daemon'),'--session','linux-app','--platform','linux']
peer=None
fixture_pid=None

def command(*args):
    result=subprocess.run([*cli,*args,'--json'],env=env,capture_output=True,text=True,timeout=60)
    print(' '.join(args),result.stdout.strip(),result.stderr.strip(),flush=True)
    assert result.returncode==0,result
    return json.loads(result.stdout)['data']

def wait(predicate):
    deadline=time.monotonic()+12
    while time.monotonic()<deadline:
        result=predicate()
        if result:return result
        time.sleep(.05)
    raise AssertionError('fixture did not become ready')

def window(title):
    result=subprocess.run(['xdotool','search','--name','^'+title+'$'],capture_output=True,text=True)
    return result.stdout.strip().splitlines()[0] if result.returncode==0 else None

try:
    command('open','extend-recording-fixture')
    wait(lambda:(work/'fixture.pid').exists())
    fixture_pid=int((work/'fixture.pid').read_text())
    xid=wait(lambda:window('Extend Recording Fixture'))
    peer=subprocess.Popen(['python3',str(fixture_script),'--peer'])
    peer_id=wait(lambda:window('Extend Recording Peer'))
    subprocess.run(['xdotool','windowmove',xid,'0','0','windowmove',peer_id,'0','0','windowraise',peer_id],check=True)
    output=work/'isolated.mp4'
    started=command('record','start',str(output),'--scope','app','--fps','12','--hide-touches')
    assert started['activeSessionApp']['bundleId']=='extend-recording-fixture',started
    # Start while the target is already covered; never refocus it to make capture pass.
    time.sleep(2)
    stopped=command('record','stop')
    assert stopped['recordingScope']=='app' and stopped['nativePathDisposition']=='retired',stopped
    pixels=subprocess.check_output(['ffmpeg','-v','error','-sseof','-0.2','-i',str(output),'-frames:v','1','-vf','scale=1:1','-f','rawvideo','-pix_fmt','rgb24','-'])
    assert len(pixels)==3 and 50<pixels[1]<110 and pixels[2]>125, ('public app capture lost its target',list(pixels))
    subprocess.run(['ffmpeg','-v','error','-i',str(output),'-f','null','-'],check=True)
    # The covering peer must still occupy the desktop after the entire public recording.
    desktop=subprocess.check_output(['ffmpeg','-v','error','-f','x11grab','-video_size','641x481','-i',':99','-frames:v','1','-vf','scale=1:1','-f','rawvideo','-pix_fmt','rgb24','-'])
    assert desktop[1]>200 and desktop[2]<30,('capture changed foreground or did not overlap',list(desktop))
    rejected=work/'missing.mp4'
    result=subprocess.run(['python3',str(root/'vendor/agent-device/linux/screen-record.py'),'--out',str(rejected),'--status',str(work/'missing.json'),'--app-id','no-such-extend-app'],capture_output=True,text=True)
    assert result.returncode!=0 and not rejected.exists(),result
    duplicate=subprocess.Popen(['python3',str(fixture_script)])
    try:
        time.sleep(.5)
        result=subprocess.run(['python3',str(root/'vendor/agent-device/linux/screen-record.py'),'--out',str(work/'ambiguous.mp4'),'--status',str(work/'ambiguous.json'),'--app-id','extend-recording-fixture'],capture_output=True,text=True)
        assert result.returncode!=0 and 'multiple mapped windows' in result.stderr and not (work/'ambiguous.mp4').exists(),result
    finally:
        duplicate.terminate();duplicate.wait(timeout=5)
    print('PASS public app recording: bound identity, initially covered target, no foreground change, full decode, missing-app refusal:',work,flush=True)
finally:
    try:
        subprocess.run(['node',cli_path,'daemon','stop','--state-dir',str(work/'daemon'),'--clean'],env=env,check=True,timeout=30)
    finally:
        if peer is not None:
            peer.terminate();peer.wait(timeout=5)
        if fixture_pid is None and (work/'fixture.pid').exists():
            fixture_pid=int((work/'fixture.pid').read_text())
        if fixture_pid is not None:
            proc=Path(f'/proc/{fixture_pid}')
            if proc.exists():
                assert str(fixture_script).encode() in (proc/'cmdline').read_bytes(), 'refuse to stop an unrelated PID'
                os.kill(fixture_pid,signal.SIGTERM)
