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
from scripts.compat_agent.model import MAX_CALLS, MAX_TOKENS, Model
from scripts.compat_agent.sandbox import Sandbox, command, native_category
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

    def test_incomplete_tree_blocks_observation(self):
        gh = FakeGH()
        gh.api = lambda path: {"default_branch": "dev"}
        def incomplete(repo, sha):
            raise Stop("source_tree_incomplete")
        gh.tree = incomplete
        manifest = {"projects": [{"id": i, "commit": NEW} for i in ("codex", "opencode", "opencode-v1")]}
        with patch("scripts.compat_agent.core.request", return_value={"version": "1.0.0"}):
            events = collect(gh, manifest, {})
            self.assertTrue(events)
            self.assertTrue(all(e.get("blocked") == "source_tree_incomplete" for e in events))

    def test_exact_snapshots_include_over_300_changes_and_mode_changes(self):
        gh = FakeGH()
        gh.api = lambda path: {"default_branch": "dev"}
        old = {"src/changed.rs": {"sha": SHA, "mode": "100644", "type": "blob"},
               "src/removed.rs": {"sha": SHA, "mode": "100644", "type": "blob"}}
        new = {"src/changed.rs": {"sha": SHA, "mode": "100755", "type": "blob"}}
        new.update({f"src/new{i}.rs": {"sha": NEW, "mode": "100644", "type": "blob"}
                    for i in range(301)})
        calls = []
        def tree(repo, sha):
            calls.append((repo, sha))
            return old if sha == NEW else new
        gh.tree = tree
        manifest = {"projects": [{"id": i, "commit": NEW} for i in ("codex", "opencode", "opencode-v1")]}
        with patch("scripts.compat_agent.core.request", return_value={"version": "1.0.0"}):
            events = collect(gh, manifest, {})
        self.assertEqual(len(events), 5)
        self.assertEqual(len(calls), 4)  # Cache shared snapshots across tracks.
        for event in events:
            self.assertNotIn("blocked", event)
            self.assertEqual(len(event["files"]), 303)
            kinds = {f["filename"]: f["status"] for f in event["files"]}
            self.assertEqual(kinds["src/changed.rs"], "modified")
            self.assertEqual(kinds["src/removed.rs"], "removed")
            self.assertEqual(kinds["src/new0.rs"], "added")

    def test_tree_rejects_truncation_duplicates_and_invalid_paths(self):
        gh = GitHub("example/router", "synthetic-secret")
        entry = {"path": "src/example.rs", "type": "blob", "mode": "100644", "sha": SHA}
        for data in ({"truncated": True, "tree": []}, {"tree": []},
                     {"truncated": False, "tree": [entry, entry]},
                     {"truncated": False, "tree": [{**entry, "path": "../escape"}]}):
            gh.api = lambda path: {"tree": {"sha": NEW}} if "/git/commits/" in path else data
            with self.subTest(data=data), self.assertRaises(Stop):
                gh.tree("example/upstream", SHA)
        gh.api = lambda path: {"tree": {"sha": NEW}} if "/git/commits/" in path else {
            "truncated": False, "tree": [entry]}
        self.assertEqual(gh.tree("example/upstream", SHA)["src/example.rs"]["sha"], SHA)

    def test_oversized_inventory_blocks_observation(self):
        gh = FakeGH()
        gh.api = lambda path: {"default_branch": "dev"}
        gh.tree = lambda repo, sha: {} if sha == NEW else {
            f"src/new{i}.rs": {"sha": SHA, "mode": "100644", "type": "blob"} for i in range(5001)}
        manifest = {"projects": [{"id": i, "commit": NEW} for i in ("codex", "opencode", "opencode-v1")]}
        with patch("scripts.compat_agent.core.request", return_value={"version": "1.0.0"}):
            events = collect(gh, manifest, {})
        self.assertTrue(all(e.get("blocked") == "change_inventory_too_large" for e in events))


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
        # Synthetic model telemetry must not look like real inference in CI logs.
        logger = patch("builtins.print")
        logger.start()
        self.addCleanup(logger.stop)
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

    def test_analysis_can_finish_after_eight_read_turns(self):
        queue = [self.response("read_files", {"files": []}) for _ in range(9)]
        queue.append(self.response("submit_decision", verdict()))
        model = Model(ROOT, lambda *a: queue.pop(0))
        self.assertEqual(model.run("triage", {}, FakeBox())["decision"], "no_change")
        self.assertEqual(model.calls, 10)

    def test_analysis_budget_remains_bounded(self):
        model = Model(ROOT, lambda *a: self.response("read_file", {}))
        with self.assertRaisesRegex(Stop, "phase_budget_exceeded"):
            model.run("triage", {}, FakeBox())
        self.assertEqual(model.calls, 16)

    def test_global_limits_stop_before_another_request(self):
        for field, value in (("calls", MAX_CALLS), ("tokens", MAX_TOKENS)):
            count = []
            model = Model(ROOT, lambda *a: count.append(1))
            setattr(model, field, value)
            with self.subTest(field=field), self.assertRaisesRegex(Stop, "model_budget_or_unknown_usage"):
                model.run("triage", {}, FakeBox())
            self.assertEqual(count, [])

    def test_native_probe_never_calls_ai_or_writes_state(self):
        from importlib import import_module
        from types import SimpleNamespace
        from unittest.mock import Mock
        module = import_module("scripts.compat_agent.__main__")
        gh = FakeGH()
        box = Mock()
        box.native_check.return_value = {"passed": True, "categories": ["passed"]}
        event = {"track": "codex", "repository": "example/upstream", "before": NEW,
                 "after": SHA, "version": "1.0.0"}
        with patch.dict(os.environ, {"GITHUB_REPOSITORY": gh.repo, "COMPAT_GITHUB_TOKEN": "synthetic"}), \
                patch.object(sys, "argv", ["compat_agent", "--native-probe"]), \
                patch.object(module, "GitHub", return_value=gh), \
                patch.object(module, "command", return_value=SimpleNamespace(stdout=SHA.encode())), \
                patch.object(module, "collect", return_value=[event]), \
                patch.object(module, "Sandbox", return_value=box), \
                patch.object(module, "Model") as model:
            module.main()
        model.assert_not_called()
        box.prepare_native.assert_called_once()
        box.start.assert_called_once()
        box.native_check.assert_called_once()
        box.close.assert_called_once()
        self.assertEqual(gh.writes, [])

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

    def test_pinned_ranges_and_batched_reads_report_truncation(self):
        self.box.gh = FakeGH()
        self.box.allowed_revisions = {("example/upstream", SHA)}
        self.box.gh.file = lambda *a: "\n".join(str(i) for i in range(1, 451))
        args = {"repository": "example/upstream", "sha": SHA, "path": "src/example.rs"}
        first = self.box.dispatch("read_file", args)
        self.assertEqual(first["content"].splitlines(), [str(i) for i in range(1, 201)])
        self.assertTrue(first["truncated"])
        last = self.box.dispatch("read_files", {"files": [{**args, "start_line": 401}]})["files"][0]
        self.assertEqual(last["total_lines"], 450)
        self.assertEqual(last["content"].splitlines()[0], "401")
        self.assertFalse(last["truncated"])
        for update in ({"start_line": 0}, {"line_count": 1001}, {"line_count": True}, {"sha": NEW}):
            with self.subTest(update=update), self.assertRaises(Stop):
                self.box.dispatch("read_file", {**args, **update})
        for files in ([], [args] * 9, ["invalid"]):
            with self.subTest(files=files), self.assertRaises(Stop):
                self.box.dispatch("read_files", {"files": files})

    def test_large_read_ranges_and_batches_are_rejected(self):
        self.box.gh = FakeGH()
        self.box.allowed_revisions = {("example/upstream", SHA)}
        args = {"repository": "example/upstream", "sha": SHA, "path": "src/example.rs"}
        self.box.gh.file = lambda *a: "x" * 64001
        with self.assertRaisesRegex(Stop, "read_range_too_large"):
            self.box.dispatch("read_file", args)
        self.box.gh.file = lambda *a: "x" * 40000
        with self.assertRaisesRegex(Stop, "read_batch_too_large"):
            self.box.dispatch("read_files", {"files": [args] * 8})

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
    def test_native_categories_never_echo_candidate_output(self):
        from types import SimpleNamespace
        for output, category in ((b"client version does not match the reviewed matrix", "version_mismatch"),
                                 (b"Operation not permitted: PRIVATE_PAYLOAD", "permission_denied"),
                                 (b"test result: FAILED PRIVATE_PAYLOAD", "fixture_failed"),
                                 (b"PRIVATE_PAYLOAD", "native_failed_or_filter_missed")):
            self.assertEqual(native_category(SimpleNamespace(returncode=1, stdout=output)), category)
        self.assertEqual(native_category(SimpleNamespace(returncode=0, stdout=b"1 passed")), "passed")
        self.assertNotEqual(native_category(SimpleNamespace(returncode=0, stdout=b"0 passed")), "passed")

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
            with patch.object(box, "native_versions", return_value=[]), \
                    patch("scripts.compat_agent.sandbox.command", return_value=SimpleNamespace(
                     returncode=0, stdout=b"0 passed", stderr=b"")):
                self.assertFalse(box.native_check()["passed"])
            self.assertEqual(manifest.read_text(), "original")

    def test_native_version_startup_failure_is_not_version_mismatch(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as temp:
            box = object.__new__(Sandbox)
            box.clients = Path(temp)
            binary = box.clients / "v1/bin/opencode"
            (box.clients / "current-opencode-v1.json").write_text(json.dumps({"binary": str(binary)}))
            box.event = {"track": "opencode-v1"}
            box.pins = {"projects": [{"id": "opencode-v1", "version": "1.0.0"}]}
            box.name = "synthetic"
            for code, output, category, reported in (
                    (1, b"PRIVATE_ERROR", "version_probe_failed", []),
                    (0, b"1.0.1", "version_mismatch", ["1.0.1"]),
                    (0, b"v1.0.0\nPRIVATE_OUTPUT", "passed", ["1.0.0"])):
                with self.subTest(code=code, output=output), patch(
                        "scripts.compat_agent.sandbox.command", return_value=SimpleNamespace(
                            returncode=code, stdout=output)):
                    result = box.native_versions()[0]
                self.assertEqual(result["category"], category)
                self.assertEqual(result["reported"], reported)
                self.assertNotIn("PRIVATE", json.dumps(result))

    def test_sandbox_home_is_private_writable_tmpfs_not_readonly_root(self):
        from types import SimpleNamespace
        box = object.__new__(Sandbox)
        box.root = ROOT
        box.path = ROOT / "target/synthetic"
        box.clients = ROOT / "target/synthetic-clients"
        box.image = "synthetic-image"
        box.name = "synthetic-container"
        with patch.object(Path, "mkdir"), patch("scripts.compat_agent.sandbox.command",
                return_value=SimpleNamespace(returncode=0)) as run:
            box.start()
        calls = [c.args[0] for c in run.call_args_list]
        self.assertIn(["docker", "exec", box.name, "mkdir", "-m", "700", "-p", "/tmp/compat-home"], calls)
        self.assertIn("ENV HOME=/tmp/compat-home", (ROOT / "scripts/compat_agent/Dockerfile").read_text())


if __name__ == "__main__":
    unittest.main()
