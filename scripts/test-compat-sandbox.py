#!/usr/bin/env python3
"""Explicit Docker smoke test. No model, GitHub credentials or real upstream."""
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scripts.compat_agent.sandbox import CHECKS, Sandbox, command


class OfflineGitHub:
    repo = "example/router"


root = Path(__file__).resolve().parent.parent
base = command(["git", "rev-parse", "HEAD"], root).stdout.decode().strip()
box = Sandbox(root, OfflineGitHub(), {
    "repository": "example/upstream", "before": base, "after": base, "version": None}, base)
try:
    box.start()
    for name in CHECKS:
        result = box.check(name)
        if not result["passed"]:
            # Smoke test uses trusted source and synthetic fixtures, no sessions/secrets.
            print(result["diagnostics"], file=sys.stderr)
            raise SystemExit("Sandbox smoke check failed: " + name)
    probe = command(["docker", "exec", box.name, "sh", "-c",
                     "test \"$(id -u)\" != 0 && test ! -e /work/.git && test ! -w /work/Cargo.toml && test -w /work/target && test -w \"$HOME\" && test \"$(stat -c %a \"$HOME\")\" = 700"])
    if probe.returncode:
        raise SystemExit("Sandbox filesystem boundary failed")
    print("Credential-free sandbox checks passed")
finally:
    box.close()
