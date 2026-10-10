"""Run the actual CloudFormation-embedded environment renderer against disposable files."""
import contextlib
import io
import json
from pathlib import Path
import stat
import tempfile
import textwrap
import unittest

SECRETS = {
    "EXTEND_DATABASE_URL": "postgres://fixture@localhost/fixture?sslmode=verify-full",
    "EXTEND_APP_SECRET": "fixture-only",
    "EXTEND_ACCOUNTS_WEBHOOK_SECRET": "fixture-only",
    "EXTEND_POSTMARK_SERVER_TOKEN": "fixture-only",
    "EXTEND_DELEGATION_ENCRYPTION_KEY": "fixture-only-never-production",
    "EXTEND_REPORT_RECIPIENTS": "fixture@example.invalid",
}

# What an Extend 3 runtime secret holds besides the keys above; the 4.0 renderer must not pass any of
# it to the service, and must not print a value.
EXTEND_3 = {
    "EXTEND_IAM_APP_ID": "fixture",
    "EXTEND_IAM_APP_SECRET": "fixture-private-iam-secret",
    "EXTEND_IAM_WEBHOOK_SECRET": "fixture-private-iam-webhook",
    "EXTEND_IAM_WEBHOOK_SECRET_VERSION": "1",
    "EXTEND_HONEYCOMB_SERVICE_TOKEN": "fixture-private-honeycomb",
    "EXTEND_MEMBERSHIP_SWEEP_HOURS": "2",
    "EXTEND_OWNER_CHECK_AT_USE": "false",
}


class RuntimeRendererTests(unittest.TestCase):
    tuning = {
        "EXTEND_MAX_PAIRS_PER_DEVICE": "3",
        "EXTEND_DEVICE_APP_MIN_VERSION": "1.1.0",
        "EXTEND_CORS_ORIGINS": "https://extend.teamofsilicons.com",
    }

    def render(self, overrides, drop=()):
        template = Path(__file__).with_name("standalone.yaml").read_text()
        renderer = template.split("cat > /usr/local/sbin/extend-render-env <<'SCRIPT'", 1)[1]
        renderer = renderer.split("python3 - <<'PY'\n", 1)[1].split("\n            PY", 1)[0]
        source = {**SECRETS, **overrides}
        for key in drop:
            del source[key]
        with tempfile.TemporaryDirectory(prefix="extend-render-env-") as directory:
            root = Path(directory)
            (root / "stack.env").write_text("HOST_NAME=fixture.example.invalid\n")
            (root / "runtime.json").write_text(json.dumps(source))
            code = textwrap.dedent(renderer).replace("/etc/extend/", directory + "/")
            log = io.StringIO()
            with contextlib.redirect_stdout(log):
                exec(compile(code, "standalone.yaml:extend-render-env", "exec"), {})
            output = root / "runtime.env"
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o600)
            self.assertFalse((root / "runtime.json").exists())
            values = dict(line.split("=", 1) for line in output.read_text().splitlines())
            return values, log.getvalue()

    def test_silicon_accounts_settings_reach_the_service_with_production_defaults(self):
        values, log = self.render({})
        for key, value in SECRETS.items():
            self.assertEqual(values[key], value)
        self.assertEqual(values["ACCOUNTS_URL"], "https://accounts.teamofsilicons.com")
        self.assertEqual(values["EXTEND_APP_ID"], "extend")
        self.assertEqual(values["EXTEND_ENVIRONMENT"], "production")
        self.assertEqual(values["EXTEND_PUBLIC_URL"], "https://fixture.example.invalid")
        self.assertNotIn("ACCOUNTS_API_URL", values)
        self.assertEqual(log, "")

    def test_ting_stays_off_unless_the_secret_turns_it_on(self):
        values, _ = self.render({})
        self.assertNotIn("EXTEND_TING_URL", values)
        values, _ = self.render({"EXTEND_TING_URL": "https://backend.ting.teamofsilicons.com"})
        self.assertEqual(values["EXTEND_TING_URL"], "https://backend.ting.teamofsilicons.com")

    def test_the_secret_can_point_at_another_silicon_accounts(self):
        values, _ = self.render({"ACCOUNTS_URL": "https://accounts.example.invalid",
                                 "ACCOUNTS_API_URL": "https://accounts-api.example.invalid",
                                 "EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET": "fixture-only-previous"})
        self.assertEqual(values["ACCOUNTS_URL"], "https://accounts.example.invalid")
        self.assertEqual(values["ACCOUNTS_API_URL"], "https://accounts-api.example.invalid")
        self.assertEqual(values["EXTEND_ACCOUNTS_WEBHOOK_PREVIOUS_SECRET"], "fixture-only-previous")

    def test_every_required_secret_is_named_when_missing(self):
        for key in SECRETS:
            with self.subTest(key=key), self.assertRaisesRegex(SystemExit, key):
                self.render({}, drop=[key])

    def test_extend_3_settings_are_kept_out_of_the_service_and_named_without_values(self):
        values, log = self.render(EXTEND_3)
        self.assertTrue(EXTEND_3.keys().isdisjoint(values))
        self.assertFalse(any(key.startswith("EXTEND_IAM_") or "HONEYCOMB" in key for key in values))
        self.assertIn("not passing Extend 3 settings", log)
        for key in EXTEND_3:
            self.assertIn(key, log)
        for value in ("fixture-private-iam-secret", "fixture-private-iam-webhook", "fixture-private-honeycomb"):
            self.assertNotIn(value, log)
        self.assertNotIn("ignoring unknown keys", log)

    def test_tuning_controls_reach_the_service(self):
        values, log = self.render(self.tuning)
        self.assertEqual({key: values.get(key) for key in self.tuning}, self.tuning)
        self.assertNotIn("ignoring unknown keys", log)

    def test_omitted_controls_preserve_service_defaults(self):
        values, _ = self.render({})
        self.assertTrue(self.tuning.keys().isdisjoint(values))

    def test_api_version_settings_pass_through(self):
        values, _ = self.render({"EXTEND_DEPRECATED_API_VERSIONS": "1", "EXTEND_API_V2_CLI": ">=4.0.0, <5.0.0"})
        self.assertEqual(values["EXTEND_DEPRECATED_API_VERSIONS"], "1")
        self.assertEqual(values["EXTEND_API_V2_CLI"], ">=4.0.0, <5.0.0")

    def test_controls_cannot_inject_another_environment_assignment(self):
        for key in [*self.tuning, "ACCOUNTS_URL", "EXTEND_TING_URL"]:
            with self.subTest(key=key), self.assertRaisesRegex(SystemExit, key):
                self.render({key: "1\nEXTEND_ENVIRONMENT=development"})

    def test_unknown_settings_are_not_forwarded_or_logged_as_values(self):
        values, log = self.render({"EXTEND_RETIRED_PROVIDER_KEY": "fixture-private-value"})
        self.assertNotIn("EXTEND_RETIRED_PROVIDER_KEY", values)
        self.assertIn("EXTEND_RETIRED_PROVIDER_KEY", log)
        self.assertNotIn("fixture-private-value", log)

    def test_the_database_must_verify_its_certificate(self):
        with self.assertRaisesRegex(SystemExit, "sslmode=verify-full"):
            self.render({"EXTEND_DATABASE_URL": "postgres://fixture@localhost/fixture"})


if __name__ == "__main__":
    unittest.main()
