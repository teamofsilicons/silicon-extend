#!/usr/bin/env python3
"""Install a Linux package and verify real local-service recording upload/download.

Run on the host with Docker, the built extend CLI, ffmpeg and a development Extend
service with its local Silicon Accounts stand-in (e2e/dev.env; c:alice and her Silicon si:chef).
Only the disposable container is recorded. No source runtime is mounted into it. This is a manual
lane, not proof against production Silicon Accounts.

First it installs the package into a pristine debian:trixie with apt, once with Depends only and
once with Recommends (package-install-check.sh; needs network access), because an install into
the linux-e2e image, which already holds every -dev package, proves nothing about the package's
own dependency lines. Then it records through the service inside the linux-e2e image, which
build-image.sh builds (or rebuilds) when it is missing or was built from another Dockerfile.
Everything the lane prints is also kept in summary.txt beside its artifacts under
target/desktop/linux-recording/.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.request
import uuid

ROOT = Path(__file__).resolve().parents[3]
HARNESS = Path(__file__).parent.resolve()
DEFAULT_IMAGE = 'silicon-extend-linux-e2e'
parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
parser.add_argument('--package', required=True, type=Path)
parser.add_argument('--service-url', default='http://127.0.0.1:8480')
parser.add_argument('--container-service-url', default='http://host.docker.internal:8480')
parser.add_argument('--image', default=DEFAULT_IMAGE,
                    help='desktop image to record in (default: %(default)s, built by build-image.sh)')
parser.add_argument('--install-image', default='debian:trixie',
                    help='pristine image for the package install check (default: %(default)s)')
parser.add_argument('--skip-install-check', action='store_true',
                    help='skip the pristine install check (it needs network access for apt)')
args = parser.parse_args()
package = args.package.resolve(strict=True)
output_root = ROOT / 'target/desktop/linux-recording'
output_root.mkdir(parents=True, exist_ok=True)
work = Path(tempfile.mkdtemp(prefix='service-recording-', dir=output_root))
work.chmod(0o777)
summary = open(work / 'summary.txt', 'a', buffering=1)


def report(*parts):
    line = ' '.join(str(part) for part in parts)
    print(line, flush=True)
    summary.write(line + '\n')


report('Package:', package, 'sha256', hashlib.sha256(package.read_bytes()).hexdigest())
if args.skip_install_check:
    report('SKIPPED pristine install check (--skip-install-check); Depends and Recommends are unverified')
else:
    for phase in ['depends', 'recommends']:
        result = subprocess.run(['docker', 'run', '--rm', '-e', 'PHASE=' + phase,
                                 '-v', str(package) + ':/tmp/extend-package.deb:ro', '-v', str(HARNESS) + ':/harness:ro',
                                 args.install_image, 'bash', '/harness/package-install-check.sh'],
                                capture_output=True, text=True)
        (work / f'install-{phase}.log').write_text(result.stdout + result.stderr)
        if result.returncode != 0:
            report(f'FAILED pristine install check ({phase}); see', work / f'install-{phase}.log')
            raise SystemExit(result.stdout[-4000:] + result.stderr[-4000:])
        report(result.stdout.strip().splitlines()[-1])
if args.image == DEFAULT_IMAGE:
    subprocess.run(['bash', str(HARNESS / 'build-image.sh')], check=True)
launcher = work / 'extend-recording-fixture'
launcher.write_text('#!/bin/sh\nexec python3 /harness/record-fixture.py\n')
launcher.chmod(0o755)
container = 'extend-record-service-' + uuid.uuid4().hex[:12]
device = None
session = False
started_container = False
report('Artifacts:', work)


def cli(who, *command, expect_json=True, stdin=None):
    home = work / ('cli-' + who)
    home.mkdir(exist_ok=True)
    env = dict(os.environ, SILICON_HOME=str(home), EXTEND_API_URL=args.service_url,
               ACCOUNTS_URL=args.service_url + '/dev/accounts', EXTEND_TELEMETRY='off')
    cmd = [str(ROOT / 'target/debug/extend'), '--timeout', '90000', *command]
    if expect_json:
        cmd.append('--json')
    result = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=100, input=stdin)
    assert result.returncode == 0, (command, result.stderr)
    # `extend --json` prints the data itself on stdout, with no wrapper.
    return json.loads(result.stdout) if expect_json else result.stdout


def signin(who, account, custodian=None):
    """Sign in through the service's Silicon Accounts stand-in, with a short-lived token from it."""
    body = json.dumps({'type': 'slt', 'data': {'id': account, 'custodian': custodian}}).encode()
    request = urllib.request.Request(args.service_url + '/dev/accounts/slt', data=body,
                                     headers={'content-type': 'application/json'})
    with urllib.request.urlopen(request, timeout=30) as response:
        slt = json.load(response)['data']['slt']
    cli(who, 'login', '--slt-stdin', stdin=slt)


def remote(*command):
    response = cli('chef', *command)  # a device command prints its CommandResult
    assert response['ok'], response.get('error')
    return response


def wait_status(predicate):
    path = work / 'agent-home/.extend-agent/status.json'
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
    signin('alice', 'c:alice')
    signin('chef', 'si:chef', 'c:alice')
    subprocess.run([
        'docker', 'run', '-d', '--rm', '--init', '--name', container,
        '-e', 'EXTEND_API_URL=' + args.container_service_url,
        '-v', str(package) + ':/tmp/extend-package.deb:ro',
        '-v', str(HARNESS) + ':/harness:ro',
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
    remote('open', 'extend-recording-fixture')
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
        report('PASS installed Linux package:', scope, 'recording, relay, upload, full decode and identical repeat download')
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
