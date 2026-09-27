#!/usr/bin/env python3
"""Rehearse a captured 1.0 schema with synthetic rows in an owned local PostgreSQL 17.

This does not connect to production or prove recovery of production data. The input is a
schema-only pg_dump plus schema_versions rows; it is imported unchanged by psql. Example:

  python3 deploy/rollback/rehearse-schema.py target/release-production-check/production-schema.sql \
      --sha256 <independently-recorded-sha256>

Docker must already have the selected PostgreSQL image. No shared database or volume is used.
"""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import secrets
import subprocess
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("schema", type=Path)
    parser.add_argument("--sha256", required=True)
    parser.add_argument("--image", default="postgres:17-bookworm")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[2]
    source = args.schema.resolve()
    dump = source.read_bytes()
    digest = hashlib.sha256(dump).hexdigest()
    if digest != args.sha256.lower():
        parser.error("schema SHA-256 differs from the independently recorded value")
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = (args.output or repo / "target/rollback-verification/production-copy" / stamp).resolve()
    output.mkdir(parents=True, exist_ok=False)
    suffix = uuid.uuid4().hex
    container = f"extend-schema-rehearsal-{suffix}"
    database = f"extend_copy_{suffix}"
    report = {
        "started_at": stamp,
        "scope": "Production schema copy with synthetic fixtures; not a production-data backup rehearsal",
        "source": str(source),
        "source_sha256": digest,
        "source_bytes": len(dump),
        "image": args.image,
        "owned_container": container,
        "owned_database": database,
        "status": "running",
        "cleanup": {},
    }
    created = False

    def run(command, **kwargs):
        return subprocess.run(command, cwd=repo, check=True, capture_output=True, **kwargs)

    def psql(sql, *, db="postgres"):
        return run([
            "docker", "exec", "-i", container, "psql", "-X", "-U", "extend", "-d", db,
            "-v", "ON_ERROR_STOP=1", "-At",
        ], input=sql, timeout=60)

    def cargo(log_name, arguments, env):
        with (output / log_name).open("wb") as log:
            result = subprocess.run(["cargo", "test", "-p", "extend-service", "--test", "migration", *arguments],
                                    cwd=repo, env=env, stdout=log, stderr=subprocess.STDOUT, timeout=900)
        report[log_name] = {"exit_code": result.returncode}
        if result.returncode:
            raise RuntimeError(f"test failed; see {output / log_name}")

    try:
        report["git_head"] = run(["git", "rev-parse", "HEAD"], text=True).stdout.strip()
        inputs = [Path("crates/extend-service/src/db.rs"), Path("crates/extend-service/tests/migration.rs"),
                  Path("deploy/rollback/1.1-to-1.0.sql"), Path("deploy/rollback/rehearse-schema.py")]
        report["rehearsal_input_sha256"] = {
            str(path): hashlib.sha256((repo / path).read_bytes()).hexdigest() for path in inputs
        }
        report["image_id"] = run(["docker", "image", "inspect", args.image, "--format", "{{.Id}}"], text=True).stdout.strip()
        password = secrets.token_hex(24)
        started = run([
            "docker", "run", "-d", "--pull", "never", "--name", container,
            "--tmpfs", "/var/lib/postgresql/data:rw", "-e", "POSTGRES_USER=extend",
            "-e", f"POSTGRES_PASSWORD={password}", "-e", "POSTGRES_DB=postgres",
            "-p", "127.0.0.1::5432", args.image,
        ], text=True, timeout=60)
        created = True
        report["container_id"] = started.stdout.strip()
        deadline = time.monotonic() + 60
        while True:
            ready = subprocess.run(["docker", "exec", container, "pg_isready", "-h", "127.0.0.1", "-U", "extend"],
                                   capture_output=True, timeout=10)
            if ready.returncode == 0:
                break
            if time.monotonic() >= deadline:
                raise RuntimeError("owned PostgreSQL did not become ready")
            time.sleep(0.2)
        binding = run(["docker", "port", container, "5432/tcp"], text=True).stdout.strip()
        host, port = binding.rsplit(":", 1)
        assert host == "127.0.0.1" and port.isdigit(), binding
        report["postgres_version"] = psql(b"SHOW server_version;").stdout.decode().strip()
        assert report["postgres_version"].startswith("17."), "use PostgreSQL 17 for this schema copy"
        psql(f"CREATE DATABASE {database};\n".encode())
        imported = psql(dump, db=database)
        (output / "schema-import.log").write_bytes(imported.stdout + imported.stderr)
        report["source_sha256_after_import"] = hashlib.sha256(source.read_bytes()).hexdigest()
        assert report["source_sha256_after_import"] == digest, "source changed during import"
        env = dict(os.environ)
        base = f"postgres://extend:{password}@127.0.0.1:{port}"
        env["EXTEND_MIGRATION_SCHEMA_DATABASE_URL"] = f"{base}/{database}"
        env["EXTEND_TEST_ADMIN_URL"] = f"{base}/postgres"
        print(f"Imported exact schema into owned {database}; running copied-schema rehearsal", flush=True)
        cargo("copied-schema.log", ["copied_production_schema_rolls_backward_and_forward", "--", "--ignored", "--exact", "--nocapture"], env)
        print("Copied-schema rehearsal passed; running all standard migration regressions on the same PostgreSQL 17", flush=True)
        cargo("migration-suite.log", ["--", "--nocapture"], env)
        remaining = psql(b"SELECT datname FROM pg_database WHERE datname LIKE 'extend_mig_%' ORDER BY datname;\n").stdout.decode().strip()
        assert not remaining, f"standard migration test failed to clean owned database(s): {remaining}"
        report["standard_migration_databases_remaining"] = []
        report["status"] = "passed"
    except Exception as error:
        report["status"] = "failed"
        report["error"] = str(error)
    finally:
        if created:
            try:
                psql(f"DROP DATABASE IF EXISTS {database} WITH (FORCE);\n".encode())
                exists = psql(f"SELECT count(*) FROM pg_database WHERE datname = '{database}';\n".encode()).stdout.decode().strip()
                assert exists == "0", exists
                report["cleanup"]["owned_database_absent"] = True
            except Exception as error:
                report["cleanup"]["database_error"] = str(error)
                report["status"] = "failed"
            try:
                logs = run(["docker", "logs", container], timeout=20)
                (output / "postgres.log").write_bytes(logs.stdout + logs.stderr)
            except Exception as error:
                report["cleanup"]["log_capture_error"] = str(error)
            try:
                run(["docker", "rm", "-fv", container], timeout=30)
                inspect = subprocess.run(["docker", "inspect", container], capture_output=True, timeout=20)
                assert inspect.returncode != 0, "owned container still exists"
                report["cleanup"]["owned_container_absent"] = True
            except Exception as error:
                report["cleanup"]["container_error"] = str(error)
                report["status"] = "failed"
        report["source_sha256_after_run"] = hashlib.sha256(source.read_bytes()).hexdigest()
        if report["source_sha256_after_run"] != digest:
            report["status"] = "failed"
            report["error"] = "source changed during rehearsal"
        report["finished_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(f"{report['status']}: {output / 'report.json'}", flush=True)
    return 0 if report["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
