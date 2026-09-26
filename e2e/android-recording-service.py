#!/usr/bin/env python3
"""Record beyond 180 seconds through a paired emulator and the local development service.

Requires debug and test APKs, a connected on-device ADB client, local IAM users c:alice and
si:chef, a paired device granting si:chef access, the built Extend CLI, ffmpeg and ffprobe.
The Pixel/physical-device path is deliberately excluded from this manual emulator lane.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('device')
parser.add_argument('--serial', default='emulator-5554')
parser.add_argument('--service-url', default='http://127.0.0.1:8480')
args = parser.parse_args()
adb = ['adb', '-s', args.serial]
assert subprocess.check_output([*adb, 'shell', 'getprop', 'ro.kernel.qemu'], text=True).strip() == '1', 'Dedicated emulator required'
parent = ROOT / 'target/android-recording'
parent.mkdir(parents=True, exist_ok=True)
work = Path(tempfile.mkdtemp(prefix='service-', dir=parent))
print('Evidence:', work, flush=True)
session = None


def cli(who, *command, json_output=True):
    home = work / who
    home.mkdir(exist_ok=True)
    env = dict(os.environ, SILICON_HOME=str(home), EXTEND_API_URL=args.service_url, EXTEND_TELEMETRY='off')
    cmd = [str(ROOT / 'target/debug/extend'), '--timeout', '90000', *command]
    if json_output:
        cmd.append('--json')
    result = subprocess.run(cmd, env=env, text=True, capture_output=True, timeout=100)
    assert result.returncode == 0, (command, result.stderr)
    return json.loads(result.stdout)['data'] if json_output else result.stdout.strip()


def remote(*command):
    result = cli('chef', *command)['result']
    assert result['ok'], result.get('error')
    return result


try:
    cli('chef', 'login', 'si:chef')
    cli('alice', 'login', 'c:alice')
    session = cli('chef', 'session', 'new', args.device, '--connect', json_output=False)
    assert '2000' in remote('adb', 'shell', 'id -u')['text']
    subprocess.run([*adb, 'shell', 'am', 'start', '-n', 'com.teamofsilicons.extend.test/com.teamofsilicons.extend.RecordingFixtureActivity'], check=True, stdout=subprocess.DEVNULL)
    time.sleep(1)
    subprocess.run([*adb, 'shell', 'pidof', 'com.teamofsilicons.extend.test'], check=True, stdout=subprocess.DEVNULL)
    remote('record', 'start', 'long-service-proof', '--scope', 'device')
    print('Recording started; waiting 187 seconds to cross the native cap.', flush=True)
    time.sleep(187)
    video = work / 'recording.mp4'
    result = remote('record', 'stop', '--out', str(video))
    assert len(result['files']) == 1, result
    (work / 'result.json').write_text(json.dumps(result, indent=2))
    subprocess.run(['ffmpeg', '-v', 'error', '-xerror', '-i', str(video), '-enc_time_base', 'demux', '-fps_mode', 'passthrough', '-f', 'null', '-'], check=True)
    packets = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-select_streams', 'v', '-show_packets', '-of', 'json', str(video)]))['packets']
    assert all(int(a['dts']) < int(b['dts']) for a, b in zip(packets, packets[1:]))
    assert len(packets) > 500, 'Animated fixture was not captured'
    assert len([p for p in packets if float(p['pts_time']) > 181]) > 10, 'Recording contains no late animation'
    duration = float(json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_format', '-of', 'json', str(video)]))['format']['duration'])
    assert 181 < duration < 190, ('Recording timeline differs from the capture window', duration)
    file_id = result['files'][0]['file_id']
    digest = hashlib.sha256(video.read_bytes()).hexdigest()
    for who in ['chef', 'alice']:
        copied = work / (who + '-download.mp4')
        cli(who, 'file', 'get', file_id, '--out', str(copied))
        assert hashlib.sha256(copied.read_bytes()).hexdigest() == digest
    facts = {'session_id': session, 'device_id': args.device, 'bytes': video.stat().st_size,
             'sha256': digest, 'last_frame_seconds': packets[-1]['pts_time'], 'packets': len(packets), 'duration_seconds': duration}
    (work / 'verification.json').write_text(json.dumps(facts, indent=2))
    print('PASS segmented capture, upload, full decode, late frames and identical Silicon/Carbon downloads:', json.dumps(facts), flush=True)
finally:
    try:
        if session:
            cli('chef', 'session', 'end', session)
    finally:
        subprocess.run([*adb, 'shell', 'am', 'force-stop', 'com.teamofsilicons.extend.test'], check=True)
