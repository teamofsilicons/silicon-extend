"""package-cli.py end to end with the real `honeycomb` CLI and stand-in executables.

`honeycomb pack` keeps only honeycomb.yaml and the target roots, so anything staged elsewhere is
dropped without a word; the licences must land inside every target. Skipped without `honeycomb` on
PATH (the release workflow installs it).

    python3 -m unittest discover -s scripts -p 'test_*.py'
"""
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / 'scripts' / 'package-cli.py'


def elf(machine):
    header = bytearray(64)
    header[:7] = b'\x7fELF\x02\x01\x01'
    struct.pack_into('<HH', header, 16, 2, machine)
    return bytes(header)


def pe(machine):
    header = bytearray(64)
    header[:2] = b'MZ'
    struct.pack_into('<I', header, 60, 64)
    return bytes(header) + b'PE\0\0' + struct.pack('<H', machine)


def macho(cpu):
    header = bytearray(64)
    header[:4] = b'\xcf\xfa\xed\xfe'
    struct.pack_into('<I', header, 4, cpu)
    return bytes(header)


STAND_INS = {
    'linux-x86_64': ('extend', elf(62)),
    'linux-aarch64': ('extend', elf(183)),
    'windows-x86_64': ('extend.exe', pe(0x8664)),
    'windows-aarch64': ('extend.exe', pe(0xAA64)),
    'macos-x86_64': ('extend', macho(0x01000007)),
    'macos-aarch64': ('extend', macho(0x0100000C)),
}


@unittest.skipUnless(shutil.which('honeycomb'), 'honeycomb is not on PATH')
class PackageCli(unittest.TestCase):
    def test_every_target_ships_the_licences(self):
        with tempfile.TemporaryDirectory() as temporary:
            work = Path(temporary)
            for target, (binary, content) in STAND_INS.items():
                path = work / 'artifacts' / target / binary
                path.parent.mkdir(parents=True)
                path.write_bytes(content)
            run = subprocess.run(
                [sys.executable, str(SCRIPT), '--artifacts', str(work / 'artifacts'), '--output', str(work / 'dist')],
                capture_output=True, text=True,
            )
            self.assertEqual(run.returncode, 0, run.stdout + run.stderr)
            [archive] = (work / 'dist').glob('*.tar.gz')
            with tarfile.open(archive, 'r:gz') as packed:
                names = {m.name.removeprefix('./') for m in packed.getmembers() if m.isfile()}
            for target in STAND_INS:
                for name in ('LICENSE', 'THIRD_PARTY_NOTICES.md', 'THIRD_PARTY_LICENSES.txt'):
                    self.assertIn(f'targets/{target}/licences/{name}', names)


if __name__ == '__main__':
    unittest.main()
