#!/usr/bin/env python3
"""Install a Linux package and verify real local-service recording upload/download.

Run on the host with Docker, the built bridge CLI, ffmpeg and a development Bridge
service using local IAM (c:alice/si:chef). Only the disposable container is recorded.
No source runtime is mounted into it. This is a manual lane, not production IAM proof.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--package', required=True, type=Path)
parser.add_argument('--service-url', default='http://127.0.0.1:8480')
parser.add_argument('--container-service-url', default='http://host.docker.internal:8480')
parser.add_argument('--image', default='silicon-bridge-linux-recording')
args = parser.parse_args()
package = args.package.resolve(strict=True)
output_root = ROOT / 'target/desktop/linux-recording'
output_root.mkdir(parents=True, exist_ok=True)
work = Path(tempfile.mkdtemp(prefix='service-recording-', dir=output_root))
work.chmod(0o777)
launcher = work / 'bridge-recording-fixture'
launcher.write_text('#!/bin/sh\nexec python3 /harness/record-fixture.py\n')
launcher.chmod(0o755)
container = 'bridge-record-service-' + uuid.uuid4().hex[:12]
device = None
session = False
started_container = False
print('Artifacts:', work, flush=True)


def cli(who, *command, expect_json=True):
    home = work / ('cli-' + who)
    home.mkdir(exist_ok=True)
    env = dict(os.environ, SILICON_HOME=str(home), BRIDGE_API_URL=args.service_url, BRIDGE_TELEMETRY='off')
    cmd = [str(ROOT / 'target/debug/bridge'), '--timeout', '90000', *command]
    if expect_json:
        cmd.append('--json')
    result = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=100)
    assert result.returncode == 0, (command, result.stderr)
    return json.loads(result.stdout)['data'] if expect_json else result.stdout


def remote(*command):
    response = cli('chef', *command)['result']
    assert response['ok'], response.get('error')
    return response


def wait_status(predicate):
    path = work / 'agent-home/.bridge-agent/status.json'
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        if path.exists():
            try:
                state = json.loads(path.read_text())
            except json.JSONDecodeError:
                state = {}
            if predicate(state):
                return state
        time.sleep(.2)
    raise AssertionError('agent did not reach expected state; inspect ' + str(work / 'agent.log'))


try:
    cli('alice', 'login', 'c:alice')
    cli('chef', 'login', 'si:chef')
    subprocess.run([
        'docker', 'run', '-d', '--rm', '--init', '--name', container,
        '-e', 'BRIDGE_API_URL=' + args.container_service_url,
        '-v', str(package) + ':/tmp/bridge-package.deb:ro',
        '-v', str(Path(__file__).parent.resolve()) + ':/harness:ro',
        '-v', str(work) + ':/tmp/out', args.image,
        'bash', '/harness/record-service-container.sh',
    ], check=True, stdout=subprocess.DEVNULL)
    started_container = True
    status = wait_status(lambda s: bool(s.get('pairing')))
    device = cli('alice', 'device', 'pair', status['pairing']['code'], '--name', container,
                 '--access', 'si:chef')['device_id']
    status = wait_status(lambda s: s.get('phase') == 'online')
    assert 'screen.record' in status['capabilities'], status['capabilities']
    cli('chef', 'session', 'new', device, '--connect', expect_json=False)
    session = True
    remote('open', 'bridge-recording-fixture')
    for scope in ['app', 'device']:
        remote('record', 'start', 'service-' + scope, '--scope', scope, '--fps', '12', '--hide-touches')
        time.sleep(2)
        output = work / (scope + '.mp4')
        result = remote('record', 'stop', '--out', str(output))
        assert len(result['files']) == 1, result
        assert output.is_file() and output.stat().st_size > 0
        subprocess.run(['ffmpeg', '-v', 'error', '-i', str(output), '-f', 'null', '-'], check=True)
        info = json.loads(subprocess.check_output(['ffprobe', '-v', 'error', '-show_streams', '-show_format', '-of', 'json', str(output)]))
        video = next(s for s in info['streams'] if s['codec_type'] == 'video')
        assert (video['width'], video['height']) == ((642, 482) if scope == 'app' else (1280, 800)), video
        hashes = subprocess.check_output(['ffmpeg', '-v', 'error', '-i', str(output), '-f', 'framemd5', '-'], text=True)
        assert len({line.split(',')[-1] for line in hashes.splitlines() if line and not line.startswith('#')}) > 1
        file_id = result['files'][0]['file_id']
        downloaded = work / (scope + '-again.mp4')
        cli('chef', 'file', 'get', file_id, '--out', str(downloaded))
        assert hashlib.sha256(output.read_bytes()).digest() == hashlib.sha256(downloaded.read_bytes()).digest()
        (work / (scope + '-result.json')).write_text(json.dumps(result, indent=2))
        print('PASS installed Linux package:', scope, 'recording, relay, upload, full decode and identical repeat download', flush=True)
finally:
    try:
        if session:
            cli('chef', 'session', 'end')
    finally:
        try:
            if device:
                cli('alice', 'device', 'rm', device, '--yes')
        finally:
            if started_container:
                subprocess.run(['docker', 'stop', '--time', '15', container], check=True, stdout=subprocess.DEVNULL)
