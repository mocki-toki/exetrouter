# Implementation reference

The same Rust service powers standalone `exr` and shared `exrd`. This reference covers resource limits and accounting; see [architecture](architecture.md) for components, [server commands](server-cli.md) for setup and [development plan](development-plan.md) for remaining work.

SQLite currently uses schema 9. Migrations are transactional; versions 7, 8 and 9 have separate [account-preference](account-preferences-migration.md), [weekly-activation](quota-activation-migration.md) and [account-routing](account-routing-migration.md) rollback plans. Credentials and bearer keys are independent private files. Startup recovers unfinished requests as `aborted_unknown`.

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
| Graceful shutdown | 15 seconds |
| Upstream connect / idle receive / slow send | 10 / 900 inference (300 metadata) / 10 seconds |
| Native WS pre-response silence / ping interval | 90 / 15 seconds |
| Recoverable upstream WS idle / maximum age | 30 / 300 seconds; retired only between requests |
| Client WS peer silence / application idle | 90 / 300 seconds |

Inference wire-input, upstream WS/SSE event, translated-output and SSE-queue defaults are shared in `src/payload.rs`. Input and output bounds remain independent: accepting a larger image does not expand output buffering. Limits are fixed defaults, not a configurable aggregate memory budget; parsed JSON copies and total in-flight bytes are not measured or separately admitted yet.

Concurrency is shared by all tokens of a user and all generation surfaces. Rejection before inference creates no usage. There is no request-per-minute/IP quota for the current private use case.

Native WS sends upstream and downstream control pings while waiting for a response. Received upstream frames/pongs renew peer liveness; local heartbeats never do. Before the first non-control response event, 90 seconds of complete upstream silence interrupts the request; afterward the existing 900-second receive budget applies. Live pongs preserve slow prefill/reasoning without claiming generation progress. Between requests, client control pings detect silent peers within 90 seconds, while control traffic does not extend the 300-second application-idle deadline. Upstream sockets with a recoverable completed context are retired after 30 idle seconds or five minutes of total age; expiry never interrupts active work. The next request uses the existing same-account context recovery path. Client retry/fallback policy remains responsible for interrupted generations; proactive retirement does not submit inference.

## State recovery

For older state missing the OAuth key, use `exrd admin oauth init-key`; do not run `init` again. After a hard kill, verify the process is gone before deleting a stale control socket. The development-only same-UID option must not be used in production.

## Usage accounting

`requests` counts recorded requests. `known_usage` has both input/output counters; `unknown_usage` lacks at least one; `partial_usage` is the subset with exactly one known counter. `known_usage + unknown_usage = requests`.

Each aggregate sums only known values; if none are known it remains `null`. Confirmed zero remains `0`. Total is the sum of known input/output aggregates when both exist; unknown or partial usage means this is not a confirmed complete consumption figure. Cached input is already included in input and reasoning output in output. Empty periods have `rows=[]`. Requests may specify an IANA time zone for calendar boundaries; the client sends its system time zone, and legacy requests default to `Europe/Moscow`. Reports add hourly day buckets or daily week/month buckets in `timeline`; DST days can contain 23 or 25 hourly buckets. Human output uses local weekday/month dates, and the dashboard plots requests or reported tokens without treating incomplete counters as complete consumption.

## Validation

Use the [standard development checks](../CONTRIBUTING.md) and [opt-in compatibility fixtures](compatibility.md#resolve-and-run-native-fixtures). Ordinary tests use synthetic upstream data and temporary private state.

Client tests cover configuration, JSON/human output, quota labels, terminal sanitization and a real pseudo-terminal dashboard session. Server tests cover authorization, nullable usage, bounded transports, backup/restore and payload privacy. Dated native-client and real-upstream evidence is recorded in [live testing](live-testing.md); it does not establish multi-hour reliability.

## Live limits and reset credits

Inference uses `/backend-api/codex`; live usage/reset-credit operations use `/backend-api/wham`. Bounded metadata reads retain old observations on failure. The [weekly-activation exception](cli.md#weekly-activation) may submit one durably guarded minimal request, accounted separately from API-token requests.

`ResetPrepare` reads live usage/credits and returns an expiring user-bound confirmation only at ≤5% remaining. `ResetConfirm` consumes the handle and rechecks limits, credits, account generation and SSH authorization before one idempotent POST. Submissions are serialized and never retried automatically; uncertain outcomes remain explicit. A successful reset clears matching subscription cooldown only after a fresh allowed-usage read, leaving transport backoff intact. Confirmation state stays in memory. See [user controls](cli.md#reset-credits) and [security controls](security-deployment.md#reset-credit-controls).
