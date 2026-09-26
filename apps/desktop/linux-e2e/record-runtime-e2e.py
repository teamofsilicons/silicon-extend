#!/usr/bin/env python3
"""Public agent-device recording, running against a real isolated X11 desktop."""
import json
import os
from pathlib import Path
import subprocess
import signal
import tempfile
import time

root = Path('/src')
work = Path(tempfile.mkdtemp(prefix='runtime-recording-', dir='/tmp/out'))
print('Artifacts:', work, flush=True)
env = dict(os.environ, AGENT_DEVICE_STATE_DIR=str(work/'daemon'))
cli = ['node', str(root/'vendor/agent-device/bin/agent-device.mjs'), '--state-dir', str(work/'daemon'), '--session', 'linux-recording', '--platform', 'linux']
fixture = subprocess.Popen(['python3', str(root/'apps/desktop/linux-e2e/record-fixture.py')])

def command(*args):
    result = subprocess.run([*cli,*args,'--json'], env=env,capture_output=True,text=True,timeout=60)
    print(' '.join(args), result.stdout.strip(), result.stderr.strip(), flush=True)
    assert result.returncode == 0, result
    return json.loads(result.stdout)

try:
    time.sleep(.5)
    output = work/'public.mp4'
    start = command('record','start',str(output),'--scope','device','--fps','12','--hide-touches')
    time.sleep(1)
    stopped = command('record','stop')
    assert output.is_file(), stopped
    info = json.loads(subprocess.check_output(['ffprobe','-v','error','-show_streams','-show_format','-of','json',str(output)]))
    video=next(s for s in info['streams'] if s.get('codec_type')=='video')
    assert (video['width'],video['height']) == (1280,800),video
    subprocess.run(['ffmpeg','-v','error','-i',str(output),'-f','null','-'],check=True)
    assert not output.with_name('public.native.mp4').exists(), 'native recording was not retired after export'
    print('PASS public Linux record start/stop, dimensions, full decode and native artifact retirement',flush=True)
    recovered = work/'recovered.mp4'
    command('record','start',str(recovered),'--scope','system','--fps','12')
    time.sleep(1)
    identity = json.loads((work/'daemon/daemon.json').read_text())
    pid = identity['pid']
    proc = Path(f'/proc/{pid}')
    process_env = (proc/'environ').read_bytes().split(b'\0')
    assert ('AGENT_DEVICE_STATE_DIR=' + str(work/'daemon')).encode() in process_env
    assert b'agent-device' in (proc/'cmdline').read_bytes(), 'refuse to kill an unrelated PID'
    os.kill(pid, signal.SIGKILL)
    time.sleep(1)
    stopped = command('record','stop')
    assert recovered.is_file(), stopped
    data = stopped['data']
    assert data['nativePathDisposition'] == 'retired', data
    assert 'overlayWarning' in data and 'restart' in data['overlayWarning'], data
    subprocess.run(['ffmpeg','-v','error','-i',str(recovered),'-f','null','-'],check=True)
    print('PASS public stop recovers after daemon SIGKILL and discloses lost touch events',flush=True)
    if os.environ.get('BRIDGE_RECORD_DRIVER'):
        agent = os.environ['BRIDGE_RECORD_DRIVER']
        driver_env = dict(os.environ, SILICON_HOME=str(work/'bridge-home'), BRIDGE_AGENT_DEVICE=str(root/'vendor/agent-device/bin/agent-device.mjs'), BRIDGE_TELEMETRY='off')
        probe = subprocess.run([agent,'probe','--json'],env=driver_env,capture_output=True,text=True,check=True,timeout=30)
        report = json.loads(probe.stdout)
        assert 'screen.record' in report['capabilities'], report
        def driver(*args):
            result=subprocess.run([agent,'exec','--session','abc','--timeout-ms','60000','--out',str(work/'driver-output'),*args],env=driver_env,capture_output=True,text=True,timeout=65)
            print('Bridge driver:', result.stdout.strip(),result.stderr.strip(),flush=True)
            assert result.returncode == 0, result.stderr
            data=json.loads(result.stdout)
            assert result.returncode==0 and data['ok'],data
            return data
        try:
            driver('record','start','driver','--scope','device','--fps','12','--hide-touches')
            time.sleep(1)
            result=driver('record','stop')
            assert len(result['files'])==1,result
            video=Path(result['files'][0]['path'])
            assert video.is_file(),result
            subprocess.run(['ffmpeg','-v','error','-i',str(video),'-f','null','-'],check=True)
            assert Path(result['output']['outPath']).is_file(), 'stop moved the committed export away from its durable manifest'
            print('PASS Bridge driver probe, recording start/stop, returned artifact and full decode',flush=True)
        finally:
            subprocess.run(['node',str(root/'vendor/agent-device/bin/agent-device.mjs'),'daemon','stop','--state-dir',str(work/'bridge-home/.bridge-agent/agent-device'),'--clean'],env=driver_env,check=True,timeout=30)


finally:
    try:
        subprocess.run(['node', str(root/'vendor/agent-device/bin/agent-device.mjs'), 'daemon','stop','--state-dir',str(work/'daemon'),'--clean'],env=env,check=True,timeout=30)
    finally:
        fixture.terminate()
        fixture.wait(timeout=5)
