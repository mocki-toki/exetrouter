"""A credential-free, networkless container for candidate execution."""
import os
import json
import subprocess
import selectors
import tempfile
import time
from pathlib import Path

from .core import MAX_BYTES, Stop, source_path, writable

CHECKS = {
    "fmt": ["cargo", "fmt", "--check"],
    "clippy": ["cargo", "clippy", "--offline", "--locked", "--all-targets", "--", "-D", "warnings"],
    "tests": ["cargo", "test", "--offline", "--locked"],
    "publication": ["python3", "scripts/check-publication.py"],
}


def command(args, cwd=None, timeout=1800):
    process = None
    try:
        process = subprocess.Popen(args, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                   env={k: v for k, v in os.environ.items() if k in {
                                       "PATH", "HOME", "TMPDIR", "DOCKER_HOST", "DOCKER_CONFIG"}})
        output = bytearray()
        deadline = time.monotonic() + timeout
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while selector.get_map():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise Stop("command_failed_or_timed_out")
                for key, _ in selector.select(min(remaining, 1)):
                    chunk = os.read(key.fd, min(65536, MAX_BYTES + 1 - len(output)))
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    output.extend(chunk)
                    if len(output) > MAX_BYTES:
                        raise Stop("command_output_too_large")
            code = process.wait(timeout=max(0.01, deadline - time.monotonic()))
        return subprocess.CompletedProcess(args, code, bytes(output), b"")
    except (OSError, subprocess.TimeoutExpired):
        raise Stop("command_failed_or_timed_out") from None
    finally:
        if process is not None:
            if process.poll() is None:
                process.kill()
                process.wait()
            process.stdout.close()


class Sandbox:
    def __init__(self, root, gh, event, base):
        self.root, self.gh, self.event, self.base = root, gh, event, base
        self.allowed_revisions = {(gh.repo, base), (event["repository"], event["before"]),
                                  (event["repository"], event["after"])}
        self.temp = tempfile.TemporaryDirectory()
        self.path = Path(self.temp.name) / "candidate"
        self.path.mkdir()
        self.image = "exetrouter-compat-sandbox:local"
        self.name = "exetrouter-compat-" + self.path.parent.name.lower().replace("_", "-")
        self.changed = set()
        self.results = {}
        self.red_observed = False
        self.started = False
        self.clients = Path(self.temp.name) / "clients"
        self.clients.mkdir()
        self.pins = None
        self.inventory = command(["git", "ls-tree", "-r", "--name-only", base], root).stdout.decode().splitlines()
        for path in self.inventory:
            source_path(path)
            if path == ".dockerignore":
                # The product image intentionally omits tests/docs. This isolated
                # trusted source export needs them; it contains no local state.
                continue
            # Never copy local changes, runtime state, .git or credentials.
            content = command(["git", "show", base + ":" + path], root)
            if content.returncode:
                raise Stop("source_export_failed")
            out = self.path / path
            out.parent.mkdir(parents=True, exist_ok=True)
            if path == "CLAUDE.md":
                out.symlink_to("AGENTS.md")
            else:
                out.write_bytes(content.stdout)

    def start(self):
        # Build only the trusted baseline; download dependencies before candidate edits.
        dockerfile = self.root / "scripts/compat_agent/Dockerfile"
        result = command(["docker", "build", "-f", str(dockerfile), "-t", self.image, str(self.path)])
        if result.returncode:
            raise Stop("sandbox_build_failed")
        (self.path / "target").mkdir(exist_ok=True)
        result = command(["docker", "run", "-d", "--name", self.name, "--network", "none",
                          "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
                          "--pids-limit", "512", "--memory", "6g", "--cpus", "2",
                          "--read-only", "--tmpfs", "/tmp:rw,exec,size=128m",
                          "--mount", "type=bind,src=" + str(self.path) + ",dst=/work,readonly",
                          "--mount", "type=bind,src=" + str(self.clients) + ",dst=/clients,readonly",
                          "--tmpfs", "/cache:rw,exec,size=1g,uid=10001,gid=10001,mode=0700",
                          "--tmpfs", "/work/target:rw,exec,size=3g,uid=10001,gid=10001,mode=0700",
                          self.image, "sleep", "infinity"])
        if result.returncode:
            raise Stop("sandbox_start_failed")
        self.started = True
        # Wait for cache initialization synchronously; detached entrypoint copying
        # would race the first offline Cargo invocation.
        result = command(["docker", "exec", self.name, "cp", "-R", "/usr/local/cargo/.", "/cache/"])
        if result.returncode:
            raise Stop("sandbox_cache_initialization_failed")

    def prepare_native(self):
        if self.event["version"] is None:
            return
        self.pins = json.loads(self.local("docs/protocol-sources.json"))
        for project in self.pins["projects"]:
            if project["id"] == self.event["track"]:
                project.update(version=self.event["version"], commit=self.event["after"])
        pin_file = Path(self.temp.name) / "native-pins.json"
        pin_file.write_text(json.dumps(self.pins))
        args = ["python3", str(self.root / "scripts/fetch-test-clients.py"),
                "--manifest", str(pin_file), "--output-dir", str(self.clients)]
        if self.event["track"] == "opencode-v1":
            args.append("--opencode-v1")
        result = command(args)
        if result.returncode:
            raise Stop("pinned_native_download_failed")

    def native_check(self):
        if self.pins is None or not self.started:
            raise Stop("native_check_not_prepared")
        manifest = self.path / "docs/protocol-sources.json"
        original = manifest.read_bytes()
        try:
            # Test-only compile-time pins, never included in the proposed PR.
            manifest.write_text(json.dumps(self.pins))
            env = []
            if self.event["track"] == "opencode-v1":
                record = json.loads((self.clients / "current-opencode-v1.json").read_text())
                binary = "/clients/" + str(Path(record["binary"]).relative_to(self.clients))
                env += ["-e", "EXETROUTER_OPENCODE_V1_BIN=" + binary]
                tests = ["opencode_v1_http_compatibility_probe"]
            else:
                records = json.loads((self.clients / "current-clients.json").read_text())
                for client in ("codex", "opencode"):
                    binary = "/clients/" + str(Path(records[client]["binary"]).relative_to(self.clients))
                    env += ["-e", "EXETROUTER_" + client.upper() + "_BIN=" + binary]
                tests = ["current_clients_complete_tool_cycles_over_http_and_websocket"]
            outputs = []
            passed = True
            for test in tests:
                result = command(["docker", "exec"] + env + ["-w", "/work", self.name,
                    "cargo", "test", "--offline", "--locked", "--test", "responses", test,
                    "--", "--ignored", "--exact"])
                # A misspelled filter must never masquerade as a successful fixture.
                passed = passed and result.returncode == 0 and b"1 passed" in result.stdout
                outputs.append((result.stdout + result.stderr).decode(errors="replace")[-12000:])
            return {"passed": passed, "diagnostics": outputs}
        finally:
            manifest.write_bytes(original)

    def close(self):
        if self.started:
            command(["docker", "rm", "-f", self.name], timeout=60)
        self.temp.cleanup()

    def local(self, path):
        source_path(path)
        file = self.path / path
        if file.is_symlink() or not file.resolve().is_relative_to(self.path.resolve()):
            raise Stop("symlink_rejected")
        if file.stat().st_size > MAX_BYTES:
            raise Stop("source_too_large")
        return file.read_text()

    def dispatch(self, name, args, editable=False):
        if not isinstance(args, dict):
            raise Stop("invalid_tool_arguments")
        if name == "read_files":
            files = args.get("files")
            if not isinstance(files, list) or not 1 <= len(files) <= 8:
                raise Stop("invalid_read_batch")
            ranges = [self.dispatch("read_file", item) for item in files]
            if len(json.dumps(ranges).encode()) > 256000:
                raise Stop("read_batch_too_large")
            return {"files": ranges}
        if name == "read_file":
            if (args.get("repository"), args.get("sha")) not in self.allowed_revisions:
                raise Stop("revision_outside_evidence")
            start, count = args.get("start_line", 1), args.get("line_count", 200)
            if type(start) is not int or start < 1 or type(count) is not int or not 1 <= count <= 1000:
                raise Stop("invalid_read_range")
            if args["repository"] == self.gh.repo:
                content = self.local(args["path"])
            else:
                content = self.gh.file(args["repository"], args["sha"], args["path"])
            lines = content.splitlines()
            selected = "\n".join(lines[start - 1:start - 1 + count])
            if len(selected.encode()) > 64000:
                raise Stop("read_range_too_large")
            return {"repository": args["repository"], "sha": args["sha"], "path": args["path"],
                    "start_line": start, "total_lines": len(lines), "content": selected,
                    "truncated": start + count - 1 < len(lines)}
        if name == "search_code":
            query = args.get("query")
            if not isinstance(query, str) or not query or len(query) > 200:
                raise Stop("invalid_query")
            matches = []
            for path in self.inventory:
                if not path.endswith((".rs", ".md", ".json")) or path == "CLAUDE.md":
                    continue
                for number, line in enumerate(self.local(path).splitlines(), 1):
                    if query in line:
                        matches.append({"path": path, "line": number, "text": line[:1000]})
                        if len(matches) == 100:
                            return {"matches": matches, "truncated": True}
            return {"matches": matches, "truncated": False}
        if name == "write_file" and editable:
            path, content = args.get("path"), args.get("content")
            if not writable(path) or not isinstance(content, str) or len(content.encode()) > 256000:
                raise Stop("patch_policy_rejected")
            file = self.path / path
            if file.is_symlink() or not file.resolve().is_relative_to(self.path.resolve()):
                raise Stop("symlink_rejected")
            if len(self.changed | {path}) > 20:
                raise Stop("patch_too_large")
            file.parent.mkdir(parents=True, exist_ok=True)
            file.write_text(content)
            self.changed.add(path)
            self.results.clear()  # Checks for an earlier candidate cannot authorize this one.
            return {"written": True}
        if name == "run_check" and editable:
            return self.check(args.get("name"))
        raise Stop("tool_not_allowed")

    def check(self, name):
        if name not in CHECKS or not self.started:
            raise Stop("check_not_allowed")
        result = command(["docker", "exec", "-w", "/work", self.name] + CHECKS[name])
        self.results[name] = result.returncode == 0
        if (name == "tests" and result.returncode != 0 and b"test result: FAILED" in result.stdout
                and self.changed and all(p.startswith("tests/") for p in self.changed)):
            self.red_observed = True
        return {"passed": result.returncode == 0,
                "diagnostics": (result.stdout + result.stderr).decode(errors="replace")[-24000:]}

    def validate_citations(self, report):
        for finding in report["findings"]:
            for field in ("upstream", "exetrouter"):
                repo = self.event["repository"] if field == "upstream" else self.gh.repo
                for cite in finding[field]:
                    if (repo, cite["sha"]) not in self.allowed_revisions:
                        raise Stop("citation_outside_evidence")
                    text = self.gh.file(repo, cite["sha"], cite["path"])
                    if cite["symbol"] not in text:
                        raise Stop("citation_not_found")

    def bundle(self):
        files = {p: self.local(p) for p in sorted(self.changed)}
        if not files or sum(len(v.encode()) for v in files.values()) > 512000:
            raise Stop("empty_or_large_patch")
        # Apply the existing publication scanner to ALL proposed new/changed files,
        # not just the baseline git inventory. Scan messages separately as well.
        from importlib.util import module_from_spec, spec_from_file_location
        spec = spec_from_file_location("publication", self.root / "scripts/check-publication.py")
        module = module_from_spec(spec)
        spec.loader.exec_module(module)
        for content in files.values():
            if any(p.search(content) for p in module.SECRET_PATTERNS):
                raise Stop("publication_secret_rejected")
            for match in module.URL.finditer(content):
                from urllib.parse import urlsplit
                if not module.allowed_host(urlsplit(match.group().rstrip(".,;:")).hostname):
                    raise Stop("publication_endpoint_rejected")
        return files
