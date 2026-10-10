"""deploy/aws/refresh-host-helper.py: the helper it extracts is the one first boot writes, and its
commands replace a helper (keeping the old one) when run, here against a scratch directory."""
import base64
import hashlib
import importlib.util
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("refresh_host_helper", HERE / "refresh-host-helper.py")
refresh = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(refresh)


class Extraction(unittest.TestCase):
    def test_every_helper_is_a_bash_script_without_cloudformation_substitutions(self):
        for name in refresh.HELPERS:
            with self.subTest(helper=name):
                script = refresh.helper_script(name)
                self.assertTrue(script.startswith("#!/bin/bash\n"))
                self.assertNotIn("${", script)
                self.assertIn("\nset -Eeuo pipefail\n", script)  # dedented like YAML's block scalar
                if shutil.which("bash"):
                    subprocess.run(["bash", "-n"], input=script, text=True, check=True)

    def test_the_renderer_is_extend_4s(self):
        script = refresh.helper_script("extend-render-env")
        self.assertIn("'EXTEND_APP_SECRET', 'EXTEND_ACCOUNTS_WEBHOOK_SECRET'", script)
        self.assertNotIn("'EXTEND_TING_URL': 'https://", script)
        required = re.search(r"required = \((.*?)\)", script, re.S).group(1)
        self.assertNotIn("IAM", required)
        self.assertNotIn("HONEYCOMB", required)

    def test_a_helper_with_a_substitution_or_an_unknown_name_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            template = Path(directory) / "standalone.yaml"
            template.write_text("            cat > /usr/local/sbin/extend-release <<'SCRIPT'\n"
                                "            #!/bin/bash\n            echo ${LogGroup}\n            SCRIPT\n")
            with self.assertRaisesRegex(SystemExit, "substitutes at first boot"):
                refresh.helper_script("extend-release", template)
        with self.assertRaisesRegex(SystemExit, "unknown helper"):
            refresh.helper_script("extend-anything")


@unittest.skipUnless(shutil.which("bash") and shutil.which("base64"), "needs bash and base64")
class Commands(unittest.TestCase):
    def test_the_commands_replace_the_helper_and_keep_the_old_one(self):
        script = refresh.helper_script("extend-render-env")
        commands = refresh.remote_commands("extend-render-env", script)
        encoded = "".join(re.fullmatch(r"printf '%s' '([^']*)' >> \"\$helper\.b64\"", c).group(1)
                          for c in commands if c.startswith("printf"))
        self.assertEqual(base64.b64decode(encoded).decode(), script)
        with tempfile.TemporaryDirectory() as directory:
            sbin = Path(directory)
            (sbin / "extend-render-env").write_text("#!/bin/bash\necho extend 3\n")
            local = [c.replace("/usr/local/sbin", str(sbin)) for c in commands if not c.startswith("chown ")]
            run = subprocess.run(["bash", "-c", "\n".join(local)], capture_output=True, text=True, check=False)
            self.assertEqual(run.returncode, 0, run.stderr)
            self.assertEqual((sbin / "extend-render-env").read_text(), script)
            self.assertEqual((sbin / "extend-render-env").stat().st_mode & 0o777, 0o750)
            [backup] = sbin.glob("extend-render-env.*.bak")
            self.assertEqual(backup.read_text(), "#!/bin/bash\necho extend 3\n")
            self.assertIn(hashlib.sha256(script.encode()).hexdigest(), run.stdout)
            self.assertEqual(sorted(p.name for p in sbin.iterdir()), sorted(["extend-render-env", backup.name]))

    def test_without_send_nothing_is_sent(self):
        self.assertEqual(refresh.main(["extend-release"]), 0)


if __name__ == "__main__":
    unittest.main()
