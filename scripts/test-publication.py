#!/usr/bin/env python3
"""Offline regression checks for the publication allowlist and redacted failures."""
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("check-publication.py")
spec = importlib.util.spec_from_file_location("publication", SCRIPT)
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class PublicationTests(unittest.TestCase):
    def test_tracked_deletions_are_ignored_but_existing_secrets_and_dangling_links_fail(self):
        with tempfile.TemporaryDirectory(prefix="exetrouter-publication-") as temp:
            root = Path(temp)
            subprocess.run(["git", "init", "--quiet", temp], check=True)
            docs = root / "docs"
            docs.mkdir()
            removed = docs / "removed.md"
            removed.write_text("Removed feature\n")
            subprocess.run(["git", "add", "docs/removed.md"], cwd=root, check=True)
            removed.unlink()
            result = subprocess.run([sys.executable, str(SCRIPT), "--root", temp], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            kept = docs / "kept.md"
            secret = "exr_tok_" + "0" * 16 + "_" + "a" * 64
            kept.write_text(secret)
            subprocess.run(["git", "add", "docs/kept.md"], cwd=root, check=True)
            result = subprocess.run([sys.executable, str(SCRIPT), "--root", temp], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn(secret, result.stderr)
            kept.write_text("Safe metadata\n")
            broken = docs / "broken.md"
            broken.symlink_to("missing.md")
            subprocess.run(["git", "add", "docs/broken.md"], cwd=root, check=True)
            result = subprocess.run([sys.executable, str(SCRIPT), "--root", temp], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
    def test_desktop_package_is_narrowly_allowlisted(self):
        for name in ["package.json", "package-lock.json", "README.md", "src/gui.tsx", "src/window.css", "tests/catalog.test.ts"]:
            self.assertTrue(audit.allowed_path(f"clients/opencode-desktop/{name}"))
        for name in ["clients/other/index.ts", "clients/opencode-desktop/dist/gui-experimental.cjs", "clients/opencode-desktop/node_modules/package/index.ts", "clients/opencode-desktop/src/private.key", "clients/opencode-desktop/.env", "clients/opencode-desktop/state.sqlite"]:
            self.assertFalse(audit.allowed_path(name))

    def test_vscode_extension_only_allows_source_and_package_metadata(self):
        for name in ["package.json", "package-lock.json", "README.md", "src/extension.ts", "tests/protocol.test.ts", "assets/router.svg"]:
            self.assertTrue(audit.allowed_path(f"clients/vscode/{name}"))
        for name in ["dist/extension.js", "out/extension.js", "exetrouter.vsix", "src/.env", "src/private.key", "tests/node_modules/example.ts", "state.sqlite"]:
            self.assertFalse(audit.allowed_path(f"clients/vscode/{name}"))

    def test_vscode_svg_exception_allows_only_the_icon_namespace(self):
        icon = "clients/vscode/assets/router.svg"
        namespace = "http://" + "www.w3.org/2000/svg"
        self.assertEqual(self.check_file(icon, f'<svg xmlns="{namespace}"/>').returncode, 0)
        self.assertNotEqual(self.check_file(icon, f'<svg xmlns="{namespace}/other"/>').returncode, 0)
        self.assertNotEqual(self.check_file("docs/example.md", namespace).returncode, 0)

    def check_file(self, name, text):
        with tempfile.TemporaryDirectory(prefix="exetrouter-publication-") as temp:
            path = Path(temp) / name
            path.parent.mkdir(parents=True)
            path.write_text(text)
            return subprocess.run([sys.executable, str(SCRIPT), "--root", temp], capture_output=True, text=True)

    def test_lockfile_funding_exception_is_scoped_and_credentials_still_fail(self):
        lock = "clients/opencode-desktop/package-lock.json"
        funding = "https://" + "opencollective.com/example"
        self.assertEqual(self.check_file(lock, '{"funding":"' + funding + '"}').returncode, 0)
        self.assertNotEqual(self.check_file("docs/example.md", funding).returncode, 0)
        secret = "exr_tok_" + "0" * 16 + "_" + "a" * 64
        result = self.check_file(lock, '{"secret":"' + secret + '"}')
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(secret, result.stderr)

    def test_unreviewed_endpoints_fail_without_echoing_the_url(self):
        url = "https://" + "unreviewed-endpoint.invalid-domain/v1"
        result = self.check_file("clients/opencode-desktop/src/example.ts", url)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(url, result.stderr)


if __name__ == "__main__":
    unittest.main()
