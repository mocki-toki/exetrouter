"""Trusted API, state and policy boundaries. No payload logging."""
import base64
import hashlib
import json
import re
import urllib.error
import urllib.parse
import urllib.request
from pathlib import PurePosixPath

STATE_BRANCH = "automation/upstream-state"
MODEL = "gpt-6.1-sol"
MAX_BYTES = 4 * 1024 * 1024
TRACKS = {
    "codex": ("openai/codex", "@openai/codex", "rust-v"),
    "opencode": ("anomalyco/opencode", "@opencode/cli", "v"),
    "opencode-v1": ("anomalyco/opencode", "opencode-ai", "v"),
}
DECISIONS = {
    "triage": {"no_change", "docs_only", "code_change", "needs_human"},
    "verify": {"confirmed", "rejected", "needs_human"},
    "implement": {"ready", "needs_human"},
    "review": {"approve", "request_changes", "needs_human"},
}


class Stop(RuntimeError):
    """Fixed, content-free failure category."""


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise Stop("redirect_rejected")


def request(url, token=None, method="GET", body=None, limit=MAX_BYTES, timeout=900):
    headers = {"User-Agent": "exetrouter-compat-agent", "Accept": "application/json"}
    if token:
        headers["Authorization"] = "Bearer " + token
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        if len(data) > 16 * 1024 * 1024:
            raise Stop("request_too_large")
        headers["Content-Type"] = "application/json"
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.build_opener(NoRedirect).open(req, timeout=timeout) as resp:
            raw = resp.read(limit + 1)
        if len(raw) > limit:
            raise Stop("response_too_large")
        return json.loads(raw)
    except urllib.error.HTTPError as error:
        # Do not read/echo credential-bearing or arbitrary error payloads.
        raise Stop("http_" + str(error.code)) from None
    except (OSError, ValueError):
        raise Stop("transport_or_json_error") from None


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def source_path(path):
    if not isinstance(path, str) or not path or len(path) > 500:
        raise Stop("invalid_path")
    p = PurePosixPath(path)
    if p.is_absolute() or ".." in p.parts or "\\" in path or str(p) != path:
        raise Stop("invalid_path")
    if any(ord(c) < 32 for c in path):
        raise Stop("invalid_path")
    return path


def writable(path):
    source_path(path)
    return (
        path.startswith(("src/", "tests/", "docs/"))
        and not path.startswith(("src/migrations/", "docs/assets/"))
        and path.endswith((".rs", ".md", ".json"))
        and path not in {"docs/protocol-sources.json", "docs/compat-agent.md"}
    )


def decision(phase, value):
    if not isinstance(value, dict) or value.get("decision") not in DECISIONS[phase]:
        raise Stop("invalid_decision")
    if value.get("coverage_complete") is not True:
        raise Stop("incomplete_coverage")
    if not isinstance(value.get("summary"), str) or len(value["summary"]) > 6000:
        raise Stop("invalid_summary")
    findings = value.get("findings")
    if not isinstance(findings, list) or len(findings) > 20:
        raise Stop("invalid_findings")
    if phase in {"triage", "verify"} and value["decision"] in {
        "docs_only", "code_change", "confirmed"
    } and not findings:
        raise Stop("missing_evidence")
    for finding in findings:
        if not isinstance(finding, dict):
            raise Stop("invalid_finding")
        for field in ("problem", "fix", "test"):
            if not isinstance(finding.get(field), str) or not finding[field]:
                raise Stop("invalid_finding")
        for field in ("upstream", "exetrouter"):
            citations = finding.get(field)
            if not isinstance(citations, list) or not citations:
                raise Stop("missing_citation")
            for cite in citations:
                if not isinstance(cite, dict) or not re.fullmatch(r"[0-9a-f]{40}", cite.get("sha", "")):
                    raise Stop("invalid_citation")
                source_path(cite.get("path"))
                if not isinstance(cite.get("symbol"), str) or not cite["symbol"]:
                    raise Stop("invalid_citation")
    if len(json.dumps(value)) > 64000:
        raise Stop("decision_too_large")
    return value


class GitHub:
    def __init__(self, repo, token):
        if not re.fullmatch(r"[\w.-]+/[\w.-]+", repo):
            raise Stop("invalid_repository")
        self.repo, self.token = repo, token

    def api(self, path, method="GET", body=None):
        return request("https://api.github.com/" + path, self.token, method, body)

    def own(self, path, method="GET", body=None):
        return self.api("repos/" + self.repo + "/" + path, method, body)

    def file(self, repo, sha, path):
        source_path(path)
        if not re.fullmatch(r"[0-9a-f]{40}", sha):
            raise Stop("invalid_revision")
        data = self.api("repos/" + repo + "/contents/" + urllib.parse.quote(path) + "?ref=" + sha)
        if data.get("type") != "file" or data.get("encoding") != "base64":
            raise Stop("unsupported_source_file")
        raw = base64.b64decode(data["content"], validate=False)
        if len(raw) > MAX_BYTES:
            raise Stop("source_too_large")
        return raw.decode("utf-8")

    def resolve(self, repo, ref):
        data = self.api("repos/" + repo + "/commits/" + urllib.parse.quote(ref, safe=""))
        sha = data["sha"]
        if not re.fullmatch(r"[0-9a-f]{40}", sha):
            raise Stop("invalid_revision")
        return sha

    def prs(self):
        result = []
        for page in range(1, 101):
            batch = self.own(f"pulls?state=all&per_page=100&page={page}")
            result.extend(batch)
            if len(batch) < 100:
                return result
        raise Stop("pr_inventory_incomplete")


class Ledger:
    def __init__(self, gh):
        self.gh = gh
        try:
            self.head = gh.own("git/ref/heads/" + STATE_BRANCH)["object"]["sha"]
            self.value = json.loads(gh.file(gh.repo, self.head, "state.json"))
            if self.value.get("schema") != 1:
                raise Stop("unknown_state_schema")
        except Stop as error:
            if str(error) != "http_404":
                raise
            self.head = None
            self.value = {"schema": 1, "cursors": {}, "attempts": {}}

    def save(self):
        # Fast-forward-only ref update provides compare-and-swap via ancestry.
        blob = self.gh.own("git/blobs", "POST", {
            "content": json.dumps(self.value, sort_keys=True), "encoding": "utf-8"})
        tree = self.gh.own("git/trees", "POST", {"tree": [{
            "path": "state.json", "mode": "100644", "type": "blob", "sha": blob["sha"]}]})
        commit = self.gh.own("git/commits", "POST", {
            "message": "Update compatibility agent state", "tree": tree["sha"],
            "parents": [self.head] if self.head else []})
        if self.head:
            self.gh.own("git/refs/heads/" + STATE_BRANCH, "PATCH", {
                "sha": commit["sha"], "force": False})
        else:
            self.gh.own("git/refs", "POST", {
                "ref": "refs/heads/" + STATE_BRANCH, "sha": commit["sha"]})
        self.head = commit["sha"]

    def claim(self, key, track, sha, repository=None):
        if key in self.value["attempts"]:
            return False
        if len(self.value["attempts"]) >= 10000:
            raise Stop("state_capacity_reached")
        self.value["attempts"][key] = {"status": "started", "track": track, "sha": sha,
                                       "repository": repository}
        self.save()
        return True

    def finish(self, key, status, pr=None):
        item = self.value["attempts"][key]
        item["status"] = status
        if pr:
            item["pr"] = pr
        if status in {"no_change", "rejected", "published"}:
            self.value["cursors"][item["track"]] = item["sha"]
        self.save()


def collect(gh, manifest, cursors):
    baseline = {p["id"]: p for p in manifest["projects"]}
    observations = []
    for track, (repo, package, prefix) in TRACKS.items():
        meta = request("https://registry.npmjs.org/" + package + "/latest", timeout=60)
        version = meta["version"]
        if not re.fullmatch(r"\d+\.\d+\.\d+", version):
            raise Stop("invalid_stable_version")
        observations.append((track, repo, gh.resolve(repo, prefix + version), version))
    for repo in dict.fromkeys(p[0] for p in TRACKS.values()):
        branch = gh.api("repos/" + repo)["default_branch"]
        observations.append((repo + ":branch", repo, gh.resolve(repo, branch), None))
    events = []
    for track, repo, sha, version in observations:
        fallback = baseline["codex" if repo == "openai/codex" else "opencode"]["commit"]
        if track in baseline:
            fallback = baseline[track]["commit"]
        before = cursors.get(track, fallback)
        if before == sha:
            continue
        diff = gh.api(f"repos/{repo}/compare/{before}...{sha}?per_page=1")
        files = diff.get("files", [])
        # GitHub compare files are capped at 300, irrespective of pagination.
        # Fail closed instead of allowing a truncated negative classification.
        if diff.get("status") not in {"ahead", "identical"} or len(files) >= 300:
            events.append({"track": track, "repository": repo, "before": before,
                           "after": sha, "version": version,
                           "blocked": "comparison_incomplete_or_diverged"})
            continue
        events.append({"track": track, "repository": repo, "before": before,
                       "after": sha, "version": version,
                       "files": [{k: f[k] for k in ("filename", "previous_filename", "status", "patch") if k in f}
                                 for f in files]})
    return events
