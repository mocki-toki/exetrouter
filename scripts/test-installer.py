#!/usr/bin/env python3
"""Exercise the installer offline with synthetic release assets and failures."""
import hashlib
import io
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parent / 'install.sh'

class Installer(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.bin = self.root / 'tools'
        self.bin.mkdir()
        curl = self.bin / 'curl'
        curl.write_text('''#!/bin/sh
for arg in "$@"; do case "$arg" in https:*) url=$arg ;; esac; done
while [ "$#" -gt 0 ]; do if [ "$1" = -o ]; then dest=$2; break; fi; shift; done
case "$url" in */SHA256SUMS) cp "$FIXTURE_ROOT/SHA256SUMS" "$dest" ;; *) cp "$FIXTURE_ROOT/asset.tar.gz" "$dest" ;; esac
''')
        curl.chmod(0o755)
        self.env = dict(os.environ, PATH=f'{self.bin}:{os.environ["PATH"]}', FIXTURE_ROOT=str(self.root))
        arch = 'aarch64' if platform.machine() in ('arm64', 'aarch64') else 'x86_64'
        target = 'apple-darwin' if platform.system() == 'Darwin' else 'unknown-linux-gnu'
        self.asset_name = f'exetrouter-{arch}-{target}.tar.gz'
        self.prefix = self.root / 'install with spaces'
    def tearDown(self):
        self.temp.cleanup()
    def archive(self, entries=('exr', 'exrd', 'LICENSE'), corrupt=False, bad_binary=None):
        with tarfile.open(self.root/'asset.tar.gz', 'w:gz') as archive:
            for name in entries:
                data = b'#!/bin/sh\nexit 1\n' if name == bad_binary else b'#!/bin/sh\nexit 0\n'
                info = tarfile.TarInfo(name)
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        digest = '0'*64 if corrupt else hashlib.sha256((self.root/'asset.tar.gz').read_bytes()).hexdigest()
        (self.root/'SHA256SUMS').write_text(f'{digest}  {self.asset_name}\n')
    def run_installer(self, *args):
        return subprocess.run(['sh',str(INSTALLER),'--prefix',str(self.prefix),*args],env=self.env,capture_output=True)
    def test_roles_and_permissions(self):
        self.archive()
        result = self.run_installer('--role','client')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue((self.prefix/'bin/exr').is_file())
        self.assertFalse((self.prefix/'bin/exrd').exists())
        self.assertEqual((self.prefix/'bin/exr').stat().st_mode & 0o777,0o755)
        result=self.run_installer('--role','both','--version','v0.1.0')
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertTrue((self.prefix/'bin/exrd').is_file())
    def test_bad_checksum_never_installs(self):
        self.archive(corrupt=True)
        self.assertNotEqual(self.run_installer().returncode,0)
        self.assertFalse((self.prefix/'bin/exr').exists())
    def test_traversal_refused(self):
        self.archive(entries=('exr','../escaped'))
        self.assertNotEqual(self.run_installer().returncode,0)
        self.assertFalse((self.prefix/'bin/exr').exists())
    def test_incompatible_release_never_replaces_binaries(self):
        self.archive(bad_binary="exrd")
        result = self.run_installer("--role", "both")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"--from-source", result.stderr)
        self.assertFalse((self.prefix/"bin/exr").exists())
        self.assertFalse((self.prefix/"bin/exrd").exists())
    def test_unknown_options_rejected(self):
        self.assertNotEqual(self.run_installer('--role','admin').returncode,0)
        self.assertNotEqual(self.run_installer('--version','../../bad').returncode,0)

class HomebrewFormula(unittest.TestCase):
    def test_formula_uses_release_checksums_and_installs_only_client(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location("formula_generator", Path(__file__).with_name("homebrew-formula.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        sums = "a" * 64 + "  exetrouter-aarch64-apple-darwin.tar.gz\n" + "b" * 64 + "  exetrouter-x86_64-apple-darwin.tar.gz\n"
        formula = module.formula("0.2.0", sums)
        self.assertIn('version "0.2.0"', formula)
        self.assertIn('sha256 "' + "a" * 64 + '"', formula)
        self.assertIn('bin.install "exr"', formula)
        self.assertNotIn('cargo', formula)
        with self.assertRaises(ValueError):
            module.formula("../../bad", sums)
        with self.assertRaises(ValueError):
            module.formula("0.2.0", sums + sums)
        with self.assertRaises(KeyError):
            module.formula("0.2.0", "")

if __name__ == '__main__':
    unittest.main()
