# Upstream compatibility agent

The hourly GitHub Actions workflow watches Codex and OpenCode default-branch
commits and npm stable release tracks (Codex, OpenCode V2 and V1). It pins source
SHAs and compares complete recursive source trees, including blob IDs and modes.
This is an exact snapshot comparison, not GitHub Compare's capped merge-base
diff: legitimate release-branch divergence does not block analysis. Truncated
trees or oversized inventories stop analysis. The model reads relevant files at
both revisions. A new release alone is not a reason for a patch.

## Decisions and publication

The agent calls the configured HTTPS Responses endpoint with `gpt-6.1-sol`,
`store=false`, complete transient history and no inference retries. Medium does
read-only triage. A fresh xhigh session must first submit its independent verdict
before seeing medium's conclusion. Only confirmed findings reach the xhigh coding
session; a fourth, fresh xhigh session reviews the finished diff.

Only Rust source/tests and textual documentation are writable. Dependencies,
release versions, migrations, installation, deployment, automation and policy
changes require a human. Production fixes require a test-first observed failure.
Formatting, offline clippy/tests and publication checks must pass before a PR.
Candidate execution runs as an unprivileged user in a bounded, credential-free,
networkless container; the server's root-execution prohibition stays intact.
Its temporary dev/test profiles omit debug symbols and incremental caches to fit
the bounded target volume; debug assertions and all named checks stay enabled.
Upstream source/config is never executed or loaded as agent instructions.

Released changes produce normal PRs; default-branch changes produce draft PRs.
The publisher never overwrites branches or reopens closed PRs. Full four-platform
CI runs after publication; no merge, release or deployment is automatic.

## Configuration

Set these repository variables through GitHub Settings:

- `COMPAT_API_BASE_URL`: your router HTTPS URL ending in `/v1`.
- `COMPAT_AGENT_ENABLED`: `false` until configuration and a dry run are reviewed.

Set these repository secrets; never put their values in source or logs:

- `COMPAT_API_TOKEN`: dedicated router bearer issued interactively by its owner.
- Recommended: `COMPAT_APP_PRIVATE_KEY` secret and `COMPAT_APP_ID` variable for
  a GitHub App installed only on this repository, with Contents and Pull requests
  read/write. The workflow mints a short-lived repository-scoped token and revokes
  it afterwards. Configure these through GitHub's UI, not chat.
- Alternatively, `COMPAT_GITHUB_TOKEN`: fine-grained automation token scoped only
  to this repository with the same permissions. Use a dedicated bot identity
  rather than a broad personal token. Its expiry requires renewal.

Both options trigger normal PR CI without the approval requirement of Actions'
built-in `GITHUB_TOKEN`. No workflow-write or approval permission is requested.

Enable the variable and manually dispatch with `dry_run=true` first. This does
consume model inference but does not publish or update durable state. Afterwards
dispatch with `dry_run=false` or leave the hourly schedule enabled. Scheduled
events can be delayed or dropped by GitHub and are not a realtime guarantee.

For native baseline diagnosis, manually dispatch with `native_probe=true`.
This runs one complete pinned release observation against synthetic loopback
mocks, without any AI request, PR publication or durable state change. It logs
only fixed diagnostic categories, not client output or fixture payloads.

## State and failures

`automation/upstream-state` is a metadata-only orphan branch with atomic
fast-forward updates. It records cursors, policy fingerprints, attempt statuses,
source SHA and PR number, not transcripts or model-generated explanations.
The attempt is persisted before inference. Interrupted/uncertain/blocked attempts
are not retried automatically, including after runner termination. Inspect the
attempt and remote branches/PRs before explicitly removing a state entry for a
new attempt. There is no blind retry override. State must not be deleted casually.
An open proposal holds later changes for that upstream repository to avoid
duplicate fixes. A human-closed, unmerged proposal also holds that repository
until the operator explicitly clears its published attempt after review.

The reviewed protocol manifest is deliberately separate from the scan cursor.
The agent does not automatically renew published native compatibility claims.
For stable-release observations it downloads SHA-512-verified exact native client
versions and requires release tags to match the pinned commits. It runs only the
named synthetic HTTP/WS tool/compaction/reopen fixture (or the separate V1 fixture)
against loopback mocks. Compile-time version pins are temporary and never included
in a PR. A failing native baseline escalates even a negative medium verdict to
independent xhigh verification. A released patch must also pass native validation.
No real OAuth/reset/deployment operations are exposed to the model.

Limits: one package per run, 64 model requests, 2,000,000 confirmed input/output
tokens, 16 turns per analysis/verification session, 12 review turns, 24 coding turns, 20 changed files,
512 KiB total proposed source, and a 60-minute workflow timeout. Unknown usage
prevents another inference call. Overflow, missing evidence, unknown capabilities,
failed checks or a moved target base stop publication. No raw session artifacts
or candidate diagnostics are uploaded; logs contain fixed categories, phase/verdict names,
numeric request/token counters, usage-known status and PR IDs. Source reads are
line-bounded and may be batched, up to eight ranges per turn. Truncation is explicit.
The token ceiling counts complete input, including cached tokens, on every call;
it is not an uncached-token cost estimate. Each phase has fresh context, so the
global allowance must cover triage, independent verification, coding and review.

## Development

Run `python3 scripts/test-compat-agent.py` without network or credentials. The
workflow runs trusted default-branch code and exports candidates from committed
HEAD, never from a developer's dirty checkout. Docker is needed only for coding
and validation, not for negative triage. The automation is not the router runtime
and does not change its protocol, accounting or operational policy.
