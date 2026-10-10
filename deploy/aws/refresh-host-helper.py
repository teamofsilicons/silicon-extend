#!/usr/bin/env python3
"""Replace one of the host's helper scripts with the version in this checkout's standalone.yaml.

    python3 deploy/aws/refresh-host-helper.py extend-render-env                      # show what would run
    python3 deploy/aws/refresh-host-helper.py extend-render-env --send --instance i-…  # run at cutover

The host's helpers (extend-render-env, extend-release, extend-db-bootstrap) are written once, by the
instance's first boot, from the UserData in standalone.yaml. Nothing rewrites them later: an image
release leaves them as they were, and a stack update must not try to (the instance has stop
protection, so CloudFormation can't stop it to apply new UserData, and a stop would change its public
address anyway). When a release changes what a helper does, run this from the release's checkout.

It sends one AWS-RunShellScript command over SSM that keeps the current helper beside it as
`<helper>.<UTC time>.bak`, writes the new one (decoded from base64, checked with `bash -n`, root-owned,
mode 0750) and prints its SHA-256, which must equal the one this script prints. No secret is part of
any helper, so the command text holds none. Without --send it prints the commands and sends nothing.
Rolling back is copying the .bak file over the helper.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import textwrap

TEMPLATE = Path(__file__).resolve().with_name("standalone.yaml")
HELPERS = ("extend-render-env", "extend-release", "extend-db-bootstrap")
CHUNK = 2000  # characters of base64 per command line, well inside SSM's limits


def helper_script(name, template=TEMPLATE):
    """The helper exactly as first boot writes it: the heredoc body in UserData, dedented."""
    if name not in HELPERS:
        raise SystemExit(f"unknown helper {name!r}; the host has {', '.join(HELPERS)}")
    text = template.read_text(encoding="utf-8")
    opening = f"cat > /usr/local/sbin/{name} <<'SCRIPT'\n"
    if opening.strip() not in text:
        raise SystemExit(f"{template} doesn't write /usr/local/sbin/{name}")
    body = text.split(opening, 1)[1]
    end = body.index("\n            SCRIPT\n")
    script = textwrap.dedent(body[:end]) + "\n"
    if not script.startswith("#!/bin/bash\n"):
        raise SystemExit(f"/usr/local/sbin/{name} in {template} doesn't start with #!/bin/bash")
    if "${" in script:
        # UserData goes through Fn::Sub: first boot got CloudFormation's value for a ${…}, which this
        # copy can't reproduce.
        raise SystemExit(f"/usr/local/sbin/{name} uses ${{…}}, which CloudFormation substitutes at first boot")
    return script


def remote_commands(name, script):
    encoded = base64.b64encode(script.encode("utf-8")).decode("ascii")
    path = f"/usr/local/sbin/{name}"
    commands = ["set -euo pipefail", f"helper={path}", 'stamp=$(date -u +%Y%m%dT%H%M%SZ)',
                'cp -p "$helper" "$helper.$stamp.bak"', ': > "$helper.b64"']
    commands += [f"printf '%s' '{encoded[i:i + CHUNK]}' >> \"$helper.b64\"" for i in range(0, len(encoded), CHUNK)]
    commands += ['base64 -d < "$helper.b64" > "$helper.new"', 'rm -f "$helper.b64"', 'bash -n "$helper.new"',
                 'chown root:root "$helper.new"', 'chmod 0750 "$helper.new"', 'mv "$helper.new" "$helper"',
                 'sha256sum "$helper"', 'echo "replaced $helper; the previous one is $helper.$stamp.bak"']
    return commands


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("helper", choices=HELPERS)
    parser.add_argument("--send", action="store_true", help="send the command over SSM (run at cutover)")
    parser.add_argument("--instance", help="the host's instance id (the stack's InstanceId output)")
    parser.add_argument("--print-script", action="store_true", help="print the helper itself and stop")
    args = parser.parse_args(argv)
    script = helper_script(args.helper)
    if args.print_script:
        sys.stdout.write(script)
        return 0
    digest = hashlib.sha256(script.encode("utf-8")).hexdigest()
    commands = remote_commands(args.helper, script)
    print(f"/usr/local/sbin/{args.helper} from {TEMPLATE.name}: {len(script)} bytes, sha256 {digest}")
    if not args.send:
        print("Would send AWS-RunShellScript with:")
        for command in commands:
            print("  " + (command if len(command) < 120 else command[:100] + "…'"))
        print("Nothing was sent. Add --send --instance <InstanceId> to run it (at cutover).")
        return 0
    if not args.instance:
        parser.error("--send needs --instance (the stack's InstanceId output)")
    if not shutil.which("aws"):
        raise SystemExit("the aws CLI isn't on PATH")
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as parameters:
        json.dump({"commands": commands}, parameters)
    result = subprocess.run(["aws", "ssm", "send-command", "--instance-ids", args.instance,
                             "--document-name", "AWS-RunShellScript",
                             "--comment", f"Refresh {args.helper} ({digest[:12]})",
                             "--parameters", f"file://{parameters.name}",
                             "--query", "Command.CommandId", "--output", "text"],
                            capture_output=True, text=True, check=False)
    Path(parameters.name).unlink()
    if result.returncode != 0:
        raise SystemExit(f"aws ssm send-command failed: {result.stderr.strip()}")
    command_id = result.stdout.strip()
    print(f"Sent: command {command_id}. Read its output with:\n  aws ssm get-command-invocation --instance-id "
          f"{args.instance} --command-id {command_id} --query StandardOutputContent --output text\n"
          f"The printed sha256 must be {digest}.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
