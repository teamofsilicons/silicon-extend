#!/usr/bin/env python3
"""Prove that a native X11 app recording excludes an overlapping owned peer.

RECORD_ISOLATION_EDGE=unmap, resize or remap (unmap and map again faster than one frame) must each
end the recording as source-ended, with no frame of the peer.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

root=Path('/src')
out=Path(tempfile.mkdtemp(prefix='isolated-recording-',dir='/tmp/out'))
fixture_script=root/'apps/desktop/linux-e2e/record-fixture.py'
worker=root/'vendor/extend-engine/linux/screen-record.py'
fixture=subprocess.Popen(['python3',str(fixture_script)])
peer=None
recorder=None

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
    xid=wait(lambda:window('Extend Recording Fixture'))
    output=out/'isolated.mp4'; status=out/'status.json'
    recorder=subprocess.Popen(['python3',str(worker),'--out',str(output),'--status',str(status),'--window-id',xid,'--fps','12'])
    wait(lambda:status.exists() and json.loads(status.read_text()).get('state')=='recording')
    time.sleep(.5)
    peer=subprocess.Popen(['python3',str(fixture_script),'--peer'])
    peer_id=wait(lambda:window('Extend Recording Peer'))
    subprocess.run(['xdotool','windowmove',xid,'0','0','windowmove',peer_id,'0','0','windowraise',peer_id],check=True)
    # A whole-screen capture must see the green peer, proving that the overlap actually exists.
    subprocess.run(['ffmpeg','-v','error','-f','x11grab','-video_size','641x481','-i',':99','-frames:v','1','-y',str(out/'covered.png')],check=True)
    covered=subprocess.check_output(['ffmpeg','-v','error','-i',str(out/'covered.png'),'-vf','scale=1:1','-f','rawvideo','-pix_fmt','rgb24','-'])
    assert covered[1]>200 and covered[2]<30,covered
    time.sleep(2)
    edge=os.environ.get('RECORD_ISOLATION_EDGE')
    if edge=='unmap':
        subprocess.run(['xdotool','windowunmap',xid],check=True)
    elif edge=='remap':
        # Faster than one frame: the X server reseeds the off-screen copy from the covering peer.
        subprocess.run(['xdotool','windowunmap',xid,'windowmap',xid],check=True)
    elif edge=='resize':
        subprocess.run(['xdotool','windowsize',xid,'680','520'],check=True)
    else:
        recorder.send_signal(signal.SIGTERM)
    assert recorder.wait(timeout=10)==0
    state=json.loads(status.read_text());assert state['state']=='completed',state
    assert state['reason']==('source-ended' if edge else 'stopped'),state
    raw=subprocess.check_output(['ffmpeg','-v','error','-i',str(output),'-vf','scale=1:1','-f','rawvideo','-pix_fmt','rgb24','-'])
    frames=[raw[i:i+3] for i in range(0,len(raw),3)]
    # Every frame, not just the last: a reseeded copy shows the peer until the app redraws.
    leaked=[i for i,pixel in enumerate(frames) if not (50<pixel[1]<110 and pixel[2]>125)]
    assert frames and not leaked, ('recording leaked the peer or lost the target', len(frames), [list(frames[i]) for i in leaked[:5]])
    subprocess.run(['ffmpeg','-v','error','-i',str(output),'-f','null','-'],check=True)
    print('PASS opaque peer covers desktop but app recording remains target-only:',out,flush=True)
finally:
    if recorder is not None and recorder.poll() is None:
        recorder.terminate();recorder.wait(timeout=10)
    if peer is not None:
        peer.terminate();peer.wait(timeout=5)
    fixture.terminate();fixture.wait(timeout=5)
