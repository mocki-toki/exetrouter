# Current implementation

Status: Rust implementation, checked on macOS and Raspberry Pi Debian 13.6/aarch64. `exrd` is the service/operator CLI; `exr` is the SSH management CLI and English TUI. All 136 ordinary tests passed on Pi, including TUI and backup/restore. The latest native mock HTTP/WS matrix also passed there; public network deployment remains unverified.

## Implemented

- SQLite schema 6, transactional migrations, WAL, private state files, separate HMAC/OAuth encryption keys and a bounded database worker.
- User-scoped bearer tokens with direct local clipboard delivery, metadata, rotation and revocation. Secrets never appear in CLI/TUI output. Ed25519 public-key registration and revocable restricted SSH identity bindings.
- ChatGPT OAuth device login, encrypted credentials, refresh lease and generation-based compare-and-swap. Terminal refresh failure requires reauthorization.
- Account-visible model catalog, union across active accounts and explicit Codex/OpenCode metadata exports. Conservative intersections for numeric limits and positive capabilities; incompatible protocol fields reject export.
- Responses HTTP JSON/SSE/native WebSocket, owned-socket `previous_response_id`, token revalidation on each generation and no hidden inference replay.
- Native compaction adapter and user-scoped HMAC bindings for opaque checkpoints/reasoning, with a 24-hour sliding lifetime. Original account affinity persists across restart and a new client WS connection.
- Limited Chat Completions with ordered user text/image input, function calls/results, JSON/SSE, refusal, finish reasons and optional usage chunks. Unsupported parameters fail explicitly.
- Durable request accounting before submission; terminal status and nullable counters. Startup recovers unfinished requests as `aborted_unknown`, without retrying inference.
- Quota observation and cooldown, operation health/backoff and authentication rejection ordering. HTTP/WS errors distinguish local resource concurrency from upstream subscription limits.
- Graceful SIGINT/SIGTERM shutdown, accepted database work draining and owned-socket cleanup, within 15 seconds.
- Offline consistent backup, verification and restore to a new private directory, including both keys and committed WAL contents.
- Persistent client configuration, default human-readable tables, opt-in JSON, interactive revoke confirmation and an English dashboard with token management and per-account upstream limits.
- A portable [exr agent skill](../skills/exr/SKILL.md).

No request text, tool result bodies or opaque checkpoints are saved to SQLite. Only fingerprints and operational metadata are persisted. JSON/Chat/compaction may buffer output transiently within bounded request memory; raw Responses streams preserve wire events.

## Resource bounds

| Resource | Current bound |
| --- | --- |
| Database queue | 32 operations |
| HTTP handlers / control connections | 128 / 32 |
| Active generations / client WebSockets, global | 64 / 64 |
| Active generations / client WebSockets, per user | 8 / 8 by default; configurable 1–64 |
| Inference HTTP JSON / incoming client WS message or frame | 16 MiB |
| Upstream WS message/frame / individual SSE event | 1 MiB |
| Control command / reply | 32 KiB / 1 MiB |
| SSE queue / WS continuation IDs | 8 chunks / 1024 IDs per socket |
| Translated output / output items / content parts | 1 MiB / 1024 / 1024 |
| Control IO / gateway exchange / client SSH | 10 / 15 / 45 seconds |
| Upstream connect / idle receive / slow send | 10 / 900 inference (300 metadata) / 10 seconds |

Inference wire-input, upstream WS/SSE event, translated-output and SSE-queue defaults are shared in `src/payload.rs`. Input and output bounds remain independent: accepting a larger image does not expand output buffering. Limits are fixed defaults, not a configurable aggregate memory budget; parsed JSON copies and total in-flight bytes are not measured or separately admitted yet.

Concurrency is shared by all tokens of a user and all generation surfaces. Rejection before inference creates no usage. There is no request-per-minute/IP quota for the current private use case.

## Usage accounting

`requests` counts recorded requests. `known_usage` has both input/output counters; `unknown_usage` lacks at least one; `partial_usage` is the subset with exactly one known counter. `known_usage + unknown_usage = requests`.

Each aggregate sums only known values; if none are known it remains `null`. Confirmed zero remains `0`. Total is the sum of known input/output aggregates when both exist; unknown or partial usage means this is not a confirmed complete consumption figure. Cached input is already included in input and reasoning output in output. Empty periods have `rows=[]`. Requests may specify an IANA time zone for calendar boundaries; the client sends its system time zone, and legacy requests default to `Europe/Moscow`. Reports add hourly day buckets or daily week/month buckets in `timeline`; DST days can contain 23 or 25 hourly buckets. Human output uses local weekday/month dates, and the dashboard plots requests or reported tokens without treating incomplete counters as complete consumption.

## Local verification

```sh
cargo test --locked --all-targets
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo build --locked --bins
```

Client tests cover saved connection settings, explicit JSON, human output, actual quota-duration labels and terminal control-character sanitization. A real pseudo-terminal test opens the dashboard, navigates, issues a synthetic secret, dismisses it, reads limits and checks terminal restoration. Server diagnostics remain authenticated/read-only and redact personal account identifiers.

Ordinary tests use mock upstream or offline state. Native CLI/SDK and real OAuth tests are opt-in, described in [compatibility](compatibility.md) and [live testing](live-testing.md). Short actual-upstream runs succeeded with one/two accounts and `gpt-6.1-sol`; they do not establish production reliability.

## Local service setup

Run in a dedicated private state directory:

```sh
exrd init
exrd admin user-create alice
exrd admin ssh-key-add --user-id 1 --public-key-file /path/to/alice.pub
exrd admin oauth add --device
exrd serve --gateway-uid <GATEWAY_UID>
```

`init` creates `exetrouter.sqlite`, `exetrouter.key`, `exetrouter.oauth.key`, all `0600`, without overwriting existing files. Older state missing the OAuth key uses `admin oauth init-key`, not another `init`. Keys must differ. Global `--db`, `--key`, `--oauth-key`, `--control-socket` precede the subcommand.

The service listens on loopback, default `127.0.0.1:8787`; its Unix socket is `0660`. `--allow-same-uid-for-dev` exists for local testing and must not be used in production. After SIGKILL a stale socket may remain; verify the process is gone before deleting only that socket.

## Remaining work

Actual isolated SSH gateway, TLS/nginx, sustained native sessions/full model context, client reconnection/error matrices, actual external `401`/`429` recovery, load tests, systemd packaging, backup scheduling/encrypted remote storage and usage retention. Separate model quota meters, quota polling/credits and OAuth account removal are not implemented. Public API-key upstream BYOK, HTTP `previous_response_id`, automatic cross-account conversation migration and automatic client/token configuration are outside the current contract.

## Live limits and reset credits

`Limits` augments the local doctor snapshot with issuer-derived account emails and live usage/reset-credit reads. In production, inference uses `/backend-api/codex`; usage and reset-credit operations use `/backend-api/wham`. These paths and `redeem_request_id`/optional `credit_id` match [the current official Codex backend client](https://github.com/openai/codex/blob/main/codex-rs/backend-client/src/client/rate_limit_resets.rs), checked against the downloaded 0.159.3 source. GET replies are bounded; usage failures retain historical observations and explicit refresh errors. Reading limits normally generates no inference; the explicitly requested inactive-weekly-window heuristic may submit one durably guarded minimal gpt-5.6-sol request. Attempts and nullable counters are stored separately from token requests and included in user/model/total accounting; see [the migration plan](quota-activation-migration.md).

`ResetPrepare` reads usage and credits, checks the exact ≤5% remaining boundary and returns an expiring user-bound confirmation with a free-reset recommendation. `ResetConfirm` consumes that handle, reads live state again, checks current account generation and SSH authorization, then submits one idempotent POST. The process serializes reset submissions; neither HTTP transport nor client retries consumption. Unknown responses are reported as uncertain. The earliest-expiring supported credit is selected. Successful resets can clear the matching subscription cooldown only after a fresh allowed-usage read; transport health backoffs remain independent. The account/credit views and confirmations are in memory; credentials and quota observations keep the existing schema.
