#!/usr/bin/env python3
"""Audit publishable source without scanning/printing private local runtime state."""
import argparse
import ipaddress
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import urlsplit

ROOT_FILES = {"AGENTS.md", "CLAUDE.md", "Cargo.toml", "Cargo.lock", "README.md", "LICENSE", "SECURITY.md", "CONTRIBUTING.md", "CHANGELOG.md", ".gitignore", ".dockerignore", "flake.nix", "flake.lock"}
SOURCE_DIRS = {"src", "tests", "docs", "deploy", "scripts", "skills", "prompts", ".github", "Formula"}
SECRET_PATTERNS = [
    re.compile(r"\bexr_tok_[0-9a-f]{16}_[0-9a-f]{64}\b"),
    re.compile(r"\beyJ[A-Za-z0-9_-]{15,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\b"),
    re.compile(r"-----BEGIN (?:OPENSSH |RSA |EC |DSA )?PRIVATE KEY-----"),
    re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,}|sk-[A-Za-z0-9_-]{24,})\b"),
    re.compile(r"/(?:Users|home)/(?!runner(?:/|\b)|routercli(?:/|\b)|exetrouter(?:/|\b))[A-Za-z0-9_.-]+/"),
]
URL = re.compile(r"https?://[^\s\"'`<>\\)]+")
HOSTS = {"github.com", "api.github.com", "codeload.github.com", "raw.githubusercontent.com", "registry.npmjs.org", "opencode.ai", "chatgpt.com", "rustup.rs", "sh.rustup.rs", "www.rust-lang.org", "docs.github.com", "docs.docker.com", "www.star-history.com", "api.star-history.com"}


def allowed_path(name):
    parts = Path(name).parts
    if not parts or parts[0] not in ROOT_FILES | SOURCE_DIRS:
        return False
    return not any(p in {"target", ".local-state", ".git", "node_modules", "__pycache__"} for p in parts) and not (
        name.endswith((".sqlite", ".sqlite-shm", ".sqlite-wal", ".db", ".key", ".pem", ".p12", ".pfx", ".log", ".pyc"))
        or Path(name).name.startswith(".env")
    )


def allowed_host(host):
    if not host or any(c in host for c in "{}$<>"):
        return True  # Source interpolation, not a concrete deployment.
    if host in HOSTS or host in {"localhost", "example.com", "example.org", "example.net"}:
        return True
    if host.endswith((".openai.com", ".rust-lang.org", ".githubusercontent.com", ".example.com", ".example.org", ".example.net", ".example", ".test", ".invalid")):
        return True
    try:
        ip = ipaddress.ip_address(host)
        return ip.is_loopback or ip.is_unspecified
    except ValueError:
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    parser.add_argument("--package-list", type=Path)
    args = parser.parse_args()
    root = args.root.resolve()
    errors = []
    if args.package_list:
        for name in args.package_list.read_text().splitlines():
            if name in {"Cargo.toml.orig", ".cargo_vcs_info.json"}:
                continue
            if not allowed_path(name):
                errors.append((name, "unexpected packaged file"))
    else:
        if (root / ".git").exists():
            names = subprocess.check_output(["git", "ls-files", "-z"], cwd=root).decode().split("\0")
            paths = [root / name for name in names if name]
        else:
            paths = []
            for entry in ROOT_FILES | SOURCE_DIRS:
                candidate = root / entry
                if candidate.is_file():
                    paths.append(candidate)
                elif candidate.is_dir():
                    paths.extend(path for path in candidate.rglob("*") if path.is_file())
        for path in paths:
            name = str(path.relative_to(root))
            if not allowed_path(name):
                errors.append((name, "unexpected publication path"))
                continue
            if name == "docs/assets/tui-overview.jpg":
                if not path.read_bytes().startswith(b"\xff\xd8\xff") or path.stat().st_size > 5 * 1024 * 1024:
                    errors.append((name, "invalid reviewed screenshot"))
                continue
            try:
                text = path.read_text()
            except (OSError, UnicodeError):
                errors.append((name, "non-text or unreadable source file"))
                continue
            if any(pattern.search(text) for pattern in SECRET_PATTERNS):
                errors.append((name, "credential or private workstation path"))
            for match in URL.finditer(text):
                try:
                    host = urlsplit(match.group().rstrip(".,;:")).hostname
                except ValueError:
                    continue  # Deliberately malformed URL fixtures.
                if not allowed_host(host):
                    errors.append((name, "unreviewed concrete endpoint"))
                    break
    for name, rule in errors:
        print(f"{name}: {rule}", file=sys.stderr)  # Never echo matched content.
    if errors:
        return 1
    print("Publication audit passed: source/package paths and endpoint/credential rules")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
