#!/usr/bin/env python3
"""Resolve current stable clients and download verified binaries, without installation."""
import argparse
import base64
import hashlib
import io
import json
import platform
import re
import subprocess
import tarfile
import urllib.request
from pathlib import Path


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "exetrouter-compatibility"})
    with urllib.request.urlopen(request, timeout=120) as response:
        raw = response.read(512 * 1024 * 1024 + 1)
    if len(raw) > 512 * 1024 * 1024:
        raise RuntimeError("Client download exceeds the size bound")
    return raw


def metadata(url):
    return json.loads(fetch(url))


def commit(repository, tag):
    prefix = f"refs/tags/{tag}"
    output = subprocess.run(
        ["git", "ls-remote", f"https://github.com/{repository}.git", prefix, prefix + "^{}"],
        check=True, capture_output=True, text=True, timeout=45,
    ).stdout
    refs = dict(line.split()[::-1] for line in output.splitlines())
    sha = refs.get(prefix + "^{}", refs.get(prefix))
    if not sha or not re.fullmatch(r"[a-f0-9]{40}", sha):
        raise RuntimeError(f"{repository} stable tag does not resolve to a commit")
    return sha


def binary(raw, directory, filename):
    directory.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        matches = [entry for entry in archive if entry.isfile() and Path(entry.name).name == filename]
        if len(matches) != 1:
            raise RuntimeError(f"Expected exactly one {filename} in distribution")
        path = directory / filename
        path.write_bytes(archive.extractfile(matches[0]).read())
        path.chmod(0o755)
    return str(path.resolve())


def codex_distribution(raw, directory):
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        entries = [entry for entry in archive if entry.isfile()]
        executables = [entry for entry in entries if entry.name.endswith("/bin/codex")]
        if len(executables) != 1:
            raise RuntimeError("Expected exactly one Codex runtime package")
        prefix = executables[0].name.removesuffix("bin/codex")
        members = {entry.name[len(prefix):]: entry for entry in entries if entry.name.startswith(prefix)}
        if not {"bin/codex", "bin/codex-code-mode-host", "codex-package.json"} <= members.keys():
            raise RuntimeError("Codex distribution lacks required runtime helpers")
        for relative, entry in members.items():
            parts = Path(relative).parts
            if not parts or Path(relative).is_absolute() or ".." in parts:
                raise RuntimeError("Unsafe Codex distribution member")
            path = directory.joinpath(*parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.extractfile(entry).read())
            path.chmod(0o755 if entry.mode & 0o111 else 0o644)
    return str((directory / "bin" / "codex").resolve())


def sources(repository, commit, directory):
    raw = fetch(f"https://codeload.github.com/{repository}/tar.gz/{commit}")
    with tarfile.open(fileobj=io.BytesIO(raw)) as archive:
        for entry in archive:
            parts = Path(entry.name).parts[1:]
            if not entry.isfile() or not parts or ".." in parts:
                continue
            path = directory.joinpath(*parts)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(archive.extractfile(entry).read())


def fetch_v1(args):
    version = selected_version(args, "opencode-v1", "opencode-ai")
    if not re.fullmatch(r"1\.\d+\.\d+", version):
        raise RuntimeError("Expected a stable OpenCode V1 release")
    revision = commit("anomalyco/opencode", f"v{version}")
    verify_revision(args, "opencode-v1", revision)
    result = {"version": version, "commit": revision}
    if args.resolve_only:
        print(json.dumps(result, indent=2))
        return
    os_name = {"Darwin": "darwin", "Linux": "linux"}.get(platform.system())
    architecture = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x64", "AMD64": "x64"}.get(platform.machine())
    if not os_name or not architecture:
        raise RuntimeError("This test downloader supports macOS/Linux on arm64/x64")
    package = metadata(f"https://registry.npmjs.org/opencode-{os_name}-{architecture}/{version}")
    if package["version"] != version:
        raise RuntimeError("Registry returned a different distribution version")
    raw = fetch(package["dist"]["tarball"])
    integrity = "sha512-" + base64.b64encode(hashlib.sha512(raw).digest()).decode()
    if integrity != package["dist"]["integrity"]:
        raise RuntimeError("OpenCode V1 distribution integrity mismatch")
    root = args.output_dir or Path(__file__).resolve().parent.parent / "target" / "compat"
    directory = root / f"opencode-v1-{version}"
    result["binary"] = binary(raw, directory / "bin", "opencode")
    result["integrity"] = integrity
    if args.sources:
        sources("anomalyco/opencode", revision, directory / "source")
    (root / "current-opencode-v1.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


def selected_version(args, client, package):
    if args.manifest:
        return args.pins[client]["version"]
    return metadata(f"https://registry.npmjs.org/{package}")["dist-tags"]["latest"]


def verify_revision(args, client, revision):
    if args.manifest and args.pins[client]["commit"] != revision:
        raise RuntimeError("Release tag differs from the pinned manifest commit")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--resolve-only", action="store_true")
    parser.add_argument("--sources", action="store_true")
    parser.add_argument("--opencode-v1", action="store_true", help="Resolve/download the separate opencode-ai V1 client only")
    parser.add_argument("--manifest", type=Path, help="Pin versions and require exact release-tag commits")
    parser.add_argument("--output-dir", type=Path, help="Isolated runtime destination")
    args = parser.parse_args()
    args.pins = {p["id"]: p for p in json.loads(args.manifest.read_text())["projects"]} if args.manifest else {}
    if args.opencode_v1:
        fetch_v1(args)
        return
    codex = selected_version(args, "codex", "@openai/codex")
    opencode = selected_version(args, "opencode", "@opencode/cli")
    if any(not re.fullmatch(r"\d+\.\d+\.\d+", version) for version in (codex, opencode)):
        raise RuntimeError("The client matrix requires stable releases, not prereleases")
    codex_commit = commit("openai/codex", f"rust-v{codex}")
    opencode_commit = commit("anomalyco/opencode", f"v{opencode}")
    verify_revision(args, "codex", codex_commit)
    verify_revision(args, "opencode", opencode_commit)
    result = {"codex": {"version": codex, "commit": codex_commit}, "opencode": {"version": opencode, "commit": opencode_commit}}
    if args.resolve_only:
        print(json.dumps(result, indent=2))
        return
    os_name = {"Darwin": "darwin", "Linux": "linux"}.get(platform.system())
    architecture = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "x64", "AMD64": "x64"}.get(platform.machine())
    if not os_name or not architecture:
        raise RuntimeError("This test downloader supports macOS/Linux on arm64/x64")
    root = args.output_dir or Path(__file__).resolve().parent.parent / "target" / "compat"
    for client, package_name, version, filename in (
        ("codex", "@openai/codex", f"{codex}-{os_name}-{architecture}", "codex"),
        ("opencode", f"@opencode/cli-{os_name}-{architecture}", opencode, "opencode"),
    ):
        package = metadata(f"https://registry.npmjs.org/{package_name}/{version}")
        if package["version"] != version:
            raise RuntimeError("Registry returned a different distribution version")
        raw = fetch(package["dist"]["tarball"])
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(raw).digest()).decode()
        if integrity != package["dist"]["integrity"]:
            raise RuntimeError(f"{client} distribution integrity mismatch")
        result[client]["integrity"] = integrity
        directory = root / f"{client}-{result[client]['version']}"
        result[client]["binary"] = codex_distribution(raw, directory) if client == "codex" else binary(raw, directory / "bin", filename)
    if args.sources:
        sources("openai/codex", codex_commit, root / f"codex-{codex}" / "source")
        sources("anomalyco/opencode", opencode_commit, root / f"opencode-{opencode}" / "source")
    root.mkdir(parents=True, exist_ok=True)
    (root / "current-clients.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
