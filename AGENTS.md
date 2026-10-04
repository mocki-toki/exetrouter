# Working on ExetRouter

## Product and scope

ExetRouter routes supported OpenAI-compatible requests to a pool of ChatGPT Codex OAuth accounts. It provides personal standalone use, shared server access and tokens for scripts/BYOK clients. It has a defined compatibility contract, not the complete OpenAI API.

- `exr`: English CLI/TUI, first-run wizard, standalone API and remote SSH client.
- `exrd`: shared API server, restricted SSH gateway and local operator commands.
- Standalone uses the shared implementation; avoid duplicating protocol or routing logic.
- All product UI, help, documentation and bundled skills are in English.

Read the relevant contract before changing behavior: `docs/openai-api.md`, `docs/upstream-contract.md`, `docs/account-pool.md`, `docs/compatibility.md` and `SECURITY.md`. Use `docs/development-plan.md` for remaining work, and verify assumptions against the current implementation.

## Repository map

- `src/bin/`: binary entry points; `src/client/`: config, rendering, clipboard and TUI.
- `src/local.rs`: embedded standalone service; `src/server_config.rs`: server profiles.
- `src/server.rs`, `src/server/`, `src/chat.rs`, `src/upstream.rs`: API and transports.
- `src/oauth.rs`, `src/pool.rs`, `src/catalog.rs`, `src/quota.rs`, `src/reset.rs`: upstream accounts and subscription metadata.
- `src/auth.rs`, `src/control.rs`, `src/store.rs`, `src/migrations/`: authorization, management and storage.
- `src/affinity.rs`, `src/cache.rs`, `src/usage.rs`, `src/health.rs`: routing and accounting.
- `src/backup.rs`, `src/update.rs`, `scripts/`, `deploy/`: backup, installation, updates and deployment.
- `tests/`: offline integration tests and explicitly opt-in live tests.
- `skills/exr/`, `skills/exrd/`: separate operational instructions; `prompts/`: agent installation prompts.

## Invariants

- Never log or persist prompts, messages, generated content, tool arguments/results, request/response bodies, authorization headers or credentials. Errors must not echo arbitrary upstream payloads. Usage records contain model IDs and numeric counters; affinity stores keyed digests rather than content.
- Never replay a possibly accepted inference request. Quota-only fallback requires a handshake refusal or an explicit pre-generation quota rejection with no acceptance/output evidence; ambiguous transport failures and started responses are never replayed.
- Preserve user-scoped context ownership and normal account affinity. Automatic quota failover may change the upstream account only with a complete current context; retain downstream WS/session identity, drop old account-specific turn state and preserve concurrent forks. Never migrate to bypass deactivation, authentication or operational backoff.
- Unknown usage stays unknown, not zero. Keep durable accounting correct on interruption and restart.
- Preserve bounded memory, queues, concurrency and tasks. Resource protection is separate from subscription limits; do not introduce request-frequency/IP quotas without a product requirement.
- Preserve server-side identity resolution, Unix peer UID checks and private file permissions. Never grant the restricted SSH gateway Docker access, broad sudo access or a public HTTP management API.
- OAuth keys, token signing keys, state and user settings must survive upgrades. Do not rotate credentials or overwrite a profile just to install a new version.
- Reset credits require live eligibility verification, at most 5% remaining and explicit confirmation. Recommend waiting when a free reset is less than three days away. Do not automatically retry a credit mutation.

## User experience

Human-readable output is the default; `--json` is the explicit automation interface. Keep terminal charts/decorative bars inside the TUI, not ordinary CLI reports. Dates use the system's local time zone and readable English names; percentages omit unnecessary trailing decimals.

- Put contextual key hints in the bottom Controls area. Navigate left/right across Overview, Usage, Tokens, Models and Settings.
- Use a borderless header: ExetRouter/version on the left, connection on the right, tabs beneath with one-space selection padding.
- Enter opens account/token/Settings actions. Enforce operator locks on the server. Deactivation is reversible and retains OAuth state; reset-credit errors use dismissible dialogs.
- Highlight Settings for client updates; server updates belong to the operator.
- Check clipboard support before token issuance/rotation, then copy secrets without rendering them.

Update checks and ordinary metadata refresh must not submit inference or consume reset credits. The weekly-activation exception permits one minimal `gpt-5.6-sol` request on a freshly verified 100%-remaining weekly window with reset exactly seven days ahead at minute precision. Persist the attempt before submission, suppress repeats for seven days and never replay an uncertain outcome. Respect `EXR_NO_UPDATE_CHECK`; label stale/unknown observations honestly and preserve reported durations.

## Development and validation

Use Rust 1.88 or newer and a C compiler. SQLite is bundled. Prefer small changes in the existing modules and meaningful tests for changed contracts; documentation-only or cosmetic changes do not require new mirrored tests.

Before publishing code changes, run appropriate tests and the standard checks:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 scripts/check-publication.py
```

For installer/update changes also run:

```sh
python3 scripts/test-installer.py
python3 scripts/test-docker-update.py
cargo package --locked --list > target/package-files.txt
python3 scripts/check-publication.py --package-list target/package-files.txt
```

Ordinary tests are offline with synthetic fixtures and temporary private state. Ignored live/deployed tests require explicit user authorization; follow `docs/live-testing.md`. Never exhaust subscriptions deliberately. Resolve current native client versions and inspect their exact sources before making new compatibility claims; do not treat the recorded matrix as permanently current.

## Publication and operations

Keep real domains, personal paths, emails, databases, keys, secrets, private backups and runtime logs out of published files. Use example domains and synthetic identities. Review assets for visible private information and metadata. Extend the publication allowlist deliberately when adding a new public file; do not weaken its credential checks.

Version changes must agree between Cargo.toml, Cargo.lock, changelog and immutable `vMAJOR.MINOR.PATCH` release tags. Native releases are built from tagged source on four CI targets and published with SHA-256 checksums. Never rewrite a published tag or claim a release is ready while its workflow is still running.

The updater preserves the installation method and prefix, validates versions/checksums and replaces binaries atomically. An open dashboard must be reopened afterward. Docker host updates pull a versioned official image (source builds are an explicit alternative), verify a private snapshot and matching schema, replace image/native gateway and validate health; failure restores the previous deployment. Schema changes need a reviewed migration/rollback plan and are not a routine one-click packaging update.

Inspect the actual host configuration before deployment. Preserve operator customizations, SSH boundaries and existing service names. Verify backup/restore and health without sending inference unless authorized. Report what changed, what was checked and any unresolved limits accurately.

`CLAUDE.md` is a symbolic link to this file. Edit this file, not a separate copy.
