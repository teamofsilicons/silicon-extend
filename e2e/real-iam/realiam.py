#!/usr/bin/env python3
"""Silicon Bridge against a real, disposable Silicon IAM.

    e2e/real-iam/realiam.py up      # IAM (Postgres, API, worker, webhook relay) + a Bridge on :8497
    e2e/real-iam/realiam.py check   # the verification run against what `up` started
    e2e/real-iam/realiam.py down    # stop Bridge, remove this fixture's containers/network/database
    e2e/real-iam/realiam.py all     # up, check, down (down runs even when check fails; --keep skips it)

`check` also covers IAM's testing plane: Bridge's production credential creates a test
environment, test identities sign up through IAM (code 000000), Bridge selects the environment
from X-Testing-Application-Secret, and a signed test-plane webhook is routed to the test world.

Everything is owned by this fixture: one Docker network, three containers, one database on the
Bridge Postgres (`silicon-bridge-postgres`, :5440). Nothing reads existing credentials. Identity
rows (team acme, c:alice, si:chef, si:sous, the `bridge` application, its secret, scopes and webhook
endpoint) are seeded by SQL into the disposable IAM; SLT issuance, login, refresh, revocation,
authorization, directory reads, Silicon removal and webhook delivery are real IAM APIs.
See README.md next to this file.
"""
import argparse
import base64
import datetime
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import struct
import subprocess
import sys
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
STATE_DIR = Path(os.environ.get("REALIAM_STATE_DIR", HERE / ".state"))
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target" / "realiam"))
IAM_IMAGE = os.environ.get("REALIAM_IAM_IMAGE", "silicon-iam:release-433665db296d4cdb26b846e2db97ede81066bf09")
PG_IMAGE = "postgres:16.15-bookworm"
IAM_CLI = os.environ.get("REALIAM_IAM_CLI", str(Path.home() / ".silicon" / "bin" / "iam"))
NAME = "bridge-realiam"
# IAM delivers webhooks only to public addresses (its SSRF guard runs in development too), so the
# fixture network uses a range IAM considers public. It is an isolated Docker bridge; nothing routes out.
SUBNET = os.environ.get("REALIAM_SUBNET", "100.128.7.0/24")
RELAY_IP = SUBNET.rsplit(".", 1)[0] + ".10"
BRIDGE_PORT = int(os.environ.get("REALIAM_BRIDGE_PORT", "8497"))
BRIDGE_DB_CONTAINER = "silicon-bridge-postgres"
BRIDGE_DB = "bridge_realiam"
BRIDGE_DB_URL = f"postgres://bridge:bridge@127.0.0.1:5440/{BRIDGE_DB}"
ORG_UUID = "3f1b1a52-7a55-4c55-8f4e-0b1d9d7a5c01"
ACTORS = {"alice": ("c:alice", "carbon", "owner"),
          "chef": ("si:chef", "silicon", "member"),
          "sous": ("si:sous", "silicon", "member")}
BRIDGE_SCOPES = ["self.identity.read", "self.profile.read", "self.organizations.read", "self.membership.read",
                 "directory.silicons.read", "directory.carbons.read", "directory.memberships.read",
                 "directory.profiles.read"]


# ───────────────────────────── plumbing ─────────────────────────────

def private(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    with open(path, "w", opener=lambda n, f: os.open(n, f, 0o600)) as out:
        out.write(value if isinstance(value, str) else json.dumps(value, indent=2) + "\n")


def load():
    path = STATE_DIR / "state.json"
    if not path.exists():
        raise SystemExit("no fixture is up (run `realiam.py up`)")
    return json.loads(path.read_text())


def save(state):
    private(STATE_DIR / "state.json", state)


def run(args, *, stdin=None, env=None, check=True, timeout=300):
    r = subprocess.run(args, input=stdin, capture_output=True, env=env, timeout=timeout)
    if check and r.returncode:
        raise RuntimeError(f"{' '.join(map(str, args[:4]))} … exited {r.returncode}:\n{r.stdout.decode(errors='replace')[-2000:]}{r.stderr.decode(errors='replace')[-2000:]}")
    return r


def psql(sql, container=f"{NAME}-postgres", user="postgres", database="iam"):
    return run(["docker", "exec", "-i", container, "psql", "-U", user, "-d", database, "-X", "-v", "ON_ERROR_STOP=1", "-At"],
               stdin=sql.encode()).stdout.decode()


def q(v):
    return "NULL" if v is None else "'" + str(v).replace("'", "''") + "'"


def b64(raw):
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def envelope(kind, data):
    """Bridge's request body shape."""
    return {"type": kind, "data": data}


def err_code(v):
    """The error code from a Bridge ({"type":"error","data":{...}}) or IAM ({"error":{...}}) body."""
    if not isinstance(v, dict):
        return None
    return (v.get("data") or {}).get("code") if v.get("type") == "error" else (v.get("error") or {}).get("code")


def http(method, url, body=None, headers=None, token=None, expected=None):
    h = {"Content-Type": "application/json", **(headers or {})}
    if token:
        h["Authorization"] = "Bearer " + token
    raw = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    req = urllib.request.Request(url, data=raw, method=method, headers=h)
    try:
        with urllib.request.urlopen(req, timeout=40) as resp:
            status, data = resp.status, resp.read()
    except urllib.error.HTTPError as e:
        status, data = e.code, e.read()
    try:
        value = json.loads(data) if data else None
    except ValueError:
        value = data.decode(errors="replace")
    if expected and status not in expected:
        raise RuntimeError(f"{method} {url} -> HTTP {status}: {str(value)[:600]}")
    return status, value


def wait_http(url, what, tries=120):
    for _ in range(tries):
        try:
            with urllib.request.urlopen(url, timeout=2):
                return
        except urllib.error.HTTPError:
            return
        except OSError:
            time.sleep(0.5)
    raise RuntimeError(f"{what} never answered at {url}")


def digest(pepper, purpose, value):
    """IAM's token/secret digest (silicon-iam src/infrastructure/crypto.rs)."""
    material = b"silicon-iam:v1:digest" + struct.pack(">h", 1) + b"\0" + purpose.encode() + b"\0" + value.encode()
    return hmac.new(pepper, material, hashlib.sha256).hexdigest()


def encrypt(key, field, tenant, entity_uuid, plaintext):
    """IAM's AES-256-GCM row-bound encryption (encryption_aad in crypto.rs), key version 1."""
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    scope = b"\1" + tenant.encode() if tenant is not None else b"\0"
    aad = b"silicon-iam:v1:encryption" + bytes([1]) + struct.pack(">h", 1) + b"\0" + field + b"\0" + scope + uuid.UUID(entity_uuid).bytes
    nonce = secrets.token_bytes(12)
    return AESGCM(key).encrypt(nonce, plaintext.encode(), aad).hex(), nonce.hex()


# ───────────────────────────── up ─────────────────────────────

def seed(state, pepper, enc_key):
    owner = ACTORS["alice"][0]
    parts = ["BEGIN;",
             "INSERT INTO iam.cryptographic_key_versions(purpose,key_version,status) VALUES ('token_hmac',1,'active'),('contact_aead',1,'active') ON CONFLICT DO NOTHING;"]
    for actor, kind, _ in ACTORS.values():
        parts.append(f"INSERT INTO iam.principals(id,kind,status,activated_at) VALUES({q(actor)},{q(kind)},'active',now());")
    parts += [f"INSERT INTO iam.carbons(id,carbon_id,display_name) VALUES({q(owner)},{q(owner)},'Alice');",
              f"INSERT INTO iam.organizations(id,org_id,created_by_carbon_id,name) VALUES('{ORG_UUID}','acme',{q(owner)},'Acme');"]
    # Verified contacts, encrypted as IAM stores them, so step-up (local provider, code 000000) works.
    for kind, value in (("email", "alice@example.invalid"), ("phone", "+12025550143")):
        contact = str(uuid.uuid4())
        ct, nonce = encrypt(enc_key, b"carbon-" + kind.encode(), None, contact, value)
        parts.append(f"INSERT INTO iam.carbon_contacts(id,carbon_id,kind,ciphertext,nonce,encryption_key_version,verified_at) VALUES('{contact}',{q(owner)},{q(kind)},decode('{ct}','hex'),decode('{nonce}','hex'),1,now());")
    state["direct"] = {}
    for label, (actor, kind, role) in ACTORS.items():
        membership, session, access_id = str(uuid.uuid4()), str(uuid.uuid4()), str(uuid.uuid4())
        direct = ("cat_" if kind == "carbon" else "sat_") + secrets.token_urlsafe(32)
        state["direct"][label] = {"access_token": direct, "membership_id": membership, "session_id": session}
        parts.append(f"INSERT INTO iam.organization_memberships(id,organization_id,principal_id,principal_kind,org_role) VALUES('{membership}','{ORG_UUID}',{q(actor)},{q(kind)},{q(role)});")
        if kind == "silicon":
            parts.append(f"INSERT INTO iam.silicons(id,organization_id,membership_id,organization_handle,silicon_handle,display_name,provisioning_status) VALUES({q(actor)},'{ORG_UUID}','{membership}','acme',{q(actor.removeprefix('si:'))},{q(label.title())},'active');")
        method = "email_otp" if kind == "carbon" else "silicon_credential"
        parts.append(f"INSERT INTO iam.authentication_sessions(id,subject_principal_id,subject_kind,authentication_method,subject_auth_epoch,idle_expires_at,absolute_expires_at) VALUES('{session}',{q(actor)},{q(kind)},{q(method)},1,now()+interval '1 day',now()+interval '2 days');")
        parts.append(f"INSERT INTO iam.access_tokens(id,token_class,token_digest,digest_key_version,token_prefix,authentication_session_id,subject_principal_id,subject_kind,audience,subject_auth_epoch,expires_at) VALUES('{access_id}',{q(kind + '_access')},decode('{digest(pepper, kind + '-access-token', direct)}','hex'),1,{q(direct[:12])},'{session}',{q(actor)},{q(kind)},'silicon-iam',1,now()+interval '1 day');")
        parts.append(f"INSERT INTO iam.access_token_scopes(access_token_id,scope) VALUES('{access_id}','iam.self');")
    app, secret = "bridge", state["app_secret"]
    parts.append(f"INSERT INTO iam.principals(id,kind,status,activated_at) VALUES({q(app)},'application','active',now());")
    parts.append(f"INSERT INTO iam.applications(id,app_id,organization_id,created_by_carbon_id,app_name,review_status,visibility,base_url,app_scope,webhook_scope) VALUES({q(app)},{q(app)},'{ORG_UUID}',{q(owner)},'Silicon Bridge','verified','public','http://127.0.0.1:{BRIDGE_PORT}',{q(json.dumps({'iam': BRIDGE_SCOPES, 'external': []}))}::jsonb,ARRAY['full']);")
    parts.append(f"INSERT INTO iam.application_secrets(id,application_id,secret_version,secret_prefix,secret_digest,pepper_key_version,created_by_carbon_id) VALUES('{uuid.uuid4()}',{q(app)},1,{q(secret[:12])},decode('{digest(pepper, 'application-secret', secret)}','hex'),1,{q(owner)});")
    for scope in BRIDGE_SCOPES:
        parts += [f"INSERT INTO iam.oauth_scope_catalog(scope,description,sensitive) VALUES({q(scope)},'Bridge real-IAM fixture',false) ON CONFLICT DO NOTHING;",
                  f"INSERT INTO iam.application_requested_scopes(application_id,scope) VALUES({q(app)},{q(scope)});",
                  f"INSERT INTO iam.application_approved_scopes(application_id,scope,approved_by_carbon_id) VALUES({q(app)},{q(scope)},{q(owner)});"]
    # The webhook endpoint and signing key, encrypted exactly as IAM stores them. (IAM's own
    # registration API accepts only public HTTPS URLs; this fixture delivers over plain HTTP to a relay.)
    endpoint, key_id = str(uuid.uuid4()), str(uuid.uuid4())
    url = state["webhook_url"]
    url_ct, url_nonce = encrypt(enc_key, b"application-webhook-url", app, endpoint, url)
    sec_ct, sec_nonce = encrypt(enc_key, b"application-webhook-signing-secret", app, key_id, state["webhook_secret"])
    prefix = "whs_" + hashlib.sha256(state["webhook_secret"].encode()).hexdigest()[:8]
    parts.append(f"INSERT INTO iam.application_webhook_endpoints(id,application_id,url_ciphertext,url_nonce,encryption_key_version,url_digest,status,activated_at) VALUES('{endpoint}',{q(app)},decode('{url_ct}','hex'),decode('{url_nonce}','hex'),1,decode('{hashlib.sha256(url.encode()).hexdigest()}','hex'),'active',now());")
    parts.append(f"INSERT INTO iam.application_webhook_signing_keys(id,application_id,endpoint_id,secret_version,key_prefix,secret_ciphertext,secret_nonce,encryption_key_version) VALUES('{key_id}',{q(app)},'{endpoint}',1,{q(prefix)},decode('{sec_ct}','hex'),decode('{sec_nonce}','hex'),1);")
    parts.append("COMMIT;")
    psql("\n".join(parts))
    expiry = (datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(hours=12)).isoformat().replace("+00:00", "Z")
    for label, (actor, kind, _) in ACTORS.items():
        store = STATE_DIR / "iam-profiles" / label / ".silicon-iam"
        private(store / "config.json", {"telemetry": False, "auto_update": False, "current_profile": "default",
                                        "profiles": {"default": {"url": state["iam_url"]}}})
        private(store / "credentials.json", {"sessions": {"default": {"access_token": state["direct"][label]["access_token"],
                "refresh_token": "", "expires_at": expiry, "actor_type": kind, "actor_id": actor}}})


RELAY = r"""
use IO::Socket::INET; $SIG{CHLD}='IGNORE'; $|=1;
my ($port,$to)=@ARGV;
my $l=IO::Socket::INET->new(LocalPort=>$port,Listen=>64,ReuseAddr=>1) or die "listen: $!";
print "relay :$port -> $to\n";
while (my $c=$l->accept) {
  next if fork;
  my $u=IO::Socket::INET->new(PeerAddr=>$to) or exit 1;
  if (fork) { while (sysread($c,my $b,65536)) { syswrite($u,$b) } shutdown($u,1); exit 0 }
  else { while (sysread($u,my $b,65536)) { syswrite($c,$b) } shutdown($c,1); exit 0 }
}
"""


def up(args):
    if (STATE_DIR / "state.json").exists():
        raise SystemExit(f"a fixture is already up ({STATE_DIR}); run `realiam.py down` first")
    for tool in (IAM_CLI, TARGET / "debug" / "bridge-service", TARGET / "debug" / "bridge", TARGET / "debug" / "examples" / "fake_device"):
        if not Path(tool).exists():
            raise SystemExit(f"missing {tool}; build with: CARGO_TARGET_DIR={TARGET} cargo build -p bridge-service -p bridge-cli --bins --examples")
    import socket
    with socket.socket() as probe:
        if probe.connect_ex(("127.0.0.1", BRIDGE_PORT)) == 0:
            raise SystemExit(f"port {BRIDGE_PORT} is in use; set REALIAM_BRIDGE_PORT to a free port")
    STATE_DIR.mkdir(mode=0o700, exist_ok=True)
    state = {"containers": [], "network": NAME, "iam_image": IAM_IMAGE,
             "app_secret": "ask_" + b64(secrets.token_bytes(32)),
             "webhook_secret": "bridge-realiam-webhook-" + secrets.token_hex(16),
             "webhook_url": f"http://{RELAY_IP}:{BRIDGE_PORT}/webhook/",
             "honeycomb_token": "hck_" + secrets.token_hex(16)}
    save(state)
    print(f"• IAM image {IAM_IMAGE}")
    run(["docker", "network", "create", "--subnet", SUBNET, NAME])
    db_pw, rt_pw = secrets.token_urlsafe(24), secrets.token_urlsafe(24)
    run(["docker", "run", "-d", "--name", f"{NAME}-postgres", "--network", NAME, "--network-alias", "database",
         "-e", f"POSTGRES_PASSWORD={db_pw}", "-e", "POSTGRES_DB=iam", PG_IMAGE])
    state["containers"].append(f"{NAME}-postgres"); save(state)
    for _ in range(120):
        if run(["docker", "exec", f"{NAME}-postgres", "pg_isready", "-U", "postgres", "-d", "iam"], check=False).returncode == 0:
            break
        time.sleep(0.5)
    time.sleep(1)
    psql(f"CREATE ROLE silicon_iam_api NOLOGIN; CREATE ROLE silicon_iam_worker NOLOGIN; CREATE ROLE silicon_iam_key_operator NOLOGIN;"
         f"CREATE ROLE fixture_api LOGIN PASSWORD {q(rt_pw)} IN ROLE silicon_iam_api;"
         f"CREATE ROLE fixture_worker LOGIN PASSWORD {q(rt_pw)} IN ROLE silicon_iam_worker;")
    psql("CREATE DATABASE iam_testing;")
    pepper, enc_key = secrets.token_bytes(32), secrets.token_bytes(32)
    env = {"IAM_ENVIRONMENT": "development", "IAM_BIND_ADDR": "0.0.0.0:8080", "IAM_ALLOW_LOCAL_PROVIDERS": "true",
           "IAM_EXPOSE_LOCAL_OTPS": "true", "IAM_LOG_FILTER": "warn", "IAM_TELEMETRY": "off",
           "IAM_DATABASE_URL": f"postgres://fixture_api:{rt_pw}@database:5432/iam",
           "IAM_MIGRATOR_DATABASE_URL": f"postgres://postgres:{db_pw}@database:5432/iam",
           # IAM's isolated testing plane lives in its own database.
           "IAM_TESTING_DATABASE_URL": f"postgres://fixture_api:{rt_pw}@database:5432/iam_testing",
           "IAM_TESTING_MIGRATOR_DATABASE_URL": f"postgres://postgres:{db_pw}@database:5432/iam_testing",
           "IAM_TOKEN_PEPPER_CURRENT_VERSION": "1", "IAM_TOKEN_PEPPER_KEYRING": json.dumps({"1": b64(pepper)}),
           "IAM_BLIND_INDEX_CURRENT_VERSION": "1", "IAM_BLIND_INDEX_KEYRING": json.dumps({"1": b64(secrets.token_bytes(32))}),
           "IAM_ENCRYPTION_CURRENT_VERSION": "1", "IAM_ENCRYPTION_KEYRING": json.dumps({"1": b64(enc_key)}),
           "IAM_COOKIE_KEY": b64(secrets.token_bytes(32)), "IAM_PUBLIC_BASE_URL": "http://127.0.0.1:8080",
           "IAM_AUTH_BASE_URL": "http://127.0.0.1:8080", "IAM_CORS_ALLOWED_ORIGINS": "http://127.0.0.1:8080",
           "IAM_WORKER_POLL_INTERVAL_MS": "250"}
    private(STATE_DIR / "iam.env", "".join(f"{k}={v}\n" for k, v in env.items()))
    worker_env = {**env, "IAM_DATABASE_URL": f"postgres://fixture_worker:{rt_pw}@database:5432/iam",
                  "IAM_TESTING_DATABASE_URL": f"postgres://fixture_worker:{rt_pw}@database:5432/iam_testing"}
    private(STATE_DIR / "iam-worker.env", "".join(f"{k}={v}\n" for k, v in worker_env.items()))
    print("• migrating IAM")
    run(["docker", "run", "--rm", "--network", NAME, "--env-file", str(STATE_DIR / "iam.env"), IAM_IMAGE, "iam-migrate"])
    grants = run(["docker", "run", "--rm", "--entrypoint", "cat", IAM_IMAGE, "/opt/silicon-iam/postgres/runtime-grants.sql"]).stdout.decode()
    for database in ("iam", "iam_testing"):
        psql(grants, database=database)
    run(["docker", "run", "-d", "--name", f"{NAME}-iam", "--network", NAME, "--network-alias", "iam",
         "-p", "127.0.0.1::8080", "--env-file", str(STATE_DIR / "iam.env"), IAM_IMAGE, "iam-api"])
    state["containers"].append(f"{NAME}-iam"); save(state)
    port = run(["docker", "port", f"{NAME}-iam", "8080/tcp"]).stdout.decode().split()[0].rsplit(":", 1)[1]
    state["iam_url"] = f"http://127.0.0.1:{port}"; save(state)
    print("• seeding team acme: c:alice (owner), si:chef, si:sous; application bridge")
    seed(state, pepper, enc_key); save(state)
    wait_http(state["iam_url"] + "/healthz", "IAM")
    run(["docker", "run", "-d", "--name", f"{NAME}-worker", "--network", NAME, "--env-file", str(STATE_DIR / "iam-worker.env"), IAM_IMAGE, "iam-worker"])
    state["containers"].append(f"{NAME}-worker"); save(state)
    run(["docker", "run", "-d", "--name", f"{NAME}-relay", "--network", NAME, "--ip", RELAY_IP, "--entrypoint", "perl",
         PG_IMAGE, "-e", RELAY, str(BRIDGE_PORT), f"host.docker.internal:{BRIDGE_PORT}"])
    state["containers"].append(f"{NAME}-relay"); save(state)
    print(f"• IAM {state['iam_url']}  (webhooks → {state['webhook_url']} → host :{BRIDGE_PORT})")

    psql(f"DROP DATABASE IF EXISTS {BRIDGE_DB} WITH (FORCE); CREATE DATABASE {BRIDGE_DB};", container=BRIDGE_DB_CONTAINER, user="bridge", database="bridge")
    state["bridge_db"] = BRIDGE_DB; save(state)
    start_bridge(state)
    print(f"READY  state: {STATE_DIR / 'state.json'}")


def bridge_env(state):
    env = {k: v for k, v in os.environ.items() if not k.startswith("BRIDGE_")}
    env.update({"BRIDGE_ENVIRONMENT": "development", "BRIDGE_DATABASE_URL": BRIDGE_DB_URL,
                "BRIDGE_BIND": f"127.0.0.1:{BRIDGE_PORT}", "BRIDGE_PUBLIC_URL": f"http://127.0.0.1:{BRIDGE_PORT}",
                "BRIDGE_DATA_DIR": str(STATE_DIR / "bridge-data"), "BRIDGE_IAM_MODE": "sdk",
                "BRIDGE_IAM_BASE_URL": state["iam_url"], "BRIDGE_IAM_APP_ID": "bridge",
                "BRIDGE_IAM_APP_SECRET": state["app_secret"], "BRIDGE_IAM_WEBHOOK_SECRET": state["webhook_secret"],
                "BRIDGE_IAM_WEBHOOK_SECRET_VERSION": "1", "BRIDGE_FILES_MODE": "local", "BRIDGE_TING_MODE": "local",
                "BRIDGE_HONEYCOMB_SERVICE_TOKEN": state["honeycomb_token"],
                "BRIDGE_LOG": "info,bridge_service=debug,sqlx=warn,tower_http=warn"})
    return env


def start_bridge(state):
    log = open(STATE_DIR / "bridge.log", "ab")
    proc = subprocess.Popen([str(TARGET / "debug" / "bridge-service"), "serve"], env=bridge_env(state), stdout=log, stderr=log,
                            cwd=ROOT, start_new_session=True)
    state["bridge_pid"] = proc.pid; save(state)
    for _ in range(120):
        if proc.poll() is not None:
            raise RuntimeError(f"bridge-service exited {proc.returncode}; see {STATE_DIR / 'bridge.log'}:\n" + (STATE_DIR / "bridge.log").read_text()[-3000:])
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{BRIDGE_PORT}/api/version", timeout=2):
                break
        except urllib.error.HTTPError:
            break
        except OSError:
            time.sleep(0.5)
    print(f"• Bridge (BRIDGE_IAM_MODE=sdk) on http://127.0.0.1:{BRIDGE_PORT}, pid {proc.pid}, log {STATE_DIR / 'bridge.log'}")


def stop_bridge(state):
    pid = state.get("bridge_pid")
    if pid:
        def gone():
            try:  # reap it when it is our own child (`all`), so it does not linger as a zombie
                if os.waitpid(pid, os.WNOHANG)[0] == pid:
                    return True
            except ChildProcessError:
                pass
            try:
                os.killpg(pid, 0)
                return False
            except (ProcessLookupError, PermissionError):
                return True
        try:
            os.killpg(pid, signal.SIGINT)
        except (ProcessLookupError, PermissionError):
            pass
        for _ in range(40):
            if gone():
                break
            time.sleep(0.25)
        else:
            try:
                os.killpg(pid, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        state["bridge_pid"] = None


def down(_args=None):
    path = STATE_DIR / "state.json"
    if not path.exists():
        print("nothing to tear down")
        return
    state = json.loads(path.read_text())
    stop_bridge(state)
    for name in reversed(state.get("containers", [])):
        if not name.startswith(NAME + "-"):
            raise RuntimeError(f"refusing to remove unowned container {name}")
        run(["docker", "rm", "-f", name], check=False)
    run(["docker", "network", "rm", NAME], check=False)
    if state.get("bridge_db"):
        run(["docker", "exec", "-i", BRIDGE_DB_CONTAINER, "psql", "-U", "bridge", "-d", "bridge", "-X", "-c",
             f"DROP DATABASE IF EXISTS {BRIDGE_DB} WITH (FORCE)"], check=False)
    keep = STATE_DIR.parent / "last-run"
    shutil.rmtree(keep, ignore_errors=True)
    keep.mkdir(mode=0o700)
    for f in ("bridge.log", "report.json"):
        if (STATE_DIR / f).exists():
            shutil.copy2(STATE_DIR / f, keep / f)
    shutil.rmtree(STATE_DIR, ignore_errors=True)
    print(f"Removed this fixture's containers, network and {BRIDGE_DB}; logs kept in {keep}")


# ───────────────────────────── check ─────────────────────────────

def step_up(state, actor, action, resource):
    """A real IAM step-up: challenge on the verified email, then verify with the local provider's code."""
    token = state["direct"][actor]["access_token"]
    _, ch = http("POST", state["iam_url"] + "/api/v1/step-up/challenges", {"action": action, "resource_id": resource, "channel": "email"},
                 token=token, headers={"Idempotency-Key": str(uuid.uuid4())}, expected=(200, 201))
    _, v = http("POST", state["iam_url"] + f"/api/v1/step-up/challenges/{ch['session_id']}/verify", {"code": ch["local_otp"]},
                token=token, headers={"Idempotency-Key": str(uuid.uuid4())}, expected=(200, 201))
    return v["step_up_token"]


class Checks:
    def __init__(self, state):
        self.s = state
        self.api = f"http://127.0.0.1:{BRIDGE_PORT}"
        self.passed, self.failed, self.notes = [], [], []
        self.work = STATE_DIR / "work"
        self.work.mkdir(exist_ok=True)

    # helpers
    def ok(self, name, detail=""):
        self.passed.append({"check": name, "detail": detail})
        print(f"  ✓ {name}" + (f"  — {detail}" if detail else ""), flush=True)

    def fail(self, name, detail):
        self.failed.append({"check": name, "detail": detail})
        print(f"  ✗ {name}  — {detail}", flush=True)

    def iam_cli(self, actor, args, stdin=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith(("SILICON_IAM_", "IAM_TEST_")) and k != "SILICON_ORG"}
        env["SILICON_HOME"] = str(STATE_DIR / "iam-profiles" / actor)
        r = run([IAM_CLI, "--url", self.s["iam_url"], "--no-org", "--json", *args], stdin=stdin, env=env, timeout=60)
        return json.loads(r.stdout)

    def slt(self, actor):
        return self.iam_cli(actor, ["login", "--app-id", "bridge", "--grant-org", "acme", "--approve-scopes"])["slt"]

    def bridge(self, who, *args, check=True, test=None):
        home = self.work / "homes" / who
        home.mkdir(parents=True, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith("BRIDGE_")}
        env.update({"BRIDGE_API_URL": self.api, "BRIDGE_TELEMETRY": "off", "SILICON_HOME": str(home)})
        full = [str(TARGET / "debug" / "bridge")] + (["--test", test] if test else []) + list(args)
        r = subprocess.run(full, capture_output=True, env=env, timeout=120)
        out, err = r.stdout.decode(errors="replace"), r.stderr.decode(errors="replace")
        if check and r.returncode:
            raise RuntimeError(f"bridge {' '.join(args)} (as {who}) exited {r.returncode}: {out[-800:]} {err[-800:]}")
        return r.returncode, out, err

    def auth_file(self, who, test=None):
        root = self.work / "homes" / who
        found = [p for p in root.rglob("auth.json")] if not test else [p for p in root.rglob(f"{test}.json")]
        return found[0] if found else None

    def step(self, name, fn):
        try:
            fn()
        except Exception as e:  # keep going: report every check
            self.fail(name, f"{type(e).__name__}: {e}")
            traceback.print_exc(limit=2, file=sys.stderr)

    def bridge_log(self):
        import re
        return re.sub(r"\x1b\[[0-9;]*m", "", (STATE_DIR / "bridge.log").read_text(errors="replace"))

    def wait_log(self, needle, since, timeout=45):
        deadline = time.time() + timeout
        while time.time() < deadline:
            log = self.bridge_log()[since:]
            if needle in log:
                return log
            time.sleep(0.5)
        return None


def check(args):
    state = load()
    c = Checks(state)
    ctx = {}
    print(f"Bridge {c.api} against real IAM {state['iam_url']}")

    def negotiation():
        log = c.bridge_log()
        if "Silicon Bridge service listening" not in log:
            raise RuntimeError("service never reported listening")
        c.ok("Bridge started in BRIDGE_IAM_MODE=sdk; IAM API-version negotiation succeeded at startup",
             "SdkIam::connect → system().negotiate() returned before listen")
        code, out, _ = c.bridge("nobody", "iam", "--json")
        c.ok("bridge iam --json", json.loads(out)["data"].get("app_id", "?"))
    c.step("startup", negotiation)

    def logins():
        for who in ("alice", "chef", "sous"):
            slt = c.slt(who)
            c.bridge(who, "login", slt)
            code, out, _ = c.bridge(who, "login", "status", "--json")
            d = json.loads(out)["data"]
            want = ACTORS[who][0]
            if not d.get("authenticated") or d["member"]["id"] != want:
                raise RuntimeError(f"login status for {who}: {d}")
            c.ok(f"real SLT → bridge login; login status --json authenticated:true as {want}", f"teams={d.get('teams')} role={d.get('team_role')}")
    c.step("login", logins)

    def team():
        code, out, _ = c.bridge("alice", "team", "ls")
        if "acme" not in out:
            raise RuntimeError(out)
        c.ok("bridge team ls lists acme", out.strip().replace("\n", " | ")[:120])
        tok = json.loads(c.auth_file("alice").read_text())["access_token"]
        status, v = http("GET", c.api + "/api/v1/team/silicons", token=tok, headers={"X-Org-ID": "acme"})
        ids = sorted(i["id"] for i in (v or {}).get("data", {}).get("items", [])) if status == 200 else None
        if ids != ["si:chef", "si:sous"]:
            raise RuntimeError(f"GET /api/v1/team/silicons -> {status} {v}")
        c.ok("GET /api/v1/team/silicons lists si:chef and si:sous from IAM's directory", str(ids))
    c.step("team", team)

    def device():
        log = c.work / "fake.log"
        fake = subprocess.Popen([str(TARGET / "debug" / "examples" / "fake_device"), c.api, "linux"], stdout=open(log, "wb"), stderr=subprocess.STDOUT)
        ctx["fake"] = fake
        code = None
        for _ in range(100):
            txt = log.read_text(errors="replace")
            if "PAIRING_CODE" in txt:
                code = txt.split("PAIRING_CODE", 1)[1].split()[0]
                break
            time.sleep(0.2)
        if not code:
            raise RuntimeError("fake device never showed a pairing code: " + log.read_text()[-500:])
        _, out, _ = c.bridge("alice", "device", "pair", code, "--name", "Real IAM box", "--json")
        dev = json.loads(out)["data"]["device_id"]
        ctx["device"] = dev
        for _ in range(50):
            if "PAIRED" in log.read_text(errors="replace"):
                break
            time.sleep(0.2)
        c.ok("alice paired the fake device through real IAM auth", dev)
        _, out, _ = c.bridge("alice", "device", "access", "grant", dev, "si:chef")
        c.ok("bridge device access grant si:chef (IAM member_active for a Silicon)", out.strip()[:100])
        _, out, _ = c.bridge("alice", "device", "access", "grant", dev, "si:sous")
        c.ok("bridge device access grant si:sous", out.strip()[:100])
        rc, out, err = c.bridge("alice", "device", "access", "grant", dev, "si:nobody", check=False)
        if rc == 0:
            raise RuntimeError("granting a non-member Silicon succeeded")
        c.ok("granting a Silicon that is not in acme is refused", f"exit {rc}: {err.strip()[:100]}")
    c.step("device", device)

    def session():
        dev = ctx["device"]
        _, sid, _ = c.bridge("chef", "session", "new", dev, "--connect")
        ctx["session"] = sid.strip()
        c.ok("si:chef started a session", sid.strip())
        _, out, _ = c.bridge("chef", "snapshot", "-i")
        c.ok("bridge snapshot relayed", out.strip().splitlines()[0][:80] if out.strip() else "")
        shot = c.work / "shot.png"
        c.bridge("chef", "screenshot", "--out", str(shot))
        if not shot.exists() or shot.stat().st_size == 0:
            raise RuntimeError("screenshot not saved")
        c.ok("bridge screenshot stored and saved", f"{shot.stat().st_size} bytes")
    c.step("session", session)

    def refresh():
        path = c.auth_file("chef")
        auth = json.loads(path.read_text())
        old_access, old_refresh = auth["access_token"], auth["refresh_token"]
        # Direct refresh through the API with the real refresh token.
        status, v = http("POST", c.api + "/api/v1/auth/refresh", envelope("refresh", {"refresh_token": old_refresh}),
                         headers={"Idempotency-Key": str(uuid.uuid4())}, expected=(200,))
        new = v["data"]
        if new["access_token"] == old_access or new["refresh_token"] == old_refresh:
            raise RuntimeError("refresh did not rotate the pair")
        c.ok("POST /api/v1/auth/refresh rotated the IAM token pair", f"expires_in={new['expires_in']} teams={new.get('teams')}")
        status, v = http("GET", c.api + "/api/v1/auth/me", token=old_access)
        c.notes.append(f"old access token after refresh: HTTP {status}")
        status, v = http("POST", c.api + "/api/v1/auth/refresh", envelope("refresh", {"refresh_token": old_refresh}),
                         headers={"Idempotency-Key": str(uuid.uuid4())})
        if status == 200:
            raise RuntimeError("a consumed refresh token was accepted again")
        c.ok("reusing the consumed refresh token is refused", f"HTTP {status} {err_code(v)}")
        # IAM treats that reuse as compromise and revokes the family; log chef in again for what follows.
        c.bridge("chef", "login", c.slt("chef"))
        # CLI refresh path: make the stored access token unusable; the CLI must refresh on token_expired.
        auth = json.loads(path.read_text())
        before = auth["refresh_token"]
        auth["access_token"] = "oat_" + b64(secrets.token_bytes(32))
        private(path, auth)
        c.bridge("chef", "login", "status", "--json")
        _, out, _ = c.bridge("chef", "device", "ls")
        after = json.loads(path.read_text())
        if after["refresh_token"] == before or not after["access_token"].startswith("oat_"):
            raise RuntimeError("CLI did not refresh")
        c.ok("CLI refreshed through real IAM after token_expired and retried", "device ls succeeded after refresh")
        # The session was started with the old token; Bridge keeps it working with the new login.
        rc, out, err = c.bridge("chef", "session", "new", ctx["device"], "--connect", check=False)
        if rc == 0:
            ctx["session"] = out.strip()
        c.bridge("chef", "snapshot")
        c.ok("snapshot after refresh works", ctx.get("session", ""))
    c.step("refresh", refresh)

    def negatives():
        status, v = http("POST", c.api + "/api/v1/auth/login", envelope("login", {"slt": "si:chef"}), headers={"Idempotency-Key": str(uuid.uuid4())})
        code = err_code(v)
        if status < 400 or code != "slt_invalid":
            raise RuntimeError(f"bare member id login -> {status} {v}")
        c.ok("production login with a bare member id is refused", f"HTTP {status} {code}")
        status, v = http("POST", c.api + "/api/v1/auth/login", envelope("login", {"slt": "slt_" + b64(secrets.token_bytes(24))}), headers={"Idempotency-Key": str(uuid.uuid4())})
        code = err_code(v)
        if status < 400:
            raise RuntimeError(f"forged SLT accepted: {v}")
        c.ok("an SLT IAM never issued is refused", f"HTTP {status} {code}")
        slt = c.slt("sous")
        status, v = http("POST", c.api + "/api/v1/auth/login", envelope("login", {"slt": slt}), headers={"Idempotency-Key": str(uuid.uuid4())}, expected=(200,))
        status, v2 = http("POST", c.api + "/api/v1/auth/login", envelope("login", {"slt": slt}), headers={"Idempotency-Key": str(uuid.uuid4())})
        code = err_code(v2)
        if status < 400:
            raise RuntimeError("SLT accepted twice")
        c.ok("an SLT is single-use", f"second exchange HTTP {status} {code}")
        tok = v["data"]["access_token"]
        http("POST", c.api + "/api/v1/auth/logout", envelope("logout", {"token": tok}), token=tok, expected=(204,))
        time.sleep(0.5)
        status, v = http("GET", c.api + "/api/v1/auth/me", token=tok)
        code = err_code(v)
        if status != 401 or code != "token_expired":
            raise RuntimeError(f"revoked token -> {status} {v}")
        c.ok("a revoked access token gets token_expired", f"HTTP {status} {code}")
        status, v = http("GET", c.api + "/api/v1/auth/me", token="oat_" + b64(secrets.token_bytes(32)))
        code = err_code(v)
        if status != 401 or code != "token_expired":
            raise RuntimeError(f"unknown token -> {status} {v}")
        c.ok("an unknown/expired access token gets token_expired", f"HTTP {status} {code}")
    c.step("negatives", negatives)

    def webhooks():
        dev = ctx["device"]
        # 1. Remove si:sous from acme through IAM's real API; IAM must tell Bridge, Bridge must drop sous's access.
        since = len(c.bridge_log())
        token = state["direct"]["alice"]["access_token"]
        _, silicon = http("GET", state["iam_url"] + "/api/v1/organizations/acme/silicons/si:sous", token=token, expected=(200,))
        assertion = step_up(state, "alice", "organization.authorization_change", "si:sous[acme]")
        status, v = http("DELETE", state["iam_url"] + "/api/v1/organizations/acme/silicons/si:sous", token=token,
                         headers={"Idempotency-Key": str(uuid.uuid4()), "If-Match": f'"{silicon["version"]}"',
                                  "X-Step-Up-Token": assertion})
        ctx["remove_sous"] = (status, v)
        if status >= 300:
            raise RuntimeError(f"IAM refused removing si:sous: HTTP {status} {v}")
        c.ok("IAM removed si:sous from acme (real API)", f"HTTP {status}")
        log = c.wait_log("IAM event", since)
        if not log:
            raise RuntimeError("no IAM webhook reached Bridge within 45 s (see worker logs)")
        events = [l for l in log.splitlines() if "IAM event" in l]
        how = ("signature verified by the official WebhookVerifier; body read by Bridge because SDK 4.0.0 refuses "
               "public-id aggregate ids" if "event read by Bridge" in log else "verified and parsed by the official WebhookVerifier")
        c.ok("IAM delivered a signed webhook and Bridge accepted it", how + " — " + events[0].split("IAM event", 1)[1][-200:])
        for _ in range(30):
            _, out, _ = c.bridge("alice", "device", "access", "ls", dev)
            if "si:sous" not in out:
                break
            time.sleep(1)
        if "si:sous" in out:
            raise RuntimeError(f"si:sous still has access: {out}")
        c.ok("Bridge removed si:sous's device access after the webhook", out.strip().replace("\n", " | ")[:120])
    c.step("webhook: Silicon removed", webhooks)

    def revoked_elsewhere():
        # Revoke chef's IAM refresh family directly at IAM (as another client of Bridge's app would);
        # IAM sends applications no webhook for token revocation, so Bridge notices on its next
        # live authorization, at most 30 s later (its authorization cache bound).
        auth = json.loads(c.auth_file("chef").read_text())
        basic = "Basic " + base64.b64encode(f"bridge:{state['app_secret']}".encode()).decode()
        form = urllib.parse.urlencode({"token": auth["refresh_token"], "token_type_hint": "refresh_token"}).encode()
        http("POST", state["iam_url"] + "/api/v1/oauth/revoke", form, expected=(200, 204),
             headers={"Authorization": basic, "Idempotency-Key": str(uuid.uuid4()), "Content-Type": "application/x-www-form-urlencoded"})
        c.ok("IAM revoked si:chef's refresh family directly (not through Bridge)")
        time.sleep(31)
        rc, out, err = c.bridge("chef", "device", "ls", check=False)
        if rc != 3 or "token_expired" not in err:
            raise RuntimeError(f"chef still works after revocation: exit {rc} {out[-200:]} {err[-300:]}")
        c.ok("after ≤30 s Bridge refuses the revoked login (CLI refresh also refused)", f"exit {rc} token_expired")
        c.bridge("chef", "login", c.slt("chef"))
        _, sid, _ = c.bridge("chef", "session", "new", ctx["device"], "--connect", check=False)
        ctx["session"] = sid.strip() or ctx.get("session")
    c.step("revoked elsewhere", revoked_elsewhere)

    def webhook_logout():
        # 2. si:chef logs out through Bridge → IAM revokes → running session ends.
        since = len(c.bridge_log())
        c.bridge("chef", "logout")
        rc, out, err = c.bridge("alice", "device", "show", ctx["device"], check=False)
        c.ok("si:chef logged out (IAM revocation)", "")
        log = c.wait_log("IAM event", since, timeout=20)
        c.notes.append("webhook after logout: " + ("; ".join(l[-200:] for l in log.splitlines() if "IAM event" in l) if log else "none within 20 s"))
        _, out, _ = c.bridge("alice", "device", "activity", ctx["device"])
        if "logged_out" in out or "silicon_logged_out" in out or "ended" in out:
            c.ok("chef's running session ended on logout", [l for l in out.splitlines() if "end" in l.lower()][-1][:120] if out else "")
        else:
            raise RuntimeError(f"session not ended: {out[-400:]}")
    c.step("logout ends the session", webhook_logout)

    def forged():
        body = json.dumps({"event_id": str(uuid.uuid4()), "event_type": "organization.membership.removed.v1"}).encode()
        ts = str(int(time.time()))
        sig = "v1=" + hmac.new(b"not-the-secret-not-the-secret-000", f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
        status, v = http("POST", c.api + "/webhook/", body, headers={"X-Silicon-IAM-Signature": sig, "X-Silicon-IAM-Timestamp": ts,
                                                                    "X-Silicon-IAM-Key-Version": "1", "X-Silicon-IAM-Event-ID": str(uuid.uuid4())})
        if status < 400:
            raise RuntimeError(f"forged webhook accepted: {status}")
        c.ok("a webhook signed with the wrong secret is rejected", f"HTTP {status}")
        # A correctly signed event in the SDK's own shape (UUID aggregate): the official verifier's
        # parse path, and at-least-once delivery (the same event twice is applied once).
        eid = str(uuid.uuid4())
        body = json.dumps({"spec_version": "1.0", "event_id": eid, "event_type": "organization.updated.v1",
                           "occurred_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                           "organization_id": ORG_UUID, "aggregate": {"type": "organization", "id": ORG_UUID, "version": 9},
                           "data": {"current": {"organization": {"org_id": "acme"}}}}, separators=(",", ":")).encode()
        since = len(c.bridge_log())
        for _ in range(2):
            ts = str(int(time.time()))
            sig = "v1=" + hmac.new(state["webhook_secret"].encode(), f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
            http("POST", c.api + "/webhook/", body, expected=(204,), headers={"X-Silicon-IAM-Signature": sig, "X-Silicon-IAM-Timestamp": ts,
                 "X-Silicon-IAM-Key-Version": "1", "X-Silicon-IAM-Event-ID": eid})
        log = c.bridge_log()[since:]
        applied = [l for l in log.splitlines() if "IAM event" in l and eid in l]
        if len(applied) != 1 or "event read by Bridge" in log:
            raise RuntimeError(f"expected one IAM event via the SDK parser, log: {log[-600:]}")
        c.ok("a signed SDK-shaped event passes the official verifier; a duplicate delivery is applied once", eid)
    c.step("forged webhook", forged)

    # ── Testing plane ──
    tctx = {}

    def tiam(method, path, body=None, token=None, headers=None, expected=(200, 201, 202, 204)):
        return http(method, state["iam_url"] + "/api/v1" + path, body, token=token, expected=expected,
                    headers={"X-Testing-Environment-Key": tctx["key"], "Idempotency-Key": str(uuid.uuid4()), **(headers or {})})[1]

    def testing_setup():
        # Bridge's production credential asks IAM for a test environment; IAM imports Bridge into it.
        auth = "Basic " + base64.b64encode(f"bridge:{state['app_secret']}".encode()).decode()
        _, env = http("POST", state["iam_url"] + "/api/v1/application/testing-environments",
                      {"name": "bridge-realiam", "description": "Bridge real-IAM run"},
                      headers={"Authorization": auth, "Idempotency-Key": str(uuid.uuid4())}, expected=(200, 201))
        tctx.update(env=env["environment_id"], key=env["iam_test_key"], secret=env["app_secret"])
        private(STATE_DIR / "testing.private.json", env)
        c.ok("IAM created a test environment from Bridge's application credential", f"{env['environment_id']} app={env['app_id']}")
        # Honeycomb's lifecycle instruction to Bridge (Bridge's internal participant API).
        op = str(uuid.uuid4())
        http("PUT", f"{c.api}/internal/honeycomb/organizations/acme/testing-environments/{tctx['env']}/operations/{op}",
             {"operation_id": op, "environment_id": tctx["env"], "org_id": "acme", "app_id": "bridge", "environment_revision": 1,
              "generation": 1, "key_version": 1, "action": "prepare", "testing_key": tctx["key"], "name": "bridge-realiam"},
             headers={"Authorization": "Bearer " + state["honeycomb_token"]}, expected=(200,))
        c.ok("Bridge prepared the test environment (Honeycomb operation)")
        # Test identities through IAM's real signup and login (fixed code 000000 in the test plane).
        sid = tiam("POST", "/signup/sessions", {})["session_id"]
        for step, body in (("/email", {"email": "alice@example.invalid"}), ("/email/verify", {"code": "000000"}),
                           ("/phone", {"phone_number": "+12025550143"}), ("/phone/verify", {"code": "000000"})):
            tiam("POST", f"/signup/sessions/{sid}{step}", body)
        tiam("POST", f"/signup/sessions/{sid}/complete", {"carbon_id": "c:alice", "display_name": "Alice (test)"})
        ch = tiam("POST", "/login/challenges", {"carbon_id": "c:alice"})
        tctx["alice_direct"] = tiam("POST", f"/login/challenges/{ch['session_id']}/verify", {"code": "000000"})["access_token"]
        # The test world's acme is owned by IAM's synthetic importer; the test Carbon runs its own team.
        tiam("POST", "/organizations", {"org_id": "kitchen", "name": "Kitchen (test)"}, token=tctx["alice_direct"])
        for handle in ("chef", "sous"):
            tiam("POST", "/organizations/kitchen/silicons", {"silicon_id": handle, "display_name": handle.title() + " (test)",
                                                             "job_description": "Bridge test"}, token=tctx["alice_direct"])
        c.ok("test plane: c:alice signed up (OTP 000000), team kitchen with si:chef and si:sous created through IAM")
    c.step("testing: setup", testing_setup)

    def testing_bridge():
        env = tctx["env"]
        home = c.work / "homes"
        for who in ("talice", "tchef"):
            (home / who).mkdir(parents=True, exist_ok=True)
            e = {k: v for k, v in os.environ.items() if not k.startswith("BRIDGE_")}
            e.update({"BRIDGE_API_URL": c.api, "BRIDGE_TELEMETRY": "off", "SILICON_HOME": str(home / who)})
            run([str(TARGET / "debug" / "bridge"), "config", "test", "add", env], stdin=tctx["secret"].encode(), env=e)
        c.ok("bridge config test add (secret on stdin)")
        status, v = http("GET", c.api + "/api/v1/testing-environment", headers={"X-Testing-Application-Secret": tctx["secret"]})
        if status != 200 or v["data"]["environment_id"] != env:
            raise RuntimeError(f"testing-environment -> {status} {v}")
        c.ok("X-Testing-Application-Secret selects the environment (select_testing → IAM testing_context)", v["data"]["name"])
        status, v = http("GET", c.api + "/api/v1/testing-environment", headers={"X-Testing-Application-Secret": "ask_" + b64(secrets.token_bytes(32))})
        if status < 400:
            raise RuntimeError("an unknown test secret was accepted")
        c.ok("an unknown test application secret is refused", f"HTTP {status} {err_code(v)}")
        status, v = http("GET", c.api + "/api/v1/testing-environment", headers={"X-Testing-Application-Secret": state["app_secret"]})
        if status < 400:
            raise RuntimeError("the production application secret selected a test environment")
        c.ok("the production application secret does not select a test environment", f"HTTP {status} {err_code(v)}")
        c.bridge("talice", "login", "c:alice", test=env)
        _, out, _ = c.bridge("talice", "login", "status", "--json", test=env)
        d = json.loads(out)["data"]
        if not d["authenticated"] or d["member"]["id"] != "c:alice" or d["team"] != "kitchen":
            raise RuntimeError(d)
        c.ok("member-id login in the test plane (bridge --test … login c:alice)", f"team={d['team']} role={d['team_role']}")
        c.bridge("tchef", "login", "si:chef", test=env)
        c.ok("member-id login in the test plane for a Silicon (si:chef)")
        tok = json.loads(c.auth_file("talice", test=env).read_text())["auth"]["access_token"]
        status, v = http("GET", c.api + "/api/v1/team/silicons", token=tok, headers={"X-Org-ID": "kitchen", "X-Testing-Application-Secret": tctx["secret"]})
        ids = sorted(i["id"] for i in v["data"]["items"]) if status == 200 else v
        if ids != ["si:chef", "si:sous"]:
            raise RuntimeError(f"test team silicons: {status} {ids}")
        c.ok("test-plane team Silicons come from the test IAM directory", str(ids))
        status, v = http("GET", c.api + "/api/v1/auth/me", token=tok)
        if status != 401:
            raise RuntimeError(f"a test-plane token worked in production: {status}")
        c.ok("a test-plane token is refused in production", f"HTTP {status} {err_code(v)}")
        prod = json.loads(c.auth_file("alice").read_text())["access_token"]
        status, v = http("GET", c.api + "/api/v1/auth/me", token=prod, headers={"X-Testing-Application-Secret": tctx["secret"]})
        if status != 401:
            raise RuntimeError(f"a production token worked in the test plane: {status}")
        c.ok("a production token is refused in the test plane", f"HTTP {status} {err_code(v)}")
        log = c.work / "tfake.log"
        tctx["fake"] = subprocess.Popen([str(TARGET / "debug" / "examples" / "fake_device"), c.api, "linux", tctx["secret"]],
                                        stdout=open(log, "wb"), stderr=subprocess.STDOUT)
        code = None
        for _ in range(100):
            txt = log.read_text(errors="replace")
            if "PAIRING_CODE" in txt:
                code = txt.split("PAIRING_CODE", 1)[1].split()[0]
                break
            time.sleep(0.2)
        _, out, _ = c.bridge("talice", "device", "pair", code, "--name", "Test box", "--access", "si:chef", "--access", "si:sous", "--json", test=env)
        tctx["device"] = json.loads(out)["data"]["device_id"]
        time.sleep(1)
        c.ok("device paired into the test environment with access for test si:chef and si:sous", tctx["device"])
        _, out, _ = c.bridge("alice", "device", "ls")
        if "Test box" in out:
            raise RuntimeError("production sees the test device")
        c.ok("production does not see the test device")
        _, sid, _ = c.bridge("tchef", "session", "new", tctx["device"], "--connect", test=env)
        _, out, _ = c.bridge("tchef", "snapshot", test=env)
        c.ok("test-plane si:chef session + snapshot", sid.strip())
    c.step("testing: Bridge", testing_bridge)

    def testing_webhook():
        # Remove the test si:sous; IAM signs the test delivery (inherited webhook secret) and wraps it
        # in {"test": …}. Bridge must route it to the test world by its key, never to production.
        since = len(c.bridge_log())
        silicon = tiam("GET", "/organizations/kitchen/silicons/si:sous", token=tctx["alice_direct"])
        hdr = {"If-Match": f'"{silicon["version"]}"'}
        status, v = http("DELETE", state["iam_url"] + "/api/v1/organizations/kitchen/silicons/si:sous", token=tctx["alice_direct"],
                         headers={"X-Testing-Environment-Key": tctx["key"], "Idempotency-Key": str(uuid.uuid4()), **hdr})
        if status == 428 and err_code(v) == "step_up_required":
            _, chl = http("POST", state["iam_url"] + "/api/v1/step-up/challenges", {"action": "organization.authorization_change",
                          "resource_id": "si:sous[kitchen]", "channel": "email"}, token=tctx["alice_direct"],
                          headers={"X-Testing-Environment-Key": tctx["key"], "Idempotency-Key": str(uuid.uuid4())}, expected=(200, 201))
            _, su = http("POST", state["iam_url"] + f"/api/v1/step-up/challenges/{chl['session_id']}/verify", {"code": chl.get("local_otp", "000000")},
                         token=tctx["alice_direct"], headers={"X-Testing-Environment-Key": tctx["key"], "Idempotency-Key": str(uuid.uuid4())}, expected=(200, 201))
            status, v = http("DELETE", state["iam_url"] + "/api/v1/organizations/kitchen/silicons/si:sous", token=tctx["alice_direct"],
                             headers={"X-Testing-Environment-Key": tctx["key"], "Idempotency-Key": str(uuid.uuid4()), **hdr,
                                      "X-Step-Up-Token": su["step_up_token"]})
        if status >= 300:
            raise RuntimeError(f"test-plane removal: {status} {v}")
        c.ok("test plane: IAM removed test si:sous from kitchen", f"HTTP {status}")
        log = c.wait_log("IAM event", since)
        if not log:
            raise RuntimeError("no test-plane IAM webhook reached Bridge within 45 s")
        line = [l for l in log.splitlines() if "IAM event" in l][0]
        if tctx["env"] not in line:
            raise RuntimeError(f"test event not routed to the test world: {line}")
        c.ok("IAM delivered the signed test-plane webhook; Bridge routed it to the test world by its key", line.split("IAM event", 1)[1][-200:])
        for _ in range(20):
            _, out, _ = c.bridge("talice", "device", "access", "ls", tctx["device"], test=env_id())
            if "si:sous" not in out:
                break
            time.sleep(1)
        if "si:sous" in out:
            raise RuntimeError(f"test si:sous still has access: {out}")
        c.ok("Bridge removed test si:sous's access in the test world only", out.strip().replace("\n", " | ")[:120])
        _, out, _ = c.bridge("alice", "device", "access", "ls", ctx["device"])
        if "si:chef" not in out:
            raise RuntimeError(f"production access changed: {out}")
        c.ok("production access untouched by the test-plane event")

    def env_id():
        return tctx["env"]
    c.step("testing: webhook", testing_webhook)
    if tctx.get("fake"):
        tctx["fake"].terminate()

    if ctx.get("fake"):
        ctx["fake"].terminate()
    report = {"passed": c.passed, "failed": c.failed, "notes": c.notes, "iam_image": IAM_IMAGE,
              "at": datetime.datetime.now(datetime.timezone.utc).isoformat()}
    private(STATE_DIR / "report.json", report)
    print(f"\n{len(c.passed)} passed, {len(c.failed)} failed")
    for n in c.notes:
        print("  note:", n)
    return not c.failed


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("command", choices=["up", "check", "down", "all", "restart-bridge"])
    p.add_argument("--keep", action="store_true", help="with `all`: leave everything running afterwards")
    a = p.parse_args()
    os.umask(0o077)
    if a.command == "up":
        up(a)
    elif a.command == "down":
        down()
    elif a.command == "restart-bridge":
        state = load()
        stop_bridge(state)
        start_bridge(state)
    elif a.command == "check":
        sys.exit(0 if check(a) else 1)
    else:
        good = False
        try:
            up(a)
            good = check(a)
        finally:
            if not a.keep:
                down()
        sys.exit(0 if good else 1)


if __name__ == "__main__":
    main()
