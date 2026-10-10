#!/usr/bin/env python3
"""Run Extend on this machine against a local Silicon Accounts stack.

    EXTEND_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh [--build]   # up (idempotent)
    EXTEND_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts-stop.sh [--drop]
    python3 scripts/dev_accounts.py restart     # restart extend-service (same data, same secrets)
    python3 scripts/dev_accounts.py status      # what runs, as JSON (never a secret)

`up` starts, when not already running:
  - the database extend_e2e on the local PostgreSQL (created when missing), migrated by `extend-service migrate`;
  - a Briefcase stand-in on 127.0.0.1:<base+2> (scripts/briefcase_stub.py) that answers the delegated routes Extend
    stores and reads files with, and verifies every proof Extend sends at Silicon Accounts with Briefcase's own app
    credentials (every call is logged, without tokens, to <dir>/briefcase-calls.jsonl);
  - extend-service on 127.0.0.1:<base+1>: Silicon Accounts sign-in (EXTEND_ACCOUNTS_MODE=sdk with Extend's app
    secret), files in the Briefcase stand-in, notifications through Ting off (EXTEND_TING_URL unset, as at cutover);
and points Extend's app webhook at Silicon Accounts to http://127.0.0.1:<base+1>/webhooks/accounts with Extend's own
app credentials (PUT /v1/apps/extend/webhook, every update). What was there before is kept in
<dir>/webhook-previous.json and put back by `down` (a webhook that had no URL is deleted again). The signing secret
reaches the service only through its environment: Silicon Accounts shows it once (on the first PUT, or from
POST /v1/apps/extend/webhook/generate-secret), and it is kept in <dir>/webhook-secret, mode 0600, never in git. After
the service (re)starts, a test ping proves that deliveries arrive and verify; a secret that no longer matches is
replaced and the service restarted. The proof refresh tokens Extend keeps are sealed with a development key made once
in <dir>/delegation-key (0600).

Configuration (environment):
  EXTEND_TEST_STACK    JSON file describing the stack (the migration testkit's shape): accounts_public_url,
                       accounts_api_url, apps.extend.app_secret, apps.briefcase.app_secret
  ACCOUNTS_URL         Silicon Accounts public URL, the token issuer (default: stack file, else http://localhost:9590)
  ACCOUNTS_API_URL     where Extend calls Silicon Accounts (default: stack file, else ACCOUNTS_URL)
  EXTEND_APP_SECRET    extend's app secret at that stack (default: stack file)
  EXTEND_DEV_BRIEFCASE_SECRET  briefcase's app secret there, for the stand-in's proof checks (default: stack file)
  EXTEND_DEV_BASE      port block base (default 4220: website 4220, service 4221, Briefcase stand-in 4222)
  EXTEND_DEV_PG        PostgreSQL server URL without a database (default postgres://postgres@127.0.0.1:5460)
  EXTEND_DEV_DB        database name (default extend_e2e)
  EXTEND_DEV_DIR       state directory (default .mig/dev-accounts): logs, secrets, service data, the stand-in's log
  EXTEND_DEV_PIDS      pid directory (default .mig/pids)
  EXTEND_BIN_DIR       where extend-service and extend are (default $CARGO_TARGET_DIR/debug, else target/debug)
  EXTEND_DEV_FILES     briefcase (default: the stand-in) or local (files on disk, no proofs)
  PSQL                 psql executable (default: PATH, then Homebrew's postgresql@16/@17)

Only loopback Silicon Accounts URLs are accepted: this script never talks to a deployed Silicon Accounts. Nothing it
prints contains a secret.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlparse
from urllib.request import ProxyHandler, Request, build_opener
import uuid

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "extend"
WEBHOOK_PATH = "/webhooks/accounts"
LOOPBACK = {"localhost", "127.0.0.1", "::1"}
OPENER = build_opener(ProxyHandler({}))
SERVICES = ("extend-briefcase-stub", "extend-service")


class Failure(Exception):
    """A precise, user-facing reason the command cannot continue."""


def http(method, url, body=None, basic=None, bearer=None, headers=None, timeout=15, raw_body=None):
    """One JSON request; returns (status, parsed body or None). Network errors raise Failure."""
    options = {"Accept": "application/json", **(headers or {})}
    data = raw_body
    if body is not None:
        data = json.dumps(body).encode()
        options["Content-Type"] = "application/json"
    if basic:
        options["Authorization"] = "Basic " + base64.b64encode(":".join(basic).encode()).decode()
    if bearer:
        options["Authorization"] = "Bearer " + bearer
    try:
        with OPENER.open(Request(url, data=data, method=method, headers=options), timeout=timeout) as response:
            status, raw = response.status, response.read()
    except HTTPError as error:
        with error:
            status, raw = error.code, error.read()
    except (URLError, OSError) as error:
        raise Failure(f"{method} {url} failed: {getattr(error, 'reason', error)}") from None
    try:
        return status, json.loads(raw) if raw else None
    except ValueError:
        return status, {"raw": raw[:300].decode(errors="replace")}


def error_code(body):
    if not isinstance(body, dict):
        return None
    err = body.get("error")
    if isinstance(err, dict):
        return err.get("code")
    data = body.get("data")
    if body.get("type") == "error" and isinstance(data, dict):
        return data.get("code")
    return err if isinstance(err, str) else None


def port_open(port, host="127.0.0.1"):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.settimeout(0.3)
        return sock.connect_ex((host, port)) == 0


def write_private(path, text):
    """Writes a file only its owner can read (secrets, keys)."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as handle:
        handle.write(text)
    os.chmod(path, 0o600)


def read_stack(environ):
    path = environ.get("EXTEND_TEST_STACK")
    if not path:
        return {}
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError) as error:
        raise Failure(f"EXTEND_TEST_STACK={path} cannot be read as JSON: {error}") from None


class Config:
    """Everything `up`, `down`, `restart` and `status` need, read once from the environment."""

    def __init__(self, environ=os.environ):
        stack = read_stack(environ)
        apps = stack.get("apps") or {}
        self.accounts_url = (environ.get("ACCOUNTS_URL") or stack.get("accounts_public_url")
                             or "http://localhost:9590").rstrip("/")
        self.accounts_api_url = (environ.get("ACCOUNTS_API_URL") or stack.get("accounts_api_url")
                                 or self.accounts_url).rstrip("/")
        for name, url in (("ACCOUNTS_URL", self.accounts_url), ("ACCOUNTS_API_URL", self.accounts_api_url)):
            if urlparse(url).hostname not in LOOPBACK:
                raise Failure(f"{name}={url} is not on this machine; this script only runs against a local stack.")
        self.app_secret = environ.get("EXTEND_APP_SECRET") or (apps.get(APP_ID) or {}).get("app_secret") or ""
        if not self.app_secret:
            raise Failure("Extend's app secret is unknown: set EXTEND_TEST_STACK to the stack file, or EXTEND_APP_SECRET.")
        self.briefcase_secret = (environ.get("EXTEND_DEV_BRIEFCASE_SECRET")
                                 or (apps.get("briefcase") or {}).get("app_secret") or "")
        self.files = environ.get("EXTEND_DEV_FILES", "briefcase")
        if self.files not in ("briefcase", "local"):
            raise Failure(f"EXTEND_DEV_FILES must be briefcase or local, got {self.files!r}.")
        if self.files == "briefcase" and not self.briefcase_secret:
            raise Failure("The Briefcase stand-in needs Briefcase's app secret at the stack to verify proofs: set "
                          "EXTEND_TEST_STACK or EXTEND_DEV_BRIEFCASE_SECRET (or EXTEND_DEV_FILES=local).")
        self.base = int(environ.get("EXTEND_DEV_BASE", "4220"))
        self.web_port, self.api_port, self.briefcase_port = self.base, self.base + 1, self.base + 2
        self.api_url = f"http://127.0.0.1:{self.api_port}"
        self.webhook_url = f"{self.api_url}{WEBHOOK_PATH}"
        self.briefcase_url = f"http://127.0.0.1:{self.briefcase_port}"
        self.pg = environ.get("EXTEND_DEV_PG", "postgres://postgres@127.0.0.1:5460").rstrip("/")
        self.db = environ.get("EXTEND_DEV_DB", "extend_e2e")
        self.dir = Path(environ.get("EXTEND_DEV_DIR", ROOT / ".mig" / "dev-accounts")).resolve()
        self.pids = Path(environ.get("EXTEND_DEV_PIDS", ROOT / ".mig" / "pids")).resolve()
        target = environ.get("CARGO_TARGET_DIR")
        default_bin = (Path(target) if target else ROOT / "target") / "debug"
        self.bin_dir = Path(environ.get("EXTEND_BIN_DIR", default_bin)).resolve()
        self.psql = environ.get("PSQL") or shutil.which("psql") or next(
            (p for p in ("/opt/homebrew/opt/postgresql@16/bin/psql", "/opt/homebrew/opt/postgresql@17/bin/psql",
                         "/usr/local/opt/postgresql@16/bin/psql") if Path(p).exists()), "psql")
        self.log_filter = environ.get("EXTEND_LOG", "info,sqlx=warn,tower_http=warn")

    @property
    def basic(self):
        return (APP_ID, self.app_secret)

    @property
    def database_url(self):
        return f"{self.pg}/{self.db}"

    @property
    def briefcase_log(self):
        return self.dir / "briefcase-calls.jsonl"

    def binary(self, name):
        path = self.bin_dir / name
        if not path.exists():
            raise Failure(f"{path} does not exist: build it (scripts/dev-accounts.sh --build) or set EXTEND_BIN_DIR.")
        return str(path)

    def webhook_secret(self):
        """The secret Silicon Accounts signs Extend's deliveries with, as far as this machine knows."""
        stored = self.dir / "webhook-secret"
        return stored.read_text().strip() if stored.exists() else ""

    def delegation_key(self):
        """A development EXTEND_DELEGATION_ENCRYPTION_KEY, made once and kept (it seals proof refresh tokens)."""
        path = self.dir / "delegation-key"
        if not path.exists():
            write_private(path, base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("="))
        return path.read_text().strip()

    def service_env(self):
        """The whole environment of extend-service: nothing else from this shell leaks in."""
        env = {key: os.environ[key] for key in ("PATH", "HOME", "TMPDIR", "LANG") if key in os.environ}
        env.update({
            "EXTEND_ENVIRONMENT": "development",
            "EXTEND_DATABASE_URL": self.database_url,
            "EXTEND_BIND": f"127.0.0.1:{self.api_port}",
            "EXTEND_PUBLIC_URL": self.api_url,
            "EXTEND_DATA_DIR": str(self.dir / "data"),
            "EXTEND_WEBSITE_URL": f"http://localhost:{self.web_port}",
            "EXTEND_DOCS_URL": f"http://localhost:{self.web_port}/docs",
            "EXTEND_LOG": self.log_filter,
            "NO_COLOR": "1",
            "ACCOUNTS_URL": self.accounts_url,
            "ACCOUNTS_API_URL": self.accounts_api_url,
            "EXTEND_ACCOUNTS_MODE": "sdk",
            "EXTEND_APP_ID": APP_ID,
            "EXTEND_APP_SECRET": self.app_secret,
            "EXTEND_ACCOUNTS_WEBHOOK_SECRET": self.webhook_secret(),
            "EXTEND_DELEGATION_ENCRYPTION_KEY": self.delegation_key(),
            "EXTEND_FILES_MODE": self.files,
            # Notifications through Ting stay off, as at cutover (EXTEND_TING_URL unset).
            "EXTEND_TING_MODE": "off",
        })
        if self.files == "briefcase":
            env["EXTEND_BRIEFCASE_URL"] = self.briefcase_url
            env["EXTEND_BRIEFCASE_WEB_URL"] = f"http://localhost:{self.briefcase_port}"
        return env


# --- processes ---------------------------------------------------------------------------------------------------

MARKERS = {"extend-briefcase-stub": "briefcase_stub.py", "extend-service": "extend-service"}


def read_pid(cfg, name):
    try:
        return int((cfg.pids / name).read_text().strip())
    except (OSError, ValueError):
        return None


def command_of(pid):
    result = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True, check=False)
    return result.stdout.strip()


def running(cfg, name):
    """The pid of `name` when the recorded process is alive and still that program (pids get reused)."""
    pid = read_pid(cfg, name)
    if not pid:
        return None
    return pid if MARKERS[name] in command_of(pid) else None


def start(cfg, name, argv, env, port):
    """Starts one detached process (its own session, output to <dir>/logs/<name>.log) and records its pid."""
    if port_open(port):
        raise Failure(f"127.0.0.1:{port} is taken by another process; {name} cannot listen there "
                      f"(lsof -nP -iTCP:{port} -sTCP:LISTEN shows which).")
    logs = cfg.dir / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    with open(logs / f"{name}.log", "ab") as log, open(os.devnull, "rb") as devnull:
        process = subprocess.Popen(argv, stdin=devnull, stdout=log, stderr=subprocess.STDOUT, env=env,
                                   cwd=cfg.dir, start_new_session=True)
    cfg.pids.mkdir(parents=True, exist_ok=True)
    (cfg.pids / name).write_text(f"{process.pid}\n")
    return process.pid


def stop(cfg, name, grace=10.0):
    """Stops `name` (TERM, then KILL after `grace` seconds) and forgets its pid. Returns what it did."""
    pid = running(cfg, name)
    (cfg.pids / name).unlink(missing_ok=True)
    if not pid:
        return "not running"
    try:
        os.kill(pid, signal.SIGTERM)
        deadline = time.time() + grace
        while time.time() < deadline:
            if not command_of(pid):
                return f"stopped (pid {pid})"
            time.sleep(0.2)
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        return f"stopped (pid {pid})"
    return f"killed after {grace:.0f}s (pid {pid})"


def wait_until(probe, seconds, what, log=None, alive=None):
    """Waits for probe(); fails at once, with the log's last lines, when `alive` says the process exited."""
    deadline = time.time() + seconds
    reason = f"did not come up within {seconds}s"
    while time.time() < deadline:
        if probe():
            return
        if alive and not alive():
            reason = "exited while starting"
            break
        time.sleep(0.3)
    tail = ""
    if log and log.exists():
        tail = "\n  last lines of " + str(log) + ":\n    " + "\n    ".join(log.read_text(errors="replace").splitlines()[-15:])
    raise Failure(f"{what} {reason}.{tail}")


def ready(url):
    """Whether `url` answers 2xx (extend-service's /ready is 204, the stand-in's 200)."""
    try:
        status, _ = http("GET", url, timeout=2)
    except Failure:
        return False
    return 200 <= status < 300


# --- the database ------------------------------------------------------------------------------------------------

def psql(cfg, sql, database=None):
    database = database or "postgres"
    try:
        result = subprocess.run([cfg.psql, f"{cfg.pg}/{database}", "-v", "ON_ERROR_STOP=1", "-At", "-c", sql],
                                capture_output=True, text=True, check=False, timeout=60)
    except FileNotFoundError:
        raise Failure(f"psql was not found at {cfg.psql}; install PostgreSQL's client or set PSQL.") from None
    if result.returncode != 0:
        raise Failure(f"psql on {cfg.pg}/{database} failed: {result.stderr.strip()}")
    return result.stdout.strip()


def check_db_name(name):
    if not name.replace("_", "").isalnum() or not name.startswith(APP_ID):
        raise Failure(f"database name {name!r} must start with '{APP_ID}' and use letters, digits and _ only.")


def ensure_database(cfg):
    check_db_name(cfg.db)
    if psql(cfg, f"SELECT 1 FROM pg_database WHERE datname = '{cfg.db}'") != "1":
        psql(cfg, f'CREATE DATABASE "{cfg.db}"')
        return [cfg.db]
    return []


def drop_database(cfg):
    check_db_name(cfg.db)
    if psql(cfg, f"SELECT 1 FROM pg_database WHERE datname = '{cfg.db}'") == "1":
        psql(cfg, f'DROP DATABASE "{cfg.db}" WITH (FORCE)')
        return [cfg.db]
    return []


def migrate(cfg):
    result = subprocess.run([cfg.binary("extend-service"), "migrate"], env=cfg.service_env(), cwd=cfg.dir,
                            capture_output=True, text=True, check=False, timeout=300)
    if result.returncode != 0:
        raise Failure("extend-service migrate failed:\n" + (result.stderr or result.stdout)[-2000:])
    return result.stdout.strip().splitlines()[-1] if result.stdout.strip() else "migrated"


# --- Extend's app webhook at Silicon Accounts ----------------------------------------------------------------------

def accounts(cfg, method, path, body=None):
    headers = {"Idempotency-Key": str(uuid.uuid4())} if method in ("POST", "PUT") else None
    return http(method, cfg.accounts_api_url + path, body=body, basic=cfg.basic, headers=headers)


def point_webhook(cfg):
    """Points Extend's webhook at the local service (every update), remembering what was there."""
    status, current = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook")
    if status != 200:
        raise Failure(f"GET /v1/apps/{APP_ID}/webhook answered {status} {error_code(current)}: is the app secret right?")
    previous = cfg.dir / "webhook-previous.json"
    if current.get("url") != cfg.webhook_url and not previous.exists():
        previous.parent.mkdir(parents=True, exist_ok=True)
        previous.write_text(json.dumps({"url": current.get("url"), "events": current.get("events")}))
    if current.get("url") == cfg.webhook_url and current.get("events") is None and current.get("secret_set") \
            and cfg.webhook_secret():
        return "already pointed here"
    status, answer = accounts(cfg, "PUT", f"/v1/apps/{APP_ID}/webhook", {"url": cfg.webhook_url, "events": None})
    if status != 200:
        raise Failure(f"PUT /v1/apps/{APP_ID}/webhook answered {status} {error_code(answer)}: {answer}")
    if answer.get("secret"):
        write_private(cfg.dir / "webhook-secret", answer["secret"])
        return "pointed here with a new secret"
    if not cfg.webhook_secret():
        new_webhook_secret(cfg)
        return "pointed here; a new secret was generated (the stored one was not known here)"
    return "pointed here (secret kept)"


def new_webhook_secret(cfg):
    status, answer = accounts(cfg, "POST", f"/v1/apps/{APP_ID}/webhook/generate-secret", {})
    if status != 200 or not (answer or {}).get("secret"):
        raise Failure(f"generate-secret answered {status} {error_code(answer)}")
    write_private(cfg.dir / "webhook-secret", answer["secret"])


def ping_reaches_service(cfg, seconds=30):
    """Queues a test ping and returns (delivered?, detail) from the delivery's own record at Silicon Accounts."""
    status, queued = accounts(cfg, "POST", f"/v1/apps/{APP_ID}/webhook/test", {})
    if status != 202:
        raise Failure(f"webhook test answered {status} {error_code(queued)}: {queued}")
    delivery = queued["delivery_id"]
    deadline = time.time() + seconds
    while time.time() < deadline:
        status, record = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook/deliveries/{delivery}")
        if status == 200 and record.get("status") == "delivered":
            return True, f"ping {queued['event_id']} delivered"
        if status == 200 and record.get("last_status") is not None and record.get("status") != "delivered":
            return False, f"ping answered HTTP {record.get('last_status')}: {(record.get('last_error') or '')[:200]}"
        time.sleep(0.5)
    return False, f"ping {queued['event_id']} not delivered within {seconds}s"


def give_webhook_back(cfg):
    previous = cfg.dir / "webhook-previous.json"
    if not previous.exists():
        return "nothing to give back"
    saved = json.loads(previous.read_text())
    status, current = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook")
    if status == 200 and current.get("url") != cfg.webhook_url:
        previous.unlink()
        return f"left as is: it points at {current.get('url')} now, not here"
    if not saved.get("url"):
        status, answer = accounts(cfg, "DELETE", f"/v1/apps/{APP_ID}/webhook")
        if status not in (200, 204):
            raise Failure(f"could not delete the webhook ({status} {error_code(answer)}); {previous} keeps the record")
        previous.unlink()
        (cfg.dir / "webhook-secret").unlink(missing_ok=True)
        return "deleted: there was no webhook before"
    status, answer = accounts(cfg, "PUT", f"/v1/apps/{APP_ID}/webhook", {"url": saved["url"], "events": saved.get("events")})
    if status != 200:
        raise Failure(f"could not put the webhook URL back ({status} {error_code(answer)}); {previous} keeps it")
    previous.unlink()
    return f"back to {saved['url']}"


# --- commands --------------------------------------------------------------------------------------------------------

def start_briefcase_stub(cfg):
    if cfg.files != "briefcase":
        return "not used (EXTEND_DEV_FILES=local)"
    if running(cfg, "extend-briefcase-stub"):
        return "already running"
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "BRIEFCASE_STUB_APP_SECRET": cfg.briefcase_secret}
    start(cfg, "extend-briefcase-stub",
          [sys.executable, "-I", str(ROOT / "scripts" / "briefcase_stub.py"), "--port", str(cfg.briefcase_port),
           "--accounts-api", cfg.accounts_api_url, "--log", str(cfg.briefcase_log), "--issuers", APP_ID],
          env, cfg.briefcase_port)
    wait_until(lambda: ready(f"{cfg.briefcase_url}/ready"), 15, "the Briefcase stand-in",
               cfg.dir / "logs/extend-briefcase-stub.log", lambda: running(cfg, "extend-briefcase-stub"))
    return f"listening on {cfg.briefcase_url}"


def start_service(cfg):
    """Starts extend-service when it is not running; returns whether it was (re)started."""
    if running(cfg, "extend-service"):
        return False
    start(cfg, "extend-service", [cfg.binary("extend-service"), "serve"], cfg.service_env(), cfg.api_port)
    wait_until(lambda: ready(f"{cfg.api_url}/ready"), 60, "extend-service", cfg.dir / "logs/extend-service.log",
               lambda: running(cfg, "extend-service"))
    return True


def prove_webhook(cfg, report):
    delivered, detail = ping_reaches_service(cfg)
    if not delivered:
        report["webhook_ping"] = detail + "; making a new secret and restarting the service"
        new_webhook_secret(cfg)
        stop(cfg, "extend-service")
        start_service(cfg)
        delivered, detail = ping_reaches_service(cfg)
        if not delivered:
            raise Failure(f"Silicon Accounts' deliveries still fail after a new secret: {detail}")
    report["webhook_ping"] = detail


def build(cfg):
    env = dict(os.environ)
    commands = [["cargo", "build", "--locked", "-p", "extend-service", "-p", "silicon-extend-cli", "--bins"],
                ["cargo", "build", "--locked", "-p", "extend-service", "--example", "fake_device"]]
    for command in commands:
        print("$ " + " ".join(command), file=sys.stderr)
        if subprocess.run(command, cwd=ROOT, env=env, check=False).returncode != 0:
            raise Failure("cargo build failed")


def cmd_up(cfg, args):
    if args.build:
        build(cfg)
    cfg.dir.mkdir(parents=True, exist_ok=True)
    report = {"databases_created": ensure_database(cfg)}
    report["migrated"] = migrate(cfg)
    report["webhook"] = point_webhook(cfg)
    report["briefcase_stub"] = start_briefcase_stub(cfg)
    started = start_service(cfg)
    if started or args.check_webhook:
        prove_webhook(cfg, report)
    report.update(status_of(cfg))
    print(json.dumps(report, indent=1))


def cmd_restart(cfg, _args):
    report = {"extend-service": stop(cfg, "extend-service")}
    start_service(cfg)
    report.update(status_of(cfg))
    print(json.dumps(report, indent=1))


def cmd_down(cfg, args):
    report = {name: stop(cfg, name) for name in reversed(SERVICES)}
    if not args.keep_webhook:
        report["webhook"] = give_webhook_back(cfg)
    if args.drop:
        report["databases_dropped"] = drop_database(cfg)
    print(json.dumps(report, indent=1))


def status_of(cfg):
    services = {}
    for name, port in zip(SERVICES, (cfg.briefcase_port, cfg.api_port)):
        services[name] = {"pid": running(cfg, name), "port": port, "listening": port_open(port)}
    try:
        status, hook = accounts(cfg, "GET", f"/v1/apps/{APP_ID}/webhook")
        webhook = {"url": hook.get("url"), "events": hook.get("events"), "secret_set": hook.get("secret_set"),
                   "points_here": hook.get("url") == cfg.webhook_url} if status == 200 else {"error": status}
    except Failure as error:
        webhook = {"error": str(error)}
    return {"api": cfg.api_url, "briefcase": cfg.briefcase_url if cfg.files == "briefcase" else None,
            "accounts": cfg.accounts_url, "database": cfg.db, "services": services,
            "webhook_at_accounts": webhook, "dir": str(cfg.dir)}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    up = commands.add_parser("up", help="start everything that is not running (idempotent)")
    up.add_argument("--build", action="store_true", help="cargo build the service, the CLI and the fake device first")
    up.add_argument("--check-webhook", action="store_true", help="send a test ping even when nothing was started")
    down = commands.add_parser("down", help="stop what `up` started and give the webhook back")
    down.add_argument("--keep-webhook", action="store_true", help="leave Extend's webhook pointing at this machine")
    down.add_argument("--drop", action="store_true", help="also drop the development database")
    commands.add_parser("restart", help="restart extend-service (same data and secrets)")
    commands.add_parser("status", help="what runs, as JSON")
    args = parser.parse_args(argv)
    try:
        cfg = Config()
        {"up": cmd_up, "down": cmd_down, "restart": cmd_restart,
         "status": lambda c, _a: print(json.dumps(status_of(c), indent=1))}[args.command](cfg, args)
    except Failure as error:
        print(f"dev-accounts: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
