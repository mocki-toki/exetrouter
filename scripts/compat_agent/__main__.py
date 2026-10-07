"""Run only from a trusted default-branch checkout. Logs contain fixed statuses."""
import argparse
import difflib
import json
import os
import re
import sys
from pathlib import Path

from .core import GitHub, Ledger, Stop, collect, digest
from .model import Model
from .sandbox import CHECKS, Sandbox, command


def publish(gh, event, base, key, files, report):
    title, body = report.get("title"), report.get("body")
    if not isinstance(title, str) or not 5 <= len(title) <= 150 or "\n" in title:
        raise Stop("invalid_pr_title")
    if not isinstance(body, str) or not 50 <= len(body) <= 24000:
        raise Stop("invalid_pr_body")
    marker = "<!-- exetrouter-compat:" + key + " -->"
    branch = "automation/compat/" + key[:24]
    for pr in gh.prs():
        if pr["head"]["ref"] == branch:
            if marker not in (pr.get("body") or ""):
                raise Stop("pr_identity_conflict")
            return pr["number"]  # Includes human-closed PRs; never reopen/rewrite.
    if gh.resolve(gh.repo, "main") != base:
        raise Stop("target_base_changed")
    tree = []
    for path, content in files.items():
        blob = gh.own("git/blobs", "POST", {"content": content, "encoding": "utf-8"})
        tree.append({"path": path, "mode": "100644", "type": "blob", "sha": blob["sha"]})
    base_tree = gh.own("git/commits/" + base)["tree"]["sha"]
    tree = gh.own("git/trees", "POST", {"base_tree": base_tree, "tree": tree})
    commit = gh.own("git/commits", "POST", {
        "message": title, "tree": tree["sha"], "parents": [base]})
    # Never overwrite an existing branch, even after an uncertain write.
    gh.own("git/refs", "POST", {"ref": "refs/heads/" + branch, "sha": commit["sha"]})
    body += "\n\n" + marker + "\n\nAutomated compatibility proposal. No live upstream tests were run. Full platform CI is pending."
    if event["version"] is None:
        body += "\nThis proposal targets unreleased upstream source."
    pr = gh.own("pulls", "POST", {"title": title, "body": body,
                                  "base": "main", "head": branch, "draft": event["version"] is None})
    return pr["number"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    gh = GitHub(os.environ["GITHUB_REPOSITORY"], os.environ["COMPAT_GITHUB_TOKEN"])
    base = command(["git", "rev-parse", "HEAD"], root).stdout.decode().strip()
    if gh.resolve(gh.repo, "main") != base:
        raise Stop("untrusted_checkout")
    manifest = json.loads((root / "docs/protocol-sources.json").read_text())
    ledger = Ledger(gh)
    events = collect(gh, manifest, ledger.value["cursors"])
    if not events:
        print("compat_agent: no_drift")
        return
    model = Model(root)
    model.preflight()
    policy = digest({p.relative_to(root).as_posix(): p.read_text()
                     for directory in ("prompts/compat-agent", "scripts/compat_agent")
                     for p in sorted((root / directory).glob("*")) if p.is_file()})
    published = [a for a in ledger.value["attempts"].values() if a.get("pr")]
    held = set()
    if published:
        prs = {p["number"]: p for p in gh.prs()}
        for attempt in published:
            pr = prs.get(attempt["pr"])
            if not pr or not pr.get("merged_at"):
                # Queue later source changes while a proposal is open; a human's
                # rejection is sticky until the operator explicitly clears it.
                held.add(attempt.get("repository"))
    # One bounded package per run; later observations remain queued via cursors.
    for event in events:
        if event["repository"] in held:
            continue
        key = digest({"event": event, "policy": policy})
        if key in ledger.value["attempts"]:
            # Stable and branch tracks sharing the same source revision are deduplicated.
            continue
        if any(a["sha"] == event["after"] and a["status"] in {"published", "started", "uncertain", "blocked"}
               for a in ledger.value["attempts"].values()):
            continue
        if not args.dry_run and not ledger.claim(key, event["track"], event["after"], event["repository"]):
            continue
        if event.get("blocked"):
            if not args.dry_run:
                ledger.finish(key, "blocked")
            print("compat_agent: comparison_incomplete_or_diverged")
            continue
        box = Sandbox(root, gh, event, base)
        try:
            contracts = {p: box.local(p) for p in (
                "AGENTS.md", "SECURITY.md", "docs/openai-api.md", "docs/upstream-contract.md",
                "docs/account-pool.md", "docs/compatibility.md")}
            context = {"event": event, "exetrouter_repository": gh.repo, "exetrouter_sha": base,
                       "contracts": contracts}
            triage = model.run("triage", context, box)
            status = triage["decision"]
            if event["version"] is not None and status != "needs_human":
                box.prepare_native()
                box.start()
                native_before = box.native_check()
                context["native_baseline"] = native_before
                if status == "no_change" and not native_before["passed"]:
                    status = "code_change"  # Independent verification must explain the failure.
            if status in {"code_change", "docs_only"}:
                verified = model.run("verify", context, box, triage=triage)
                status = verified["decision"]
                if status == "confirmed":
                    if not box.started:
                        box.start()
                    for name in CHECKS:
                        if not box.check(name)["passed"]:
                            raise Stop("baseline_check_failed")
                    built = model.run("implement", {**context, "confirmed": verified}, box)
                    if built["decision"] != "ready":
                        raise Stop("implementation_needs_human")
                    files = box.bundle()
                    production = any(p.startswith("src/") for p in files)
                    if production and not any(p.startswith("tests/") for p in files):
                        raise Stop("regression_test_missing")
                    if production and not box.red_observed:
                        raise Stop("regression_red_not_demonstrated")
                    for name in CHECKS:
                        if not box.check(name)["passed"]:
                            raise Stop("candidate_check_failed")
                    if event["version"] is not None:
                        if not box.native_check()["passed"]:
                            raise Stop("candidate_native_check_failed")
                        box.results["native"] = True
                    diffs = {p: "".join(difflib.unified_diff(
                        box.gh.file(gh.repo, base, p).splitlines(keepends=True),
                        content.splitlines(keepends=True), fromfile=p, tofile=p))
                        if p in box.inventory else content for p, content in files.items()}
                    reviewed = model.run("review", {**context, "confirmed": verified,
                        "diff": diffs, "checks": box.results, "proposal": built,
                        "red_observed": box.red_observed}, box)
                    if reviewed["decision"] != "approve":
                        raise Stop("review_not_approved")
                    # PR text uses the same credential/endpoint publication rules as code.
                    description = "docs/compat-pr-description.md"
                    if description in files:
                        raise Stop("reserved_description_path")
                    box.dispatch("write_file", {"path": description, "content":
                        str(built.get("title", "")) + "\n" + str(built.get("body", ""))}, True)
                    box.bundle()
                    box.changed.remove(description)
                    if args.dry_run:
                        status = "dry_run_ready"
                    else:
                        number = publish(gh, event, base, key, files, built)
                        ledger.finish(key, "published", number)
                        print("compat_agent: published PR " + str(number))
                        return
            if not args.dry_run:
                ledger.finish(key, status if status in {"no_change", "rejected"} else "blocked")
            print("compat_agent: " + status)
            return
        except Exception:
            if not args.dry_run:
                ledger.finish(key, "uncertain")
            raise
        finally:
            box.close()
    print("compat_agent: previously_attempted")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        # Never render exceptions from dependency/network/candidate code.
        category = str(error) if isinstance(error, Stop) and re.fullmatch(r"[a-z0-9_]+", str(error)) else "internal_error"
        print("compat_agent: " + category, file=sys.stderr)
        sys.exit(1)
