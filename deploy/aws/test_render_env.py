"""Run the actual CloudFormation-embedded environment renderer against disposable files."""
import contextlib
import io
import json
from pathlib import Path
import stat
import tempfile
import textwrap
import unittest


class RuntimeRendererTests(unittest.TestCase):
    tuning = {
        "EXTEND_MAX_PAIRS_PER_DEVICE": "3",
        "EXTEND_MEMBERSHIP_SWEEP_HOURS": "2",
        "EXTEND_OWNER_CHECK_CACHE_S": "0",
        "EXTEND_OWNER_CHECK_AT_USE": "false",
        "EXTEND_TEST_LINK_WINDOW_S": "45",
    }

    def render(self, overrides):
        template = Path(__file__).with_name("standalone.yaml").read_text()
        renderer = template.split("cat > /usr/local/sbin/extend-render-env <<'SCRIPT'", 1)[1]
        renderer = renderer.split("python3 - <<'PY'\n", 1)[1].split("\n            PY", 1)[0]
        source = {
            "EXTEND_DATABASE_URL": "postgres://fixture@localhost/fixture?sslmode=verify-full",
            "EXTEND_IAM_APP_ID": "fixture",
            "EXTEND_IAM_APP_SECRET": "fixture-only",
            "EXTEND_IAM_WEBHOOK_SECRET": "fixture-only",
            "EXTEND_HONEYCOMB_SERVICE_TOKEN": "fixture-only",
            "EXTEND_POSTMARK_SERVER_TOKEN": "fixture-only",
            "EXTEND_REPORT_RECIPIENTS": "fixture@example.invalid",
            **overrides,
        }
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

    def test_all_11_controls_reach_the_service_including_disabled_owner_check(self):
        values, log = self.render(self.tuning)
        self.assertEqual({key: values.get(key) for key in self.tuning}, self.tuning)
        self.assertNotIn("ignoring unknown keys", log)
        self.assertEqual(values["EXTEND_ENVIRONMENT"], "production")

    def test_omitted_controls_preserve_service_defaults(self):
        values, _ = self.render({})
        self.assertTrue(self.tuning.keys().isdisjoint(values))

    def test_controls_cannot_inject_another_environment_assignment(self):
        for key in self.tuning:
            with self.subTest(key=key), self.assertRaisesRegex(SystemExit, key):
                self.render({key: "1\nEXTEND_ENVIRONMENT=development"})

    def test_managed_jev_configuration_reaches_service_without_logging_secrets(self):
        settings = {
            "EXTEND_JEV_API_KEY": "fixture-production-key",
            "EXTEND_JEV_TEST_API_KEY": "fixture-test-key",
            "EXTEND_JEV_URL": "https://api.typesafe.ai/v1/systemone",
            "EXTEND_JEV_MODEL": "jev-1.13.0",
        }
        values, log = self.render(settings)
        self.assertEqual({key: values.get(key) for key in settings}, settings)
        self.assertNotIn("fixture-production-key", log)
        self.assertNotIn("fixture-test-key", log)
        defaults, _ = self.render({})
        self.assertTrue(settings.keys().isdisjoint(defaults))

    def test_managed_jev_settings_cannot_inject_environment_assignments(self):
        for key in ("EXTEND_JEV_API_KEY", "EXTEND_JEV_TEST_API_KEY", "EXTEND_JEV_URL", "EXTEND_JEV_MODEL"):
            with self.subTest(key=key), self.assertRaisesRegex(SystemExit, key):
                self.render({key: "fixture\nEXTEND_ENVIRONMENT=development"})


if __name__ == "__main__":
    unittest.main()
