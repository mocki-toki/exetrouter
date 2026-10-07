#!/usr/bin/env python3
"""Offline compatibility automation boundary and flow tests."""
import copy
import importlib.util
import json
import os
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scripts.compat_agent.core import (GitHub, Ledger, MODEL, STATE_BRANCH, Stop,
                                      collect, decision, digest, source_path, writable)
from scripts.compat_agent.model import Model
from scripts.compat_agent.sandbox import Sandbox, command
from scripts.compat_agent.__main__ import publish

ROOT = Path(__file__).resolve().parent.parent
SHA = "a" * 40
NEW = "b" * 40


def verdict(kind="no_change"):
    return {"decision": kind, "summary": "Synthetic conclusion", "coverage_complete": True,
            "findings": []}


def finding():
    cite = {"sha": SHA, "path": "src/example.rs", "symbol": "example"}
    return {"problem": "Changed contract", "fix": "Minimal fix", "test": "Regression",
            "upstream": [cite], "exetrouter": [cite]}


class FakeGH:
    repo = "example/router"

    def __init__(self):
        self.writes = []
        self.pull_requests = []
        self.head = None
        self.state = None

    def own(self, path, method="GET", body=None):
        if method != "GET":
            self.writes.append((path, method, body))
            if path == "git/blobs":
                try:
                    self.state = json.loads(body["content"])
                except ValueError:
                    pass
            if path == "pulls":
                return {"number": 42}
            return {"sha": NEW}
        if path.startswith("git/ref/heads/"):
            if self.head is None:
                raise Stop("http_404")
            return {"object": {"sha": self.head}}
        if path.startswith("git/commits/"):
            return {"tree": {"sha": SHA}}
        raise AssertionError(path)

    def file(self, repo, sha, path):
        if path == "state.json":
            return json.dumps(self.state)
        return "fn example() {}"

    def resolve(self, repo, ref):
        return SHA

    def prs(self):
        return self.pull_requests


class PolicyTests(unittest.TestCase):
    def test_packaging_excludes_python_bytecode(self):
        import fnmatch
        patterns = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["include"]
        for name in ("scripts/compat_agent/__pycache__/core.cpython-312.pyc",
                     "scripts/compat_agent/core.pyc"):
            self.assertFalse(any(fnmatch.fnmatch(name, p) for p in patterns))
        self.assertTrue(any(fnmatch.fnmatch("scripts/compat_agent/core.py", p) for p in patterns))

    def test_paths(self):
        for path in ("../secret", "/secret", "src/../secret", "src\\secret", "src//test.rs", "src/\nsecret"):
            with self.subTest(path=path), self.assertRaises(Stop):
                source_path(path)
        self.assertTrue(writable("src/server.rs"))
        self.assertTrue(writable("tests/new.rs"))
        for path in ("AGENTS.md", "SECURITY.md", ".github/workflows/ci.yml", "Cargo.toml",
                      "src/migrations/10.sql", "scripts/check-publication.py",
                      "docs/protocol-sources.json", "docs/compat-agent.md", "docs/assets/new.jpg",
                      "tests/AGENTS.md", "src/CLAUDE.md", "docs/SECURITY.md"):
            self.assertFalse(writable(path), path)

    def test_decisions_fail_closed(self):
        for value in ({}, {**verdict(), "coverage_complete": False},
                      {**verdict(), "decision": "confirmed"}, verdict("code_change")):
            with self.assertRaises(Stop):
                decision("triage", value)
        good = {**verdict("code_change"), "findings": [finding()]}
        self.assertEqual(decision("triage", good), good)
        good["findings"][0]["upstream"][0]["sha"] = "latest"
        with self.assertRaises(Stop):
            decision("triage", good)

    def test_hash_canonical(self):
        self.assertEqual(digest({"a": 1, "b": 2}), digest({"b": 2, "a": 1}))

    def test_state_write_ahead_and_no_repeat(self):
        gh = FakeGH()
        state = Ledger(gh)
        self.assertTrue(state.claim("key", "codex", SHA))
        self.assertEqual(gh.state["attempts"]["key"]["status"], "started")
        self.assertFalse(state.claim("key", "codex", SHA))
        state.finish("key", "uncertain")
        self.assertNotIn("codex", state.value["cursors"])
        self.assertFalse(state.claim("key", "codex", SHA))
        state.finish("key", "published", 42)
        self.assertEqual(state.value["cursors"]["codex"], SHA)
        refs = [b for p, m, b in gh.writes if m == "PATCH"]
        self.assertTrue(refs)
        self.assertTrue(all(b["force"] is False for b in refs))

    def test_closed_pr_not_reopened(self):
        gh = FakeGH()
        gh.pull_requests = [{"number": 7, "state": "closed", "head": {"ref": "automation/compat/key"},
                             "body": "<!-- exetrouter-compat:key -->"}]
        result = publish(gh, {"version": None}, SHA, "key", {}, {"title": "A synthetic fix",
                         "body": "A sufficiently long explanation of the synthetic compatibility mismatch."})
        self.assertEqual(result, 7)
        self.assertEqual(gh.writes, [])

    def test_pr_draft_and_fixed_base(self):
        gh = FakeGH()
        self.assertEqual(publish(gh, {"version": None}, SHA, "key", {"src/example.rs": "fn example() {}"},
                                {"title": "A synthetic fix", "body": "x" * 60}), 42)
        body = next(b for p, m, b in gh.writes if p == "pulls")
        self.assertTrue(body["draft"])
        self.assertEqual(body["base"], "main")

    def test_moved_base_no_publication(self):
        gh = FakeGH()
        with self.assertRaises(Stop):
            publish(gh, {"version": "1.0.0"}, NEW, "key", {},
                    {"title": "Synthetic fix", "body": "x" * 60})
        self.assertFalse(gh.writes)

    def test_incomplete_compare(self):
        gh = FakeGH()
        gh.api = lambda path: ({"default_branch": "dev"} if "compare" not in path else
                               {"status": "ahead", "files": [{}] * 300})
        manifest = {"projects": [{"id": i, "commit": NEW} for i in ("codex", "opencode", "opencode-v1")]}
        with patch("scripts.compat_agent.core.request", return_value={"version": "1.0.0"}):
            events = collect(gh, manifest, {})
            self.assertTrue(events)
            self.assertTrue(all(e.get("blocked") == "comparison_incomplete_or_diverged" for e in events))


class FakeBox:
    def __init__(self):
        self.calls = []

    def validate_citations(self, value):
        pass

    def dispatch(self, name, args, editable=False):
        self.calls.append((name, args, editable))
        return {"content": "Synthetic untrusted source"}


class ModelTests(unittest.TestCase):
    def setUp(self):
        self.env = patch.dict(os.environ, {"COMPAT_API_BASE_URL": "https://api.example.com/v1",
                                          "COMPAT_API_TOKEN": "synthetic-secret"})
        self.env.start()
        self.addCleanup(self.env.stop)

    def response(self, name, args, usage=True):
        return {"status": "completed", "usage": {"input_tokens": 100, "output_tokens": 10} if usage else None,
                "output": [{"type": "reasoning", "encrypted_content": "synthetic-opaque"},
                           {"type": "function_call", "call_id": "call_1", "name": name,
                            "arguments": json.dumps(args)}]}

    def test_efforts_full_history_and_blind_verify(self):
        calls = []
        queue = [self.response("submit_preliminary", verdict("rejected")),
                 self.response("submit_decision", verdict("rejected"))]
        def api(url, token, method, body):
            calls.append(copy.deepcopy(body))
            return queue.pop(0)
        model = Model(ROOT, api)
        self.assertEqual(model.run("verify", {"original": "evidence"}, FakeBox(),
                                   triage={"marker": "TRIAGE_ONLY"})["decision"], "rejected")
        self.assertNotIn("TRIAGE_ONLY", json.dumps(calls[0]))
        self.assertIn("TRIAGE_ONLY", json.dumps(calls[1]))
        self.assertIn("synthetic-opaque", json.dumps(calls[1]["input"]))
        for call in calls:
            self.assertEqual(call["model"], MODEL)
            self.assertEqual(call["reasoning"]["effort"], "xhigh")
            self.assertFalse(call["store"])
            self.assertNotIn("previous_response_id", call)

    def test_verify_must_submit_independent_verdict(self):
        model = Model(ROOT, lambda *a: self.response("submit_decision", verdict("rejected")))
        with self.assertRaisesRegex(Stop, "independent_verdict_missing"):
            model.run("verify", {}, FakeBox(), triage={})

    def test_preliminary_verdict_requires_complete_evidence(self):
        for args in ({"summary": "I will analyze"}, verdict("confirmed"),
                     {**verdict("rejected"), "coverage_complete": False}):
            with self.subTest(args=args), self.assertRaises(Stop):
                model = Model(ROOT, lambda *a: self.response("submit_preliminary", args))
                model.run("verify", {}, FakeBox(), triage={})

    def test_preliminary_citations_are_verified_before_reveal(self):
        box = FakeBox()
        def reject_citations(value):
            raise Stop("citation_not_found")
        box.validate_citations = reject_citations
        model = Model(ROOT, lambda *a: self.response("submit_preliminary", {
            **verdict("confirmed"), "findings": [finding()]}))
        with self.assertRaisesRegex(Stop, "citation_not_found"):
            model.run("verify", {}, box, triage={})

    def test_medium_cannot_edit(self):
        requests = []
        def api(url, token, method, body):
            requests.append(body)
            return self.response("submit_decision", verdict())
        Model(ROOT, api).run("triage", {}, FakeBox())
        self.assertEqual(requests[0]["reasoning"]["effort"], "medium")
        self.assertNotIn("write_file", [t["name"] for t in requests[0]["tools"]])

    def test_unknown_usage_stops_next_request(self):
        count = []
        def api(*args):
            count.append(1)
            return self.response("read_file", {}, usage=False)
        with self.assertRaisesRegex(Stop, "unknown_usage"):
            Model(ROOT, api).run("triage", {}, FakeBox())
        self.assertEqual(len(count), 1)

    def test_no_inference_retry(self):
        calls = []
        def api(*args):
            calls.append(1)
            raise Stop("transport_or_json_error")
        with self.assertRaises(Stop):
            Model(ROOT, api).run("triage", {}, FakeBox())
        self.assertEqual(len(calls), 1)

    def test_preflight_fail_closed(self):
        model = Model(ROOT, lambda *a: {"models": [{"slug": MODEL,
                      "supported_reasoning_levels": [{"effort": "medium"}]}]})
        with self.assertRaisesRegex(Stop, "reasoning_levels_unavailable"):
            model.preflight()

    def test_unsafe_url(self):
        for url in ("http://api.example.com/v1", "https://secret@api.example.com/v1",
                    "https://api.example.com/v1?token=secret"):
            with patch.dict(os.environ, {"COMPAT_API_BASE_URL": url}), self.assertRaises(Stop):
                Model(ROOT)


class SandboxTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.box = object.__new__(Sandbox)
        self.box.path = Path(temp.name)
        self.box.root = ROOT
        self.box.changed = set()
        self.box.results = {"tests": True}
        self.box.started = False

    def test_check_invalidated_by_edit(self):
        self.box.dispatch("write_file", {"path": "tests/new.rs", "content": "fn example() {}"}, True)
        self.assertEqual(self.box.results, {})
        self.assertEqual(self.box.bundle(), {"tests/new.rs": "fn example() {}"})

    def test_readonly_and_protected_tools(self):
        for name, args in (("write_file", {"path": "src/new.rs", "content": ""}),
                           ("shell", {"command": "env"})):
            with self.assertRaises(Stop):
                self.box.dispatch(name, args)
        with self.assertRaises(Stop):
            self.box.dispatch("write_file", {"path": "scripts/new.py", "content": ""}, True)

    def test_symlink_escape(self):
        (self.box.path / "src").symlink_to(self.box.path.parent)
        with self.assertRaises(Stop):
            self.box.dispatch("write_file", {"path": "src/escape.rs", "content": "x"}, True)

    def test_private_endpoint_not_publishable(self):
        self.box.dispatch("write_file", {"path": "docs/new.md", "content": "https://" + "private.invalid-domain.dev"}, True)
        with self.assertRaisesRegex(Stop, "endpoint_rejected"):
            self.box.bundle()

    def test_real_credentials_not_publishable(self):
        secret = "ghp_" + "a" * 40
        self.box.dispatch("write_file", {"path": "tests/new.rs", "content": secret}, True)
        with self.assertRaisesRegex(Stop, "secret_rejected"):
            self.box.bundle()

    def test_citations_must_exist(self):
        self.box.gh = FakeGH()
        self.box.event = {"repository": "example/upstream"}
        self.box.allowed_revisions = {("example/upstream", SHA), ("example/router", SHA)}
        self.box.validate_citations({"findings": [finding()]})
        bad = finding()
        bad["upstream"][0]["symbol"] = "nonexistent"
        with self.assertRaisesRegex(Stop, "citation_not_found"):
            self.box.validate_citations({"findings": [bad]})

    def test_command_output_is_bounded_while_running(self):
        with patch("scripts.compat_agent.sandbox.MAX_BYTES", 128), self.assertRaisesRegex(
                Stop, "command_output_too_large"):
            command([sys.executable, "-c", "import sys; sys.stdout.write('x'*100000)"])

    def test_command_timeout(self):
        with self.assertRaisesRegex(Stop, "command_failed_or_timed_out"):
            command([sys.executable, "-c", "import time; time.sleep(10)"], timeout=0.1)


class LoaderTests(unittest.TestCase):
    def test_pinned_version_and_tag(self):
        from types import SimpleNamespace
        spec = importlib.util.spec_from_file_location("client_loader", ROOT / "scripts/fetch-test-clients.py")
        loader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(loader)
        args = SimpleNamespace(manifest=Path("synthetic.json"),
                               pins={"codex": {"version": "0.1.0", "commit": SHA}})
        with patch.object(loader, "metadata", side_effect=AssertionError("must not resolve latest")):
            self.assertEqual(loader.selected_version(args, "codex", "@openai/codex"), "0.1.0")
        loader.verify_revision(args, "codex", SHA)
        with self.assertRaises(RuntimeError):
            loader.verify_revision(args, "codex", NEW)

    def test_native_manifest_restored_on_failure(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as temp:
            box = object.__new__(Sandbox)
            box.path = Path(temp)
            box.clients = box.path / "clients"
            box.clients.mkdir()
            binary = box.clients / "v1/bin/opencode"
            (box.clients / "current-opencode-v1.json").write_text(json.dumps({"binary": str(binary)}))
            box.event = {"track": "opencode-v1"}
            box.pins = {"projects": []}
            box.started = True
            box.name = "synthetic"
            manifest = box.path / "docs/protocol-sources.json"
            manifest.parent.mkdir()
            manifest.write_text("original")
            with patch("scripts.compat_agent.sandbox.command", return_value=SimpleNamespace(
                    returncode=0, stdout=b"0 passed", stderr=b"")):
                self.assertFalse(box.native_check()["passed"])
            self.assertEqual(manifest.read_text(), "original")


if __name__ == "__main__":
    unittest.main()
