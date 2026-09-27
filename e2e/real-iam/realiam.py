#!/usr/bin/env python3
"""Silicon Extend against a real, disposable Silicon IAM.

    e2e/real-iam/realiam.py up      # IAM (Postgres, API, worker, webhook relay) + an Extend on :8497
    e2e/real-iam/realiam.py check   # the verification run against what `up` started
    e2e/real-iam/realiam.py down    # stop Extend, remove this fixture's containers/network/database
    e2e/real-iam/realiam.py all     # up, check, down (down runs even when check fails; --keep skips it)

    e2e/real-iam/realiam.py build-minio               # once, for --briefcase
    e2e/real-iam/realiam.py all --briefcase --ting    # + a real Briefcase (MinIO) and a real Ting

`--briefcase` and `--ting` switch Extend to EXTEND_FILES_MODE=briefcase / EXTEND_TING_MODE=ting
against real services reached only through IAM OBO proofs (see README.md, "Briefcase and Ting lanes").

`check` also covers IAM's testing plane: Extend's production credential creates a test
environment, test identities sign up through IAM (code 000000), Extend selects the environment
from X-Testing-Application-Secret, and a signed test-plane webhook is routed to the test world.

Everything is owned by this fixture: one Docker network, its containers (four, plus six with both
lanes), one database on the Extend Postgres (`silicon-extend-postgres`, :5440). Nothing reads
existing credentials. Identity rows (team acme, c:alice, c:bob, si:chef, si:sous, the `extend`
application — and with lanes `briefcase` and `ting` — their secrets, scopes, OBO catalogs and
webhook endpoints) are seeded by SQL into the disposable IAM; SLT issuance, login, refresh, revocation,
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
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target"))
# `up` copies the three binaries here, so another build of the shared target dir cannot swap them mid-run.
BINARIES = {"extend-service": ("debug", "extend-service"), "extend": ("debug", "extend"),
            "fake_device": ("debug", "examples", "fake_device")}


def binary(name):
    return str(STATE_DIR / "bin" / name)
IAM_IMAGE = os.environ.get("REALIAM_IAM_IMAGE", "silicon-iam:release-433665db296d4cdb26b846e2db97ede81066bf09")
PG_IMAGE = "postgres:16.15-bookworm"
IAM_CLI = os.environ.get("REALIAM_IAM_CLI", str(Path.home() / ".silicon" / "bin" / "iam"))
NAME = "extend-realiam"
# IAM delivers webhooks only to public addresses (its SSRF guard runs in development too), so the
# fixture network uses a range IAM considers public. It is an isolated Docker extend; nothing routes out.
SUBNET = os.environ.get("REALIAM_SUBNET", "100.128.7.0/24")
RELAY_IP = SUBNET.rsplit(".", 1)[0] + ".10"
EXTEND_PORT = int(os.environ.get("REALIAM_EXTEND_PORT", "8497"))
EXTEND_DB_CONTAINER = "silicon-extend-postgres"
EXTEND_DB = "extend_realiam"
EXTEND_DB_URL = f"postgres://extend:extend@127.0.0.1:5440/{EXTEND_DB}"
ORG_UUID = "3f1b1a52-7a55-4c55-8f4e-0b1d9d7a5c01"
ACTORS = {"alice": ("c:alice", "carbon", "owner"),
          "chef": ("si:chef", "silicon", "member"),
          "sous": ("si:sous", "silicon", "member"),
          # A Carbon who is a plain member: the Briefcase lane pairs a device as bob to see exactly
          # what sharing grants a device owner who is not also a Team owner/admin.
          "bob": ("c:bob", "carbon", "member")}
EXTEND_SCOPES = ["self.identity.read", "self.profile.read", "self.organizations.read", "self.membership.read",
                 "directory.silicons.read", "directory.carbons.read", "directory.memberships.read",
                 "directory.profiles.read"]

# ── Optional lanes: real Silicon Briefcase (with MinIO) and real Ting, reached through IAM OBO ──
CACHE_DIR = HERE / ".cache"
BRIEFCASE_IMAGE = os.environ.get("REALIAM_BRIEFCASE_IMAGE", "briefcase-backend:candidate")
# MinIO no longer publishes images or binaries (quay.io answers 401, dl.min.io answers 410), so the
# fixture builds the release Briefcase's compose.yaml pins from its official source (`build-minio`).
MINIO_RELEASE = "RELEASE.2025-07-23T15-54-02Z"
MINIO_IMAGE = os.environ.get("REALIAM_MINIO_IMAGE", f"extend-realiam-minio:{MINIO_RELEASE}")
GO_IMAGE = "golang:1.24-bookworm"
# Ting 0.1.9's published Linux ARM64 server (github.com/teamofsilicons/silicon-ting releases).
TING_COMMIT = os.environ.get("REALIAM_TING_COMMIT", "6853b4e247f434e358f4bbd05e5d23d5ae7870cd")
TING_SHA256 = os.environ.get("REALIAM_TING_SHA256", "069e71b1d40b4bec4b045155a5a73c2aed54c1f4dcd5440425b5abc99fbaec2e")
TING_RUNTIME_IMAGE = "debian:bookworm-slim"
TING_TYPE = "extend.device.requested"
# Briefcase's IAM OBO catalog as its docs/obo.md registers it: endpoint → (path, metadata schema, critical).
BRIEFCASE_ENDPOINTS = {
    "briefcase.files.create": ("/api/v1/obo/files", {"path": {"type": "string"}, "name": {"type": "string"},
                                                     "content_type": {"type": "string"}}, False),
    "briefcase.invitations.create": ("/api/v1/obo/invitations", {}, True),
    "briefcase.entries.trash": ("/api/v1/obo/entries/trash", {}, False),
    "briefcase.files.read": ("/api/v1/obo/files/read", {}, False),
}
BRIEFCASE_SCOPES = ["self.identity.read", "self.profile.read", "self.organizations.read", "self.membership.read",
                    "self.tags.read", "directory.carbons.read", "directory.silicons.read",
                    "directory.memberships.read", "directory.tags.read"]
# Ting's catalog (udd/api.md): empty metadata, every endpoint critical. Extend uses these two.
TING_ENDPOINTS = {"tings.send": "/v1/tings", "subscriptions.register": "/v1/subscriptions"}
TING_SCOPES = ["self.identity.read", "self.organizations.read", "self.membership.read"]


def lanes(state):
    return set(state.get("lanes", []))


def extend_external(state):
    """Extend's `app_scope.external`: the Briefcase and Ting endpoints it may call for a member."""
    out = []
    if "briefcase" in lanes(state):
        out += [{"app_id": "briefcase", "endpoint_id": e} for e in BRIEFCASE_ENDPOINTS]
    if "ting" in lanes(state):
        out += [{"app_id": "ting", "endpoint_id": e} for e in TING_ENDPOINTS]
    return out


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
    """Extend's request body shape."""
    return {"type": kind, "data": data}


def err_code(v):
    """The error code from an Extend ({"type":"error","data":{...}}) or IAM ({"error":{...}}) body."""
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
    parts += [f"INSERT INTO iam.carbons(id,carbon_id,display_name) VALUES({q(actor)},{q(actor)},{q(label.title())});"
              for label, (actor, kind, _) in ACTORS.items() if kind == "carbon"]
    parts += [f"INSERT INTO iam.organizations(id,org_id,created_by_carbon_id,name) VALUES('{ORG_UUID}','acme',{q(owner)},'Acme');"]
    # Verified contacts, encrypted as IAM stores them, so step-up (local provider, code 000000) works.
    # IAM requires every active Carbon to hold verified contacts.
    contacts = {"c:alice": ("alice@example.invalid", "+12025550143"), "c:bob": ("bob@example.invalid", "+12025550144")}
    for carbon, (email, phone) in contacts.items():
        for kind, value in (("email", email), ("phone", phone)):
            contact = str(uuid.uuid4())
            ct, nonce = encrypt(enc_key, b"carbon-" + kind.encode(), None, contact, value)
            parts.append(f"INSERT INTO iam.carbon_contacts(id,carbon_id,kind,ciphertext,nonce,encryption_key_version,verified_at) VALUES('{contact}',{q(carbon)},{q(kind)},decode('{ct}','hex'),decode('{nonce}','hex'),1,now());")
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
    def application(app, name, secret, iam_scopes, external, base_url, webhook_scope=None):
        scopes = iam_scopes + [f"obo:{e['app_id']}:{e['endpoint_id']}" for e in external]
        parts.append(f"INSERT INTO iam.principals(id,kind,status,activated_at) VALUES({q(app)},'application','active',now());")
        webhook = f",ARRAY{webhook_scope!r}" if webhook_scope else ""
        parts.append(f"INSERT INTO iam.applications(id,app_id,organization_id,created_by_carbon_id,app_name,review_status,visibility,base_url,app_scope{',webhook_scope' if webhook_scope else ''}) VALUES({q(app)},{q(app)},'{ORG_UUID}',{q(owner)},{q(name)},'verified','public',{q(base_url)},{q(json.dumps({'iam': iam_scopes, 'external': external}))}::jsonb{webhook});")
        parts.append(f"INSERT INTO iam.application_secrets(id,application_id,secret_version,secret_prefix,secret_digest,pepper_key_version,created_by_carbon_id) VALUES('{uuid.uuid4()}',{q(app)},1,{q(secret[:12])},decode('{digest(pepper, 'application-secret', secret)}','hex'),1,{q(owner)});")
        for scope in scopes:
            parts.extend([f"INSERT INTO iam.oauth_scope_catalog(scope,description,sensitive) VALUES({q(scope)},'Extend real-IAM fixture',false) ON CONFLICT DO NOTHING;",
                          f"INSERT INTO iam.application_requested_scopes(application_id,scope) VALUES({q(app)},{q(scope)});",
                          f"INSERT INTO iam.application_approved_scopes(application_id,scope,approved_by_carbon_id) VALUES({q(app)},{q(scope)},{q(owner)});"])

    def endpoints(app, catalog):
        # The audience's OBO catalog, as Honeycomb's accepted configuration would publish it.
        for endpoint, (path, metadata, critical) in catalog.items():
            parts.append(f"INSERT INTO iam.application_obo_endpoints(organization_id,application_id,endpoint_id,path,metadata_definition,critical,ttl_seconds) VALUES('{ORG_UUID}',{q(app)},{q(endpoint)},{q(path)},{q(json.dumps(metadata))}::jsonb,{'true' if critical else 'false'},60);")

    app, secret = "extend", state["app_secret"]
    application(app, "Silicon Extend", secret, EXTEND_SCOPES, extend_external(state), f"http://127.0.0.1:{EXTEND_PORT}", ["full"])
    if "briefcase" in lanes(state):
        application("briefcase", "Silicon Briefcase", state["briefcase"]["app_secret"], BRIEFCASE_SCOPES, [], state["briefcase"]["url"])
        endpoints("briefcase", BRIEFCASE_ENDPOINTS)
    if "ting" in lanes(state):
        application("ting", "Silicon Ting", state["ting"]["app_secret"], TING_SCOPES, [], state["ting"]["url"])
        endpoints("ting", {e: (p, {}, True) for e, p in TING_ENDPOINTS.items()})
    def webhook(app, url, secret):
        # The webhook endpoint and signing key, encrypted exactly as IAM stores them. (IAM's own
        # registration API accepts only public HTTPS URLs; this fixture delivers over plain HTTP to a relay.)
        endpoint, key_id = str(uuid.uuid4()), str(uuid.uuid4())
        url_ct, url_nonce = encrypt(enc_key, b"application-webhook-url", app, endpoint, url)
        sec_ct, sec_nonce = encrypt(enc_key, b"application-webhook-signing-secret", app, key_id, secret)
        prefix = "whs_" + hashlib.sha256(secret.encode()).hexdigest()[:8]
        parts.append(f"INSERT INTO iam.application_webhook_endpoints(id,application_id,url_ciphertext,url_nonce,encryption_key_version,url_digest,status,activated_at) VALUES('{endpoint}',{q(app)},decode('{url_ct}','hex'),decode('{url_nonce}','hex'),1,decode('{hashlib.sha256(url.encode()).hexdigest()}','hex'),'active',now());")
        parts.append(f"INSERT INTO iam.application_webhook_signing_keys(id,application_id,endpoint_id,secret_version,key_prefix,secret_ciphertext,secret_nonce,encryption_key_version) VALUES('{key_id}',{q(app)},'{endpoint}',1,{q(prefix)},decode('{sec_ct}','hex'),decode('{sec_nonce}','hex'),1);")

    webhook(app, state["webhook_url"], state["webhook_secret"])
    for lane in ("briefcase", "ting"):
        if lane in lanes(state):
            webhook(lane, state[lane]["webhook_url"], state[lane]["webhook_secret"])
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


def free_port():
    import socket
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def image_exists(image):
    return run(["docker", "image", "inspect", image], check=False).returncode == 0


def build_minio(_args=None):
    """Builds MINIO_IMAGE from MinIO's official source at MINIO_RELEASE (Go in Docker, ~2 min)."""
    if image_exists(MINIO_IMAGE):
        print(f"• {MINIO_IMAGE} already exists")
        return
    work = CACHE_DIR / "minio-build"
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    print(f"• cloning github.com/minio/minio at {MINIO_RELEASE}")
    run(["git", "clone", "-q", "--depth", "1", "--branch", MINIO_RELEASE, "https://github.com/minio/minio", str(work / "src")], timeout=600)
    print(f"• building minio with {GO_IMAGE}")
    (work / "gomod").mkdir()
    run(["docker", "run", "--rm", "-v", f"{work / 'src'}:/src", "-v", f"{work / 'gomod'}:/go/pkg/mod", "-w", "/src",
         "-e", "CGO_ENABLED=0", GO_IMAGE, "go", "build", "-trimpath", "-ldflags", "-s -w", "-o", "/src/minio-bin", "."], timeout=1800)
    (work / "image").mkdir()
    shutil.copy2(work / "src" / "minio-bin", work / "image" / "minio")
    (work / "image" / "Dockerfile").write_text(
        f"FROM {TING_RUNTIME_IMAGE}\nCOPY minio /usr/bin/minio\n"
        f"LABEL org.opencontainers.image.source=https://github.com/minio/minio org.opencontainers.image.version={MINIO_RELEASE}\n"
        "ENTRYPOINT [\"/usr/bin/minio\"]\n")
    run(["docker", "build", "-q", "-t", MINIO_IMAGE, str(work / "image")], timeout=600)
    shutil.rmtree(work, ignore_errors=True)
    print(f"• built {MINIO_IMAGE}")


def s3_create_bucket(endpoint, bucket, access, secret, region="us-east-1"):
    """PUT Bucket with a hand-rolled AWS SigV4 signature (no SDK needed)."""
    now = datetime.datetime.now(datetime.timezone.utc)
    amz, day = now.strftime("%Y%m%dT%H%M%SZ"), now.strftime("%Y%m%d")
    host = urllib.parse.urlparse(endpoint).netloc
    empty = hashlib.sha256(b"").hexdigest()
    signed = "host;x-amz-content-sha256;x-amz-date"
    canonical = f"PUT\n/{bucket}\n\nhost:{host}\nx-amz-content-sha256:{empty}\nx-amz-date:{amz}\n\n{signed}\n{empty}"
    scope = f"{day}/{region}/s3/aws4_request"
    to_sign = f"AWS4-HMAC-SHA256\n{amz}\n{scope}\n{hashlib.sha256(canonical.encode()).hexdigest()}"
    key = ("AWS4" + secret).encode()
    for part in (day, region, "s3", "aws4_request"):
        key = hmac.new(key, part.encode(), hashlib.sha256).digest()
    signature = hmac.new(key, to_sign.encode(), hashlib.sha256).hexdigest()
    auth = f"AWS4-HMAC-SHA256 Credential={access}/{scope}, SignedHeaders={signed}, Signature={signature}"
    status, value = http("PUT", f"{endpoint}/{bucket}", b"", headers={"Authorization": auth, "x-amz-date": amz,
                         "x-amz-content-sha256": empty, "Content-Type": ""})
    if status not in (200, 409):
        raise RuntimeError(f"creating bucket {bucket}: HTTP {status} {str(value)[:300]}")


# Briefcase and Ting (through the official IAM client) accept plain-HTTP IAM only on a literal
# loopback address, so both run inside the IAM container's network namespace and reach IAM at
# 127.0.0.1:8080; their own listeners are published through the IAM container.
BRIEFCASE_BIND_PORT, TING_BIND_PORT = 8083, 8082


def lane_relay_ip(lane):
    """Each lane's webhook relay, at an address IAM treats as public (see SUBNET)."""
    return SUBNET.rsplit(".", 1)[0] + (".11" if lane == "briefcase" else ".12")


def start_lane_relay(state, lane, port):
    """Relays IAM's webhook deliveries to a lane service listening in IAM's network namespace."""
    run(["docker", "run", "-d", "--name", f"{NAME}-{lane}-relay", "--network", NAME, "--ip", lane_relay_ip(lane),
         "--entrypoint", "perl", PG_IMAGE, "-e", RELAY, str(port), f"iam:{port}"])
    state["containers"].append(f"{NAME}-{lane}-relay"); save(state)


def lane_ports(state):
    out = []
    if "briefcase" in lanes(state):
        out += ["-p", f"127.0.0.1:{state['briefcase']['port']}:{BRIEFCASE_BIND_PORT}"]
    if "ting" in lanes(state):
        out += ["-p", f"127.0.0.1:{state['ting']['port']}:{TING_BIND_PORT}"]
    return out


def plan_lanes(state, args):
    """Chooses ports and credentials for the optional lanes before IAM is seeded with them."""
    chosen = [lane for lane in ("briefcase", "ting") if getattr(args, lane, False)]
    state["lanes"] = chosen
    if "briefcase" in chosen:
        for image, hint in ((BRIEFCASE_IMAGE, "build it from silicon-briefcase (Dockerfile) or set REALIAM_BRIEFCASE_IMAGE"),
                            (MINIO_IMAGE, "run `realiam.py build-minio` first")):
            if not image_exists(image):
                raise SystemExit(f"missing Docker image {image}: {hint}")
        port = free_port()
        state["briefcase"] = {"port": port, "url": f"http://127.0.0.1:{port}", "app_secret": "ask_" + b64(secrets.token_bytes(32)),
                              "webhook_url": f"http://{lane_relay_ip('briefcase')}:{BRIEFCASE_BIND_PORT}/webhook/",
                              "webhook_secret": "whs_" + secrets.token_urlsafe(32),
                              "bucket": "briefcase-realiam", "s3_access": "extend-realiam", "s3_secret": secrets.token_hex(20),
                              "db_password": secrets.token_urlsafe(18), "worker_password": secrets.token_urlsafe(18)}
    if "ting" in chosen:
        if not image_exists(TING_RUNTIME_IMAGE):
            run(["docker", "pull", TING_RUNTIME_IMAGE], timeout=600)
        port = free_port()
        state["ting"] = {"port": port, "url": f"http://127.0.0.1:{port}", "app_secret": "ask_" + b64(secrets.token_bytes(32)),
                         "webhook_url": f"http://{lane_relay_ip('ting')}:{TING_BIND_PORT}/v1/iam/webhook",
                         "webhook_secret": "whs_" + secrets.token_urlsafe(32)}


def up_briefcase(state, db_pw):
    """MinIO (SSE-S3 via a local KMS key) + Briefcase's migrator, API and worker on the fixture network."""
    bc = state["briefcase"]
    kms = "extend-realiam-key:" + base64.b64encode(secrets.token_bytes(32)).decode()
    run(["docker", "run", "-d", "--name", f"{NAME}-minio", "--network", NAME, "--network-alias", "minio",
         "-p", "127.0.0.1::9000", "-e", f"MINIO_ROOT_USER={bc['s3_access']}", "-e", f"MINIO_ROOT_PASSWORD={bc['s3_secret']}",
         "-e", f"MINIO_KMS_SECRET_KEY={kms}", MINIO_IMAGE, "server", "/data"])
    state["containers"].append(f"{NAME}-minio"); save(state)
    minio_port = run(["docker", "port", f"{NAME}-minio", "9000/tcp"]).stdout.decode().split()[0].rsplit(":", 1)[1]
    bc["minio_url"] = f"http://127.0.0.1:{minio_port}"; save(state)
    wait_http(bc["minio_url"] + "/minio/health/ready", "MinIO")
    s3_create_bucket(bc["minio_url"], bc["bucket"], bc["s3_access"], bc["s3_secret"])
    # Briefcase's own database next to IAM's, with its two runtime roles (deploy/postgres/001_runtime_roles.sql).
    psql(f"CREATE ROLE briefcase_api LOGIN PASSWORD {q(bc['db_password'])} NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOBYPASSRLS;"
         f"CREATE ROLE briefcase_worker LOGIN PASSWORD {q(bc['worker_password'])} NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT BYPASSRLS;")
    psql("CREATE DATABASE briefcase;")
    env = {"BRIEFCASE_ENVIRONMENT": "development", "BRIEFCASE_BIND_ADDR": f"0.0.0.0:{BRIEFCASE_BIND_PORT}",
           "BRIEFCASE_PUBLIC_BASE_URL": f"{bc['url']}/api/v1/", "BRIEFCASE_PUBLIC_SITE_BASE_URL": f"{bc['url']}/",
           "BRIEFCASE_DATABASE_URL": f"postgres://briefcase_api:{bc['db_password']}@database:5432/briefcase",
           "BRIEFCASE_WORKER_DATABASE_URL": f"postgres://briefcase_worker:{bc['worker_password']}@database:5432/briefcase",
           "BRIEFCASE_MIGRATOR_DATABASE_URL": f"postgres://postgres:{db_pw}@database:5432/briefcase",
           "BRIEFCASE_IAM_BASE_URL": "http://127.0.0.1:8080/", "BRIEFCASE_IAM_APP_ID": "briefcase",
           "BRIEFCASE_IAM_APP_SECRET": bc["app_secret"], "BRIEFCASE_IAM_REQUEST_TIMEOUT_MS": "4000",
           "BRIEFCASE_IAM_WEBHOOK_SIGNING_SECRET": bc["webhook_secret"], "BRIEFCASE_IAM_WEBHOOK_KEY_VERSION": "1",
           "BRIEFCASE_S3_REGION": "us-east-1", "BRIEFCASE_S3_BUCKET": bc["bucket"], "BRIEFCASE_S3_KEY_PREFIX": "organizations",
           "BRIEFCASE_S3_ENDPOINT_URL": "http://minio:9000", "BRIEFCASE_S3_FORCE_PATH_STYLE": "true",
           "BRIEFCASE_S3_ENCRYPTION_MODE": "sse_s3", "BRIEFCASE_TEMPORARY_DIRECTORY": "/tmp/silicon-briefcase",
           "AWS_ACCESS_KEY_ID": bc["s3_access"], "AWS_SECRET_ACCESS_KEY": bc["s3_secret"], "AWS_REGION": "us-east-1",
           "BRIEFCASE_WORKER_POLL_INTERVAL_MS": "500", "BRIEFCASE_LOG_FILTER": "silicon_briefcase=info,tower_http=warn"}
    private(STATE_DIR / "briefcase.env", "".join(f"{k}={v}\n" for k, v in env.items()))
    print("• migrating Briefcase")
    run(["docker", "run", "--rm", "--network", NAME, "--env-file", str(STATE_DIR / "briefcase.env"), BRIEFCASE_IMAGE, "briefcase-migrate"])
    run(["docker", "run", "-d", "--name", f"{NAME}-briefcase", "--network", f"container:{NAME}-iam",
         "--env-file", str(STATE_DIR / "briefcase.env"), BRIEFCASE_IMAGE, "briefcase-api"])
    state["containers"].append(f"{NAME}-briefcase"); save(state)
    run(["docker", "run", "-d", "--name", f"{NAME}-briefcase-worker", "--network", f"container:{NAME}-iam",
         "--env-file", str(STATE_DIR / "briefcase.env"), BRIEFCASE_IMAGE, "briefcase-worker"])
    state["containers"].append(f"{NAME}-briefcase-worker"); save(state)
    start_lane_relay(state, "briefcase", BRIEFCASE_BIND_PORT)
    for _ in range(120):
        status, _ = probe(bc["url"] + "/readyz")
        if status == 200:
            break
        if run(["docker", "inspect", "-f", "{{.State.Running}}", f"{NAME}-briefcase"]).stdout.decode().strip() != "true":
            raise RuntimeError("Briefcase API exited:\n" + run(["docker", "logs", "--tail", "40", f"{NAME}-briefcase"], check=False).stderr.decode()[-3000:])
        time.sleep(0.5)
    else:
        raise RuntimeError(f"Briefcase never became ready at {bc['url']}/readyz")
    version = http("GET", bc["url"] + "/api/version")[1]
    bc["version"] = (version or {}).get("version") if isinstance(version, dict) else None
    save(state)
    print(f"• Briefcase {BRIEFCASE_IMAGE} on {bc['url']} (MinIO {bc['minio_url']}, bucket {bc['bucket']})")


def probe(url):
    """HTTP status of a GET, or 0 while nothing answers (Docker's published port accepts and drops)."""
    try:
        return http("GET", url)
    except (OSError, ValueError, __import__("http.client").client.HTTPException):
        return 0, None


def ting_binary():
    """The published Ting server, downloaded once and checked against its release SHA-256."""
    folder = CACHE_DIR / f"ting-{TING_COMMIT}"
    binary = folder / "ting-server"
    if binary.exists():
        return binary
    folder.mkdir(parents=True, exist_ok=True)
    archive = folder / f"ting-server-{TING_COMMIT}.tar.gz"
    url = f"https://github.com/teamofsilicons/silicon-ting/releases/download/server-{TING_COMMIT}/{archive.name}"
    print(f"• downloading {url}")
    with urllib.request.urlopen(url, timeout=120) as resp, open(archive, "wb") as out:
        shutil.copyfileobj(resp, out)
    if hashlib.sha256(archive.read_bytes()).hexdigest() != TING_SHA256:
        archive.unlink()
        raise RuntimeError("the Ting server archive does not match its pinned SHA-256")
    import tarfile
    with tarfile.open(archive) as bundle:
        member = bundle.getmember("./ting-server")
        if not member.isfile():
            raise RuntimeError("unexpected Ting archive layout")
        with bundle.extractfile(member) as src, open(binary, "wb") as out:
            shutil.copyfileobj(src, out)
    binary.chmod(0o755)
    archive.unlink()
    return binary


def up_ting(state):
    """The published Ting server (SQLite) on the fixture network, with Extend's type registered."""
    tg = state["ting"]
    server = ting_binary()
    data = STATE_DIR / "ting-data"
    data.mkdir(mode=0o700, exist_ok=True)
    env = {"TING_BIND": f"0.0.0.0:{TING_BIND_PORT}", "TING_PUBLIC_ORIGIN": tg["url"], "TING_DATABASE_PATH": "/data/ting.sqlite",
           "TING_ENCRYPTION_KEY": secrets.token_hex(32), "TING_IAM_URL": "http://127.0.0.1:8080",
           "TING_IAM_APP_SECRET": tg["app_secret"], "TING_HONEYCOMB_URL": "http://127.0.0.1:1",
           "TING_IAM_WEBHOOK_SECRET": tg["webhook_secret"], "TING_IAM_WEBHOOK_SECRET_VERSION": "1",
           "TING_SPACESTATION_URL": "http://127.0.0.1:1", "TING_SPACESTATION_KEY": "table-fixture-" + secrets.token_hex(16),
           "TING_SPACESTATION_TABLE": "fixture", "TING_DOCS_URL": "http://127.0.0.1:1/docs", "RUST_LOG": "info"}
    private(STATE_DIR / "ting.env", "".join(f"{k}={v}\n" for k, v in env.items()))
    ca = next((p for p in ("/etc/ssl/cert.pem", "/etc/ssl/certs/ca-certificates.crt") if Path(p).exists()), None)
    run(["docker", "run", "-d", "--name", f"{NAME}-ting", "--network", f"container:{NAME}-iam",
         "--env-file", str(STATE_DIR / "ting.env"),
         "-v", f"{server}:/app/ting-server:ro", "-v", f"{data}:/data",
         *(["-v", f"{ca}:/etc/ssl/certs/ca-certificates.crt:ro"] if ca else []),
         "--entrypoint", "/app/ting-server", TING_RUNTIME_IMAGE])
    state["containers"].append(f"{NAME}-ting"); save(state)
    start_lane_relay(state, "ting", TING_BIND_PORT)

    def healthy():
        for _ in range(120):
            status, v = probe(tg["url"] + "/healthz")
            if status == 200:
                return v
            time.sleep(0.5)
        raise RuntimeError("Ting never answered /healthz:\n" + run(["docker", "logs", "--tail", "30", f"{NAME}-ting"], check=False).stdout.decode()[-2000:])
    healthy()
    # Ting registers types through Honeycomb-authorized sessions; with no Honeycomb here, the
    # fixture writes Extend's one type while the server is stopped (never two SQLite writers).
    run(["docker", "stop", f"{NAME}-ting"])
    import sqlite3
    db = sqlite3.connect(data / "ting.sqlite")
    with db:
        db.execute("INSERT OR IGNORE INTO types(ctx,org,app,name,description) VALUES('production','acme','extend',?,?)",
                   (TING_TYPE, "A Silicon asks to use a device another Silicon is using"))
    db.close()
    run(["docker", "start", f"{NAME}-ting"])
    tg["version"] = healthy().get("version")
    save(state)
    print(f"• Ting {tg['version']} (server-{TING_COMMIT[:12]}) on {tg['url']}; type {TING_TYPE} registered")


def up(args):
    if (STATE_DIR / "state.json").exists():
        raise SystemExit(f"a fixture is already up ({STATE_DIR}); run `realiam.py down` first")
    for tool in (IAM_CLI, *(TARGET.joinpath(*parts) for parts in BINARIES.values())):
        if not Path(tool).exists():
            raise SystemExit(f"missing {tool}; build with: cargo build -p extend-service -p extend-cli --bins --examples")
    import socket
    with socket.socket() as probe:
        if probe.connect_ex(("127.0.0.1", EXTEND_PORT)) == 0:
            raise SystemExit(f"port {EXTEND_PORT} is in use; set REALIAM_EXTEND_PORT to a free port")
    STATE_DIR.mkdir(mode=0o700, exist_ok=True)
    (STATE_DIR / "bin").mkdir(mode=0o700, exist_ok=True)
    for name, parts in BINARIES.items():
        shutil.copy2(TARGET.joinpath(*parts), STATE_DIR / "bin" / name)
    state = {"containers": [], "network": NAME, "iam_image": IAM_IMAGE,
             "app_secret": "ask_" + b64(secrets.token_bytes(32)),
             "webhook_secret": "extend-realiam-webhook-" + secrets.token_hex(16),
             "webhook_url": f"http://{RELAY_IP}:{EXTEND_PORT}/webhook/",
             "honeycomb_token": "hck_" + secrets.token_hex(16)}
    plan_lanes(state, args)
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
         "-p", "127.0.0.1::8080", *lane_ports(state), "--env-file", str(STATE_DIR / "iam.env"), IAM_IMAGE, "iam-api"])
    state["containers"].append(f"{NAME}-iam"); save(state)
    port = run(["docker", "port", f"{NAME}-iam", "8080/tcp"]).stdout.decode().split()[0].rsplit(":", 1)[1]
    state["iam_url"] = f"http://127.0.0.1:{port}"; save(state)
    print("• seeding team acme: c:alice (owner), c:bob, si:chef, si:sous; applications " + ", ".join(["extend", *sorted(lanes(state))]))
    seed(state, pepper, enc_key); save(state)
    wait_http(state["iam_url"] + "/healthz", "IAM")
    run(["docker", "run", "-d", "--name", f"{NAME}-worker", "--network", NAME, "--env-file", str(STATE_DIR / "iam-worker.env"), IAM_IMAGE, "iam-worker"])
    state["containers"].append(f"{NAME}-worker"); save(state)
    run(["docker", "run", "-d", "--name", f"{NAME}-relay", "--network", NAME, "--ip", RELAY_IP, "--entrypoint", "perl",
         PG_IMAGE, "-e", RELAY, str(EXTEND_PORT), f"host.docker.internal:{EXTEND_PORT}"])
    state["containers"].append(f"{NAME}-relay"); save(state)
    print(f"• IAM {state['iam_url']}  (webhooks → {state['webhook_url']} → host :{EXTEND_PORT})")
    if "briefcase" in lanes(state):
        up_briefcase(state, db_pw)
    if "ting" in lanes(state):
        up_ting(state)

    psql(f"DROP DATABASE IF EXISTS {EXTEND_DB} WITH (FORCE); CREATE DATABASE {EXTEND_DB};", container=EXTEND_DB_CONTAINER, user="extend", database="extend")
    state["extend_db"] = EXTEND_DB; save(state)
    start_extend(state)
    print(f"READY  state: {STATE_DIR / 'state.json'}")


def extend_env(state):
    env = {k: v for k, v in os.environ.items() if not k.startswith("EXTEND_")}
    env.update({"EXTEND_ENVIRONMENT": "development", "EXTEND_DATABASE_URL": EXTEND_DB_URL,
                "EXTEND_BIND": f"127.0.0.1:{EXTEND_PORT}", "EXTEND_PUBLIC_URL": f"http://127.0.0.1:{EXTEND_PORT}",
                "EXTEND_DATA_DIR": str(STATE_DIR / "extend-data"), "EXTEND_IAM_MODE": "sdk",
                "EXTEND_IAM_BASE_URL": state["iam_url"], "EXTEND_IAM_APP_ID": "extend",
                "EXTEND_IAM_APP_SECRET": state["app_secret"], "EXTEND_IAM_WEBHOOK_SECRET": state["webhook_secret"],
                "EXTEND_IAM_WEBHOOK_SECRET_VERSION": "1", "EXTEND_FILES_MODE": "local", "EXTEND_TING_MODE": "local",
                "EXTEND_HONEYCOMB_SERVICE_TOKEN": state["honeycomb_token"],
                "EXTEND_LOG": "info,extend_service=debug,sqlx=warn,tower_http=warn"})
    if "briefcase" in lanes(state):
        env.update({"EXTEND_FILES_MODE": "briefcase", "EXTEND_BRIEFCASE_URL": state["briefcase"]["url"],
                    "EXTEND_BRIEFCASE_WEB_URL": state["briefcase"]["url"]})
    if "ting" in lanes(state):
        env.update({"EXTEND_TING_MODE": "ting", "EXTEND_TING_URL": state["ting"]["url"]})
    return env


def start_extend(state):
    log = open(STATE_DIR / "extend.log", "ab")
    proc = subprocess.Popen([binary("extend-service"), "serve"], env=extend_env(state), stdout=log, stderr=log,
                            cwd=ROOT, start_new_session=True)
    state["extend_pid"] = proc.pid; save(state)
    for _ in range(120):
        if proc.poll() is not None:
            raise RuntimeError(f"extend-service exited {proc.returncode}; see {STATE_DIR / 'extend.log'}:\n" + (STATE_DIR / "extend.log").read_text()[-3000:])
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{EXTEND_PORT}/api/version", timeout=2):
                break
        except urllib.error.HTTPError:
            break
        except OSError:
            time.sleep(0.5)
    env = extend_env(state)
    print(f"• Extend (EXTEND_IAM_MODE=sdk, EXTEND_FILES_MODE={env['EXTEND_FILES_MODE']}, EXTEND_TING_MODE={env['EXTEND_TING_MODE']}) "
          f"on http://127.0.0.1:{EXTEND_PORT}, pid {proc.pid}, log {STATE_DIR / 'extend.log'}")


def stop_extend(state):
    pid = state.get("extend_pid")
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
        state["extend_pid"] = None


def down(_args=None):
    path = STATE_DIR / "state.json"
    if not path.exists():
        print("nothing to tear down")
        return
    state = json.loads(path.read_text())
    stop_extend(state)
    for name in (f"{NAME}-briefcase", f"{NAME}-briefcase-worker", f"{NAME}-ting"):
        if name in state.get("containers", []):
            r = run(["docker", "logs", name], check=False)
            private(STATE_DIR / f"{name.removeprefix(NAME + '-')}.log", (r.stdout + r.stderr).decode(errors="replace"))
    for name in reversed(state.get("containers", [])):
        if not name.startswith(NAME + "-"):
            raise RuntimeError(f"refusing to remove unowned container {name}")
        run(["docker", "rm", "-f", name], check=False)
    run(["docker", "network", "rm", NAME], check=False)
    if state.get("extend_db"):
        run(["docker", "exec", "-i", EXTEND_DB_CONTAINER, "psql", "-U", "extend", "-d", "extend", "-X", "-c",
             f"DROP DATABASE IF EXISTS {EXTEND_DB} WITH (FORCE)"], check=False)
    keep = STATE_DIR.parent / "last-run"
    shutil.rmtree(keep, ignore_errors=True)
    keep.mkdir(mode=0o700)
    for f in ("extend.log", "report.json", "briefcase.log", "briefcase-worker.log", "ting.log"):
        if (STATE_DIR / f).exists():
            shutil.copy2(STATE_DIR / f, keep / f)
    shutil.rmtree(STATE_DIR, ignore_errors=True)
    print(f"Removed this fixture's containers, network and {EXTEND_DB}; logs kept in {keep}")


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
        self.api = f"http://127.0.0.1:{EXTEND_PORT}"
        self.passed, self.failed, self.notes, self.gaps = [], [], [], []
        self.work = STATE_DIR / "work"
        self.work.mkdir(exist_ok=True)

    # helpers
    def ok(self, name, detail=""):
        self.passed.append({"check": name, "detail": detail})
        print(f"  ✓ {name}" + (f"  — {detail}" if detail else ""), flush=True)

    def fail(self, name, detail):
        self.failed.append({"check": name, "detail": detail})
        print(f"  ✗ {name}  — {detail}", flush=True)

    def gap(self, name, detail):
        """A limit of another service that Extend cannot fix; reported, not counted as a failure."""
        self.gaps.append({"check": name, "detail": detail})
        print(f"  ! {name}  — {detail}", flush=True)

    def iam_cli(self, actor, args, stdin=None):
        env = {k: v for k, v in os.environ.items() if not k.startswith(("SILICON_IAM_", "IAM_TEST_")) and k != "SILICON_ORG"}
        env["SILICON_HOME"] = str(STATE_DIR / "iam-profiles" / actor)
        r = run([IAM_CLI, "--url", self.s["iam_url"], "--no-org", "--json", *args], stdin=stdin, env=env, timeout=60)
        return json.loads(r.stdout)

    def slt(self, actor):
        return self.iam_cli(actor, ["login", "--app-id", "extend", "--grant-org", "acme", "--approve-scopes"])["slt"]

    def extend(self, who, *args, check=True, test=None):
        home = self.work / "homes" / who
        home.mkdir(parents=True, exist_ok=True)
        env = {k: v for k, v in os.environ.items() if not k.startswith("EXTEND_")}
        env.update({"EXTEND_API_URL": self.api, "EXTEND_TELEMETRY": "off", "SILICON_HOME": str(home)})
        full = [binary("extend")] + (["--test", test] if test else []) + list(args)
        r = subprocess.run(full, capture_output=True, env=env, timeout=120)
        out, err = r.stdout.decode(errors="replace"), r.stderr.decode(errors="replace")
        if check and r.returncode:
            raise RuntimeError(f"extend {' '.join(args)} (as {who}) exited {r.returncode}: {out[-800:]} {err[-800:]}")
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

    def extend_log(self):
        import re
        return re.sub(r"\x1b\[[0-9;]*m", "", (STATE_DIR / "extend.log").read_text(errors="replace"))

    def screenshot_file(self, who):
        """`extend --json screenshot` in the Silicon's current session; the one file it made."""
        since = len(self.extend_log())
        _, out, _ = self.extend(who, "--json", "screenshot")
        files = json.loads(out)["files"]
        if len(files) != 1:
            errors = [l for l in self.extend_log()[since:].splitlines() if "storing a command file failed" in l or "Briefcase" in l]
            raise RuntimeError(f"expected one stored file, got {len(files)}; Extend log: {errors[-3:]}")
        return files[0]

    def extend_db(self, sql):
        return psql(sql, container=EXTEND_DB_CONTAINER, user="extend", database=EXTEND_DB).strip()

    def app_login(self, who, app):
        """A real IAM SLT for another application (Briefcase or Ting), approved by the member."""
        return self.iam_cli(who, ["login", "--app-id", app, "--grant-org", "acme", "--approve-scopes"])["slt"]

    def wait_log(self, needle, since, timeout=45):
        deadline = time.time() + timeout
        while time.time() < deadline:
            log = self.extend_log()[since:]
            if needle in log:
                return log
            time.sleep(0.5)
        return None


# ───────────────────────────── Briefcase lane ─────────────────────────────

def raw_get(url, token, headers=None):
    req = urllib.request.Request(url, headers={"Authorization": "Bearer " + token, **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=40) as resp:
            return resp.status, resp.read(), dict(resp.headers)
    except urllib.error.HTTPError as e:
        return e.code, e.read(), dict(e.headers)


def briefcase_token(c, state, who):
    """Signs a member in to Briefcase itself (IAM SLT for app `briefcase` → POST /api/v1/auth/slt)."""
    cache = c.__dict__.setdefault("bc_tokens", {})
    if who not in cache:
        _, v = http("POST", state["briefcase"]["url"] + "/api/v1/auth/slt", {"slt": c.app_login(who, "briefcase")},
                    headers={"Idempotency-Key": str(uuid.uuid4())}, expected=(200,))
        if v["actor"]["public_id"] != ACTORS[who][0]:
            raise RuntimeError(f"Briefcase signed in {v['actor']} instead of {ACTORS[who][0]}")
        cache[who] = v["access_token"]
    return cache[who]


def bc_get(c, state, who, path):
    return http("GET", state["briefcase"]["url"] + path, token=briefcase_token(c, state, who), headers={"X-Org-ID": "acme"})


def briefcase_unseen_owner(c, state, ctx):
    """Briefcase's delegated invitation accepts only members it already projected (from a signed-in
    request or an IAM webhook); it does not ask IAM's directory. c:alice has not used Briefcase yet."""
    since = len(c.extend_log())
    f = c.screenshot_file("chef")
    if f.get("shared_with") == "c:alice":
        c.ok("a file is shared with a device owner who never used Briefcase", f["file_id"])
        return
    log = [l for l in c.extend_log()[since:].splitlines() if "sharing an Extend file" in l]
    if not log or "invalid_principal" not in log[-1]:
        raise RuntimeError(f"sharing failed for another reason: {log[-1:] or 'nothing logged'}")
    c.ok("the file is still stored when sharing fails; Extend logs why", log[-1].split("error=", 1)[-1][:260])
    c.gap("Briefcase shares a delegated file only with a Carbon it has already seen",
          "briefcase.invitations.create → 422 invalid_principal for c:alice until she has signed in to Briefcase once "
          "(its delegated invite does not resolve the recipient through IAM's directory; the bearer invite does)")


def briefcase_file(c, state, ctx):
    bc = state["briefcase"]
    folder = "apps/extend/private/si:chef/"
    token = briefcase_token(c, state, "alice")
    status, _ = http("GET", bc["url"] + "/api/v1/entries", token=token, headers={"X-Org-ID": "acme"})
    if status != 200:
        raise RuntimeError(f"c:alice cannot list Briefcase: HTTP {status}")
    c.ok("c:alice signed in to Briefcase (real SLT for app briefcase) and listed it; Briefcase now knows her")
    f = ctx["bc_file"] = c.screenshot_file("chef")
    prefix = f"{bc['url']}/org/acme/{folder}"
    name = urllib.parse.unquote(f["url"].removeprefix(prefix))
    if not f["url"].startswith(prefix) or not (name.startswith("screenshot-") and name.endswith(".png")):
        raise RuntimeError(f"Extend returned {f['url']!r}, not a Briefcase permanent URL {prefix}screenshot-….png")
    c.ok("extend screenshot → Briefcase OBO briefcase.files.create; Extend returned Briefcase's permanent URL", f["url"])
    if f.get("shared_with") != "c:alice":
        raise RuntimeError(f"Extend recorded shared_with={f.get('shared_with')!r}; the invitation to c:alice failed (see extend.log)")
    status, e = bc_get(c, state, "alice", f"/api/v1/entries/{f['file_id']}")
    if status != 200:
        raise RuntimeError(f"c:alice cannot see the file in Briefcase: HTTP {status} {e}")
    want = {"path": folder + name, "origin_app_id": "extend", "permanent_url": f["url"], "size": f["size_bytes"]}
    got = {k: e.get(k) for k in want}
    if got != want or (e.get("owner") or {}).get("id") != "si:chef":
        raise RuntimeError(f"Briefcase entry {got} owner={e.get('owner')} differs from {want}")
    c.ok("the file is in Briefcase in Extend's app folder for si:chef, owned by si:chef, under its own name",
         f"{e['path']} origin_app_id={e['origin_app_id']} size={e['size']} (read as c:alice through Briefcase's API)")
    status, grants = bc_get(c, state, "alice", f"/api/v1/entries/{f['file_id']}/permissions")
    mine = [g for g in (grants or {}).get("items", []) if (g.get("principal") or {}).get("id") == "c:alice"] if status == 200 else []
    if len(mine) != 1 or sorted(mine[0].get("access", [])) != ["read", "update"] or (mine[0].get("granted_by") or {}).get("id") != "si:chef":
        raise RuntimeError(f"grants on the file: HTTP {status} {grants}")
    c.ok("Briefcase holds si:chef's grant to device owner c:alice: read and update (no delete; Briefcase grants create only on folders)",
         f"grant {mine[0]['id']} access={sorted(mine[0]['access'])}")
    access = sorted(e.get("effective_access") or [])
    c.notes.append(f"c:alice is acme's owner, so Briefcase gives her every right on the file regardless of the grant: {access}")
    status, body, _ = raw_get(f"{bc['url']}/api/v1/entries/{f['file_id']}/content", token, {"X-Org-ID": "acme"})
    if status != 200 or len(body) != f["size_bytes"] or not body.startswith(b"\x89PNG"):
        raise RuntimeError(f"content as c:alice: HTTP {status}, {len(body)} bytes, starts {body[:8]!r}")
    ctx["bc_sha256"] = hashlib.sha256(body).hexdigest()
    c.ok("c:alice reads the PNG bytes from Briefcase (stored in MinIO with SSE-S3)", f"{len(body)} bytes sha256={ctx['bc_sha256'][:16]}…")
    status, v = http("GET", f["url"], token=token, headers={"X-Org-ID": "acme"})
    resolved = v.get("id") if isinstance(v, dict) else None
    if status != 200 or resolved != f["file_id"]:
        raise RuntimeError(f"GET {f['url']} as c:alice -> HTTP {status} {str(v)[:200]}")
    c.ok("the permanent URL Extend returned resolves in Briefcase to the same entry", f"GET {f['url'].removeprefix(bc['url'])} → {resolved}")


def briefcase_member_owner(c, state, ctx):
    """c:bob (a member, not an owner) pairs a device; si:chef's screenshot there is shared with bob."""
    bc = state["briefcase"]
    c.extend("bob", "login", c.slt("bob"))
    briefcase_token(c, state, "bob")
    http("GET", bc["url"] + "/api/v1/entries", token=briefcase_token(c, state, "bob"), headers={"X-Org-ID": "acme"}, expected=(200,))
    log = c.work / "bob-fake.log"
    ctx["bob_fake"] = subprocess.Popen([binary("fake_device"), c.api, "linux"], stdout=open(log, "wb"), stderr=subprocess.STDOUT)
    code = None
    for _ in range(100):
        txt = log.read_text(errors="replace")
        if "PAIRING_CODE" in txt:
            code = txt.split("PAIRING_CODE", 1)[1].split()[0]
            break
        time.sleep(0.2)
    _, out, _ = c.extend("bob", "device", "pair", code, "--name", "Bob box", "--access", "si:chef", "--json")
    dev = json.loads(out)["device_id"]
    time.sleep(1)
    _, sid, _ = c.extend("chef", "session", "new", dev, "--connect")
    try:
        f = c.screenshot_file("chef")
    finally:
        c.extend("chef", "session", "end", sid.strip(), check=False)
        c.extend("chef", "session", "connect", ctx["session"], check=False)
    if f.get("shared_with") != "c:bob":
        raise RuntimeError(f"shared_with={f.get('shared_with')!r} (see extend.log)")
    status, e = bc_get(c, state, "bob", f"/api/v1/entries/{f['file_id']}")
    access = sorted((e or {}).get("effective_access") or []) if status == 200 else (status, e)
    if access != ["read", "update"]:
        raise RuntimeError(f"c:bob's effective access on the file: {access}")
    c.ok("device owner c:bob (a Team member) can read and update si:chef's file on his device, not delete it", f"effective_access={access}")
    status, body, _ = raw_get(f"{bc['url']}/api/v1/entries/{f['file_id']}/content", briefcase_token(c, state, "bob"), {"X-Org-ID": "acme"})
    status_del, v = http("DELETE", f"{bc['url']}/api/v1/entries/{f['file_id']}", token=briefcase_token(c, state, "bob"),
                         headers={"X-Org-ID": "acme", "Idempotency-Key": str(uuid.uuid4())})
    if status != 200 or len(body) != f["size_bytes"] or status_del not in (403, 404):
        raise RuntimeError(f"c:bob read HTTP {status} ({len(body)} bytes), delete HTTP {status_del} {v}")
    c.ok("c:bob reads the bytes; Briefcase refuses his delete", f"read HTTP 200, DELETE HTTP {status_del} {err_code(v)}")
    ctx["bob_fake"].terminate()


def briefcase_keep(c, state, ctx):
    f = ctx["bc_file"]
    _, out, _ = c.extend("chef", "--json", "file", "keep", f["file_id"])
    kept = json.loads(out)
    if not kept.get("permanent") or kept.get("self_destruct_at"):
        raise RuntimeError(f"extend file keep -> {kept}")
    status, e = bc_get(c, state, "chef", f"/api/v1/entries/{f['file_id']}")
    if status != 200 or e.get("self_destruct_at") or e.get("deleted_at"):
        raise RuntimeError(f"after keep, Briefcase entry: HTTP {status} {e}")
    c.ok("extend file keep: permanent in Extend; the Briefcase entry has no self-destruct time and stays",
         f"permanent={kept['permanent']} self_destruct_at={kept.get('self_destruct_at')}")


def briefcase_self_destruct(c, state, ctx):
    bc = state["briefcase"]
    doomed = c.screenshot_file("chef")
    if not doomed.get("self_destruct_at"):
        raise RuntimeError(f"a new screenshot has no self-destruct time: {doomed}")
    status, _ = bc_get(c, state, "alice", f"/api/v1/entries/{doomed['file_id']}")
    if status != 200:
        raise RuntimeError(f"second screenshot not in Briefcase: HTTP {status}")
    since = len(c.extend_log())
    n = c.extend_db(f"UPDATE extend.files SET self_destruct_at = now() - interval '1 minute' WHERE file_id = '{doomed['file_id']}' RETURNING file_id")
    if doomed["file_id"] not in n:
        raise RuntimeError("could not backdate the file in Extend's database")
    c.ok("second screenshot stored; its self_destruct_at backdated in Extend's database", doomed["file_id"])
    # The scheduler's slow pass runs every 30 s.
    deadline = time.time() + 75
    while time.time() < deadline:
        if c.extend_db(f"SELECT count(*) FROM extend.files WHERE file_id = '{doomed['file_id']}'") == "0":
            break
        time.sleep(1)
    else:
        raise RuntimeError("Extend's scheduler did not act on the passed self-destruct time within 75 s")
    failed = [l for l in c.extend_log()[since:].splitlines() if "self-destruct delete failed" in l]
    if failed:
        raise RuntimeError(f"Extend dropped its record but Briefcase's trash failed: {failed[-1][-400:]}")
    status, e = bc_get(c, state, "alice", f"/api/v1/entries/{doomed['file_id']}")
    if status != 404:
        raise RuntimeError(f"the self-destructed file is still visible in Briefcase: HTTP {status} {str(e)[:200]}")
    status, binned = bc_get(c, state, "chef", "/api/v1/bin")
    in_bin = [i for i in (binned or {}).get("items", []) if i.get("id") == doomed["file_id"]] if status == 200 else []
    if not in_bin:
        raise RuntimeError(f"the file is not in si:chef's Briefcase bin: HTTP {status} {str(binned)[:300]}")
    c.ok("self-destruct: Extend trashed the file through Briefcase OBO briefcase.entries.trash",
         f"gone for c:alice (404), in si:chef's bin (deleted_at={in_bin[0].get('deleted_at')})")
    status, _ = bc_get(c, state, "alice", f"/api/v1/entries/{ctx['bc_file']['file_id']}")
    if status != 200:
        raise RuntimeError(f"the kept file disappeared: HTTP {status}")
    c.ok("the kept file is untouched by the self-destruct pass")


def briefcase_download(c, state, ctx):
    """`extend file get` / `screenshot --out` fetch a file's bytes for the Silicon that made it."""
    out = c.work / "bc-get.png"
    rc, stdout, err = c.extend("chef", "file", "get", ctx["bc_file"]["file_id"], "--out", str(out), check=False)
    if rc != 0 or not out.exists():
        raise RuntimeError(f"extend file get exited {rc}: {(stdout + err).strip()[-300:]} — the CLI downloads the Briefcase "
                           "permanent URL with its Extend token, which Briefcase refuses; Extend needs a service-side "
                           "download through OBO briefcase.files.read")
    if hashlib.sha256(out.read_bytes()).hexdigest() != ctx.get("bc_sha256"):
        raise RuntimeError("extend file get saved different bytes than Briefcase holds")
    c.ok("extend file get downloaded the Briefcase file", f"{out.stat().st_size} bytes")


# ───────────────────────────── Ting lane ─────────────────────────────

def ting_request(c, state, ctx, reason):
    since = len(c.extend_log())
    _, out, _ = c.extend("sous", "--json", "request", "send", ctx["device"], "--reason", reason)
    r = json.loads(out)
    err = c.extend_db(f"SELECT coalesce(last_error, '') FROM extend.requests WHERE request_id = '{r['request_id']}'")
    return r, err, c.extend_log()[since:]


def ting_unregistered(c, state, ctx):
    # si:chef is using the device (session step); si:sous asks for it.
    reason = "Need it for 2 minutes to read an OTP (sent before si:chef is a Ting recipient)"
    r, err, _ = ting_request(c, state, ctx, reason)
    ctx["ting_first_at"] = time.time()
    if r["to"] != "si:chef":
        raise RuntimeError(f"request addressed to {r['to']}, expected si:chef")
    if r["delivery"] == "delivered":
        c.ok("Extend registered si:chef as a Ting recipient before delivering", "first request delivered")
        ctx["ting_registered_by_extend"] = True
        return
    c.fail("Extend registers the using Silicon as a Ting recipient (subscriptions.register) before delivering",
           f"delivery={r['delivery']}; Ting answered: {err[:300]}")


def ting_register(c, state, who):
    """Registers `who` as a Ting recipient for Extend the way Extend itself would: a fresh IAM OBO
    proof for subscriptions.register, minted with Extend's credential for the member's Extend token."""
    token = json.loads(c.auth_file(who).read_text())["access_token"]
    body = json.dumps({"org_id": "acme", "app_id": "extend", "for": ACTORS[who][0]}, separators=(",", ":")).encode()
    proof = c.iam_cli(who, ["app", "obo", "exchange", "ting", "subscriptions.register", "--as-app-id", "extend",
                            "--app-secret", state["app_secret"], "--subject-token", token, "--org-context", "acme",
                            "--method", "POST", "--body-file", "-"], stdin=body)["access_proof"]
    _, grant = http("POST", state["ting"]["url"] + "/v1/subscriptions", body, token=proof, expected=(200, 201))
    return grant


def ting_delivery(c, state, ctx):
    tg = state["ting"]
    if not ctx.get("ting_registered_by_extend"):
        grant = ting_register(c, state, "chef")
        if not grant.get("active") or grant.get("for") != "si:chef" or grant.get("app_id") != "extend":
            raise RuntimeError(f"subscription: {grant}")
        c.ok("stand-in for the missing Extend call: si:chef registered as a Ting recipient of extend through IAM OBO",
             f"subscription {grant['id']} active")
        # Extend returns a Silicon's open request for a device again within 60 s instead of sending another.
        wait = 61 - (time.time() - ctx.get("ting_first_at", 0))
        if wait > 0:
            time.sleep(wait)
    reason = "Need it for 2 minutes — \"vendor\" OTP, ünïcode & <tags> kept exactly"
    r, err, log = ting_request(c, state, ctx, reason)
    if r["delivery"] != "delivered":
        raise RuntimeError(f"delivery={r['delivery']}: {err[:400]}")
    c.ok("extend request send → Ting accepted it (OBO tings.send)", f"request {r['request_id']} delivery=delivered")
    _, session = http("POST", tg["url"] + "/v1/session", {"slt": c.app_login("chef", "ting")},
                      headers={"Idempotency-Key": str(uuid.uuid4())}, expected=(200, 201))
    _, inbox = http("GET", tg["url"] + "/v1/orgs/acme/inbox?app_id=extend", token=session["session_token"], expected=(200,))
    found = [t for t in inbox.get("items", []) if t.get("key") == r["request_id"]]
    if not found:
        raise RuntimeError(f"no ting with key {r['request_id']} in si:chef's inbox: {str(inbox)[:400]}")
    t = found[0]
    data = t.get("data") or {}
    if (t.get("type"), t.get("for"), data.get("reason"), data.get("from"), data.get("device_id")) != \
            (TING_TYPE, "si:chef", reason, "si:sous", ctx["device"]):
        raise RuntimeError(f"the ting differs from the request: {t}")
    c.ok("si:chef's Ting inbox has the request with the reason exactly as sent",
         f"{t['id']} type={t['type']} for={t['for']} silent={t.get('silent')} reason={data['reason']!r}")


def check(args):
    state = load()
    c = Checks(state)
    ctx = {}
    print(f"Extend {c.api} against real IAM {state['iam_url']}")

    def negotiation():
        log = c.extend_log()
        if "Silicon Extend service listening" not in log:
            raise RuntimeError("service never reported listening")
        c.ok("Extend started in EXTEND_IAM_MODE=sdk; IAM API-version negotiation succeeded at startup",
             "SdkIam::connect → system().negotiate() returned before listen")
        code, out, _ = c.extend("nobody", "iam", "--json")
        c.ok("extend iam --json", json.loads(out).get("app_id", "?"))
    c.step("startup", negotiation)

    def logins():
        for who in ("alice", "chef", "sous"):
            slt = c.slt(who)
            c.extend(who, "login", slt)
            code, out, _ = c.extend(who, "login", "status", "--json")
            d = json.loads(out)
            want = ACTORS[who][0]
            if not d.get("authenticated") or d["member"]["id"] != want:
                raise RuntimeError(f"login status for {who}: {d}")
            c.ok(f"real SLT → extend login; login status --json authenticated:true as {want}", f"teams={d.get('teams')} role={d.get('team_role')}")
    c.step("login", logins)

    def team():
        code, out, _ = c.extend("alice", "team", "ls")
        if "acme" not in out:
            raise RuntimeError(out)
        c.ok("extend team ls lists acme", out.strip().replace("\n", " | ")[:120])
        tok = json.loads(c.auth_file("alice").read_text())["access_token"]
        status, v = http("GET", c.api + "/api/v1/team/silicons", token=tok, headers={"X-Org-ID": "acme"})
        ids = sorted(i["id"] for i in (v or {}).get("data", {}).get("items", [])) if status == 200 else None
        if ids != ["si:chef", "si:sous"]:
            raise RuntimeError(f"GET /api/v1/team/silicons -> {status} {v}")
        c.ok("GET /api/v1/team/silicons lists si:chef and si:sous from IAM's directory", str(ids))
    c.step("team", team)

    def device():
        log = c.work / "fake.log"
        fake = subprocess.Popen([binary("fake_device"), c.api, "linux"], stdout=open(log, "wb"), stderr=subprocess.STDOUT)
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
        _, out, _ = c.extend("alice", "device", "pair", code, "--name", "Real IAM box", "--json")
        dev = json.loads(out)["device_id"]
        ctx["device"] = dev
        for _ in range(50):
            if "PAIRED" in log.read_text(errors="replace"):
                break
            time.sleep(0.2)
        c.ok("alice paired the fake device through real IAM auth", dev)
        _, out, _ = c.extend("alice", "device", "access", "grant", dev, "si:chef")
        c.ok("extend device access grant si:chef (IAM member_active for a Silicon)", out.strip()[:100])
        _, out, _ = c.extend("alice", "device", "access", "grant", dev, "si:sous")
        c.ok("extend device access grant si:sous", out.strip()[:100])
        rc, out, err = c.extend("alice", "device", "access", "grant", dev, "si:nobody", check=False)
        if rc == 0:
            raise RuntimeError("granting a non-member Silicon succeeded")
        c.ok("granting a Silicon that is not in acme is refused", f"exit {rc}: {err.strip()[:100]}")
    c.step("device", device)

    def session():
        dev = ctx["device"]
        _, sid, _ = c.extend("chef", "session", "new", dev, "--connect")
        ctx["session"] = sid.strip()
        c.ok("si:chef started a session", sid.strip())
        _, out, _ = c.extend("chef", "snapshot", "-i")
        c.ok("extend snapshot relayed", out.strip().splitlines()[0][:80] if out.strip() else "")
        if "briefcase" in lanes(state):
            return  # the Briefcase lane's own steps take and check the screenshots
        shot = c.work / "shot.png"
        c.extend("chef", "screenshot", "--out", str(shot))
        if not shot.exists() or shot.stat().st_size == 0:
            raise RuntimeError("screenshot not saved")
        c.ok("extend screenshot stored and saved", f"{shot.stat().st_size} bytes")
    c.step("session", session)

    if "ting" in lanes(state):
        c.step("ting: unregistered recipient", lambda: ting_unregistered(c, state, ctx))
    if "briefcase" in lanes(state):
        c.step("briefcase: owner Briefcase has not seen", lambda: briefcase_unseen_owner(c, state, ctx))
        c.step("briefcase: file in Briefcase", lambda: briefcase_file(c, state, ctx))
        c.step("briefcase: member Carbon owner", lambda: briefcase_member_owner(c, state, ctx))
        c.step("briefcase: keep", lambda: briefcase_keep(c, state, ctx))
        c.step("briefcase: self-destruct", lambda: briefcase_self_destruct(c, state, ctx))
        c.step("briefcase: download", lambda: briefcase_download(c, state, ctx))
    if "ting" in lanes(state):
        c.step("ting: delivery", lambda: ting_delivery(c, state, ctx))

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
        c.extend("chef", "login", c.slt("chef"))
        # CLI refresh path: make the stored access token unusable; the CLI must refresh on token_expired.
        auth = json.loads(path.read_text())
        before = auth["refresh_token"]
        auth["access_token"] = "oat_" + b64(secrets.token_bytes(32))
        private(path, auth)
        c.extend("chef", "login", "status", "--json")
        _, out, _ = c.extend("chef", "device", "ls")
        after = json.loads(path.read_text())
        if after["refresh_token"] == before or not after["access_token"].startswith("oat_"):
            raise RuntimeError("CLI did not refresh")
        c.ok("CLI refreshed through real IAM after token_expired and retried", "device ls succeeded after refresh")
        # The session was started with the old token; Extend keeps it working with the new login.
        rc, out, err = c.extend("chef", "session", "new", ctx["device"], "--connect", check=False)
        if rc == 0:
            ctx["session"] = out.strip()
        c.extend("chef", "snapshot")
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
        # 1. Remove si:sous from acme through IAM's real API; IAM must tell Extend, Extend must drop sous's access.
        since = len(c.extend_log())
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
            raise RuntimeError("no IAM webhook reached Extend within 45 s (see worker logs)")
        events = [l for l in log.splitlines() if "IAM event" in l]
        how = ("signature verified by the official WebhookVerifier; body read by Extend because SDK 4.0.0 refuses "
               "public-id aggregate ids" if "event read by Extend" in log else "verified and parsed by the official WebhookVerifier")
        c.ok("IAM delivered a signed webhook and Extend accepted it", how + " — " + events[0].split("IAM event", 1)[1][-200:])
        for _ in range(30):
            _, out, _ = c.extend("alice", "device", "access", "ls", dev)
            if "si:sous" not in out:
                break
            time.sleep(1)
        if "si:sous" in out:
            raise RuntimeError(f"si:sous still has access: {out}")
        c.ok("Extend removed si:sous's device access after the webhook", out.strip().replace("\n", " | ")[:120])
    c.step("webhook: Silicon removed", webhooks)

    def revoked_elsewhere():
        # Revoke chef's IAM refresh family directly at IAM (as another client of Extend's app would);
        # IAM sends applications no webhook for token revocation, so Extend notices on its next
        # live authorization, at most 30 s later (its authorization cache bound).
        auth = json.loads(c.auth_file("chef").read_text())
        basic = "Basic " + base64.b64encode(f"extend:{state['app_secret']}".encode()).decode()
        form = urllib.parse.urlencode({"token": auth["refresh_token"], "token_type_hint": "refresh_token"}).encode()
        http("POST", state["iam_url"] + "/api/v1/oauth/revoke", form, expected=(200, 204),
             headers={"Authorization": basic, "Idempotency-Key": str(uuid.uuid4()), "Content-Type": "application/x-www-form-urlencoded"})
        c.ok("IAM revoked si:chef's refresh family directly (not through Extend)")
        time.sleep(31)
        rc, out, err = c.extend("chef", "device", "ls", check=False)
        if rc != 3 or "token_expired" not in err:
            raise RuntimeError(f"chef still works after revocation: exit {rc} {out[-200:]} {err[-300:]}")
        c.ok("after ≤30 s Extend refuses the revoked login (CLI refresh also refused)", f"exit {rc} token_expired")
        c.extend("chef", "login", c.slt("chef"))
        _, sid, _ = c.extend("chef", "session", "new", ctx["device"], "--connect", check=False)
        ctx["session"] = sid.strip() or ctx.get("session")
    c.step("revoked elsewhere", revoked_elsewhere)

    def webhook_logout():
        # 2. si:chef logs out through Extend → IAM revokes → running session ends.
        since = len(c.extend_log())
        c.extend("chef", "logout")
        rc, out, err = c.extend("alice", "device", "show", ctx["device"], check=False)
        c.ok("si:chef logged out (IAM revocation)", "")
        log = c.wait_log("IAM event", since, timeout=20)
        c.notes.append("webhook after logout: " + ("; ".join(l[-200:] for l in log.splitlines() if "IAM event" in l) if log else "none within 20 s"))
        _, out, _ = c.extend("alice", "device", "activity", ctx["device"])
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
        since = len(c.extend_log())
        for _ in range(2):
            ts = str(int(time.time()))
            sig = "v1=" + hmac.new(state["webhook_secret"].encode(), f"{ts}.".encode() + body, hashlib.sha256).hexdigest()
            http("POST", c.api + "/webhook/", body, expected=(204,), headers={"X-Silicon-IAM-Signature": sig, "X-Silicon-IAM-Timestamp": ts,
                 "X-Silicon-IAM-Key-Version": "1", "X-Silicon-IAM-Event-ID": eid})
        log = c.extend_log()[since:]
        applied = [l for l in log.splitlines() if "IAM event" in l and eid in l]
        if len(applied) != 1 or "event read by Extend" in log:
            raise RuntimeError(f"expected one IAM event via the SDK parser, log: {log[-600:]}")
        c.ok("a signed SDK-shaped event passes the official verifier; a duplicate delivery is applied once", eid)
    c.step("forged webhook", forged)

    # ── Testing plane ──
    tctx = {}

    def tiam(method, path, body=None, token=None, headers=None, expected=(200, 201, 202, 204)):
        return http(method, state["iam_url"] + "/api/v1" + path, body, token=token, expected=expected,
                    headers={"X-Testing-Environment-Key": tctx["key"], "Idempotency-Key": str(uuid.uuid4()), **(headers or {})})[1]

    def testing_setup():
        # Extend's production credential asks IAM for a test environment; IAM imports Extend into it.
        auth = "Basic " + base64.b64encode(f"extend:{state['app_secret']}".encode()).decode()
        _, env = http("POST", state["iam_url"] + "/api/v1/application/testing-environments",
                      {"name": "extend-realiam", "description": "Extend real-IAM run"},
                      headers={"Authorization": auth, "Idempotency-Key": str(uuid.uuid4())}, expected=(200, 201))
        tctx.update(env=env["environment_id"], key=env["iam_test_key"], secret=env["app_secret"])
        private(STATE_DIR / "testing.private.json", env)
        c.ok("IAM created a test environment from Extend's application credential", f"{env['environment_id']} app={env['app_id']}")
        # Honeycomb's lifecycle instruction to Extend (Extend's internal participant API).
        op = str(uuid.uuid4())
        http("PUT", f"{c.api}/internal/honeycomb/organizations/acme/testing-environments/{tctx['env']}/operations/{op}",
             {"operation_id": op, "environment_id": tctx["env"], "org_id": "acme", "app_id": "extend", "environment_revision": 1,
              "generation": 1, "key_version": 1, "action": "prepare", "testing_key": tctx["key"], "name": "extend-realiam"},
             headers={"Authorization": "Bearer " + state["honeycomb_token"]}, expected=(200,))
        c.ok("Extend prepared the test environment (Honeycomb operation)")
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
                                                             "job_description": "Extend test"}, token=tctx["alice_direct"])
        c.ok("test plane: c:alice signed up (OTP 000000), team kitchen with si:chef and si:sous created through IAM")
    c.step("testing: setup", testing_setup)

    def testing_extend():
        env = tctx["env"]
        home = c.work / "homes"
        for who in ("talice", "tchef"):
            (home / who).mkdir(parents=True, exist_ok=True)
            e = {k: v for k, v in os.environ.items() if not k.startswith("EXTEND_")}
            e.update({"EXTEND_API_URL": c.api, "EXTEND_TELEMETRY": "off", "SILICON_HOME": str(home / who)})
            run([binary("extend"), "config", "test", "add", env], stdin=tctx["secret"].encode(), env=e)
        c.ok("extend config test add (secret on stdin)")
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
        c.extend("talice", "login", "c:alice", test=env)
        _, out, _ = c.extend("talice", "login", "status", "--json", test=env)
        d = json.loads(out)
        if not d["authenticated"] or d["member"]["id"] != "c:alice" or d["team"] != "kitchen":
            raise RuntimeError(d)
        c.ok("member-id login in the test plane (extend --test … login c:alice)", f"team={d['team']} role={d['team_role']}")
        c.extend("tchef", "login", "si:chef", test=env)
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
        tctx["fake"] = subprocess.Popen([binary("fake_device"), c.api, "linux", tctx["secret"]],
                                        stdout=open(log, "wb"), stderr=subprocess.STDOUT)
        code = None
        for _ in range(100):
            txt = log.read_text(errors="replace")
            if "PAIRING_CODE" in txt:
                code = txt.split("PAIRING_CODE", 1)[1].split()[0]
                break
            time.sleep(0.2)
        _, out, _ = c.extend("talice", "device", "pair", code, "--name", "Test box", "--access", "si:chef", "--access", "si:sous", "--json", test=env)
        tctx["device"] = json.loads(out)["device_id"]
        time.sleep(1)
        c.ok("device paired into the test environment with access for test si:chef and si:sous", tctx["device"])
        _, out, _ = c.extend("alice", "device", "ls")
        if "Test box" in out:
            raise RuntimeError("production sees the test device")
        c.ok("production does not see the test device")
        _, sid, _ = c.extend("tchef", "session", "new", tctx["device"], "--connect", test=env)
        _, out, _ = c.extend("tchef", "snapshot", test=env)
        c.ok("test-plane si:chef session + snapshot", sid.strip())
    c.step("testing: Extend", testing_extend)

    def testing_webhook():
        # Remove the test si:sous; IAM signs the test delivery (inherited webhook secret) and wraps it
        # in {"test": …}. Extend must route it to the test world by its key, never to production.
        since = len(c.extend_log())
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
            raise RuntimeError("no test-plane IAM webhook reached Extend within 45 s")
        line = [l for l in log.splitlines() if "IAM event" in l][0]
        if tctx["env"] not in line:
            raise RuntimeError(f"test event not routed to the test world: {line}")
        c.ok("IAM delivered the signed test-plane webhook; Extend routed it to the test world by its key", line.split("IAM event", 1)[1][-200:])
        for _ in range(20):
            _, out, _ = c.extend("talice", "device", "access", "ls", tctx["device"], test=env_id())
            if "si:sous" not in out:
                break
            time.sleep(1)
        if "si:sous" in out:
            raise RuntimeError(f"test si:sous still has access: {out}")
        c.ok("Extend removed test si:sous's access in the test world only", out.strip().replace("\n", " | ")[:120])
        _, out, _ = c.extend("alice", "device", "access", "ls", ctx["device"])
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
    report = {"passed": c.passed, "failed": c.failed, "gaps": c.gaps, "notes": c.notes, "iam_image": IAM_IMAGE,
              "lanes": sorted(lanes(state)), "briefcase_image": BRIEFCASE_IMAGE if "briefcase" in lanes(state) else None,
              "ting_server": f"server-{TING_COMMIT}" if "ting" in lanes(state) else None,
              "at": datetime.datetime.now(datetime.timezone.utc).isoformat()}
    private(STATE_DIR / "report.json", report)
    print(f"\n{len(c.passed)} passed, {len(c.failed)} failed, {len(c.gaps)} known gaps in other services")
    for n in c.notes:
        print("  note:", n)
    return not c.failed


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("command", choices=["up", "check", "down", "all", "restart-extend", "build-minio"])
    p.add_argument("--keep", action="store_true", help="with `all`: leave everything running afterwards")
    p.add_argument("--briefcase", action="store_true",
                   help="with `up`/`all`: store files in a real Briefcase (+ MinIO) through IAM OBO (EXTEND_FILES_MODE=briefcase)")
    p.add_argument("--ting", action="store_true",
                   help="with `up`/`all`: deliver requests through a real Ting server through IAM OBO (EXTEND_TING_MODE=ting)")
    a = p.parse_args()
    os.umask(0o077)
    if a.command == "build-minio":
        build_minio(a)
    elif a.command == "up":
        up(a)
    elif a.command == "down":
        down()
    elif a.command == "restart-extend":
        state = load()
        stop_extend(state)
        start_extend(state)
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
