#!/usr/bin/env python3
"""Verify reviewed public source files and optionally detect newer releases.

No inference, credentials, client execution or installation. Downloaded source
stays in ignored target/. A newer version requires review, never automatic trust.
"""
import argparse
import concurrent.futures
import hashlib
import json
import re
import subprocess
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MAX_FILE_BYTES = 2 * 1024 * 1024


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "exetrouter-protocol-audit"})
    with urllib.request.urlopen(request, timeout=30) as response:
        data = response.read(MAX_FILE_BYTES + 1)
    if len(data) > MAX_FILE_BYTES:
        raise ValueError("source or metadata exceeds the download bound")
    return data


def current(project):
    repository = project["repository"]
    package = project.get("npm_package")
    if package:
        version = json.loads(fetch(f"https://registry.npmjs.org/{package}/latest"))["version"]
        tag = ("rust-v" if project["id"] == "codex" else "v") + version
    else:
        tag = json.loads(fetch(f"https://api.github.com/repos/{repository}/releases/latest"))["tag_name"]
        version = tag.removeprefix("v")
    if not re.fullmatch(r"(?:rust-)?v\d+\.\d+\.\d+", tag):
        raise ValueError("expected a stable release tag")
    ref = f"refs/tags/{tag}"
    output = subprocess.run(
        ["git", "ls-remote", f"https://github.com/{repository}.git", ref, ref + "^{}"],
        capture_output=True, text=True, check=True, timeout=45,
    ).stdout
    refs = dict(line.split()[::-1] for line in output.splitlines())
    commit = refs.get(ref + "^{}", refs.get(ref))
    if not commit or not re.fullmatch(r"[a-f0-9]{40}", commit):
        raise ValueError("release tag did not resolve to a commit")
    return version, commit


def check(project, directory, download, check_current):
    name, repository, commit = project["id"], project["repository"], project["commit"]
    if not re.fullmatch(r"[a-z0-9-]+", name) or not re.fullmatch(r"[a-f0-9]{40}", commit):
        raise ValueError("invalid source identity")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid source repository")
    base = directory / name / commit
    for source in project["files"]:
        relative = Path(source["path"])
        if relative.is_absolute() or ".." in relative.parts:
            raise ValueError("invalid source path")
        path = base / relative
        if not path.resolve().is_relative_to(directory.resolve()) or path.is_symlink():
            raise ValueError("source cache escapes its directory")
        if download:
            data = fetch(f"https://raw.githubusercontent.com/{repository}/{commit}/{relative.as_posix()}")
        else:
            data = path.read_bytes()
        if hashlib.sha256(data).hexdigest() != source["sha256"]:
            raise ValueError("reviewed source hash differs")
        if download:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    result = {"project": name, "reviewed_version": project["version"], "source_files": "verified"}
    if check_current:
        version, resolved = current(project)
        result.update(current_version=version, revision_matches=resolved == commit and version == project["version"])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetch", action="store_true", help="download and verify pinned source files")
    parser.add_argument("--check-current", action="store_true", help="check release drift using public metadata")
    args = parser.parse_args()
    manifest = json.loads((ROOT / "docs/protocol-sources.json").read_text())
    directory = ROOT / "target/protocol-sources"
    failed = False
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        jobs = [(project, pool.submit(check, project, directory, args.fetch, args.check_current))
                for project in manifest["projects"]]
        for project, job in jobs:
            try:
                result = job.result()
                failed |= result.get("revision_matches") is False
            except Exception as error:
                result = {"project": project["id"], "error_class": type(error).__name__}
                failed = True
            print(json.dumps(result), flush=True)
    return int(failed)


if __name__ == "__main__":
    raise SystemExit(main())
