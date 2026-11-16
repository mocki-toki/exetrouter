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
| Inference HTTP JSON / incoming client WS message or frame | No router-defined byte ceiling; provider limits and host memory still apply |
| Incoming upstream WS message/frame / individual SSE event | 1 MiB |
| Outgoing upstream WS | Negotiated permessage-deflate; plain fallback uses 1 MiB continuation frames |
| Control command / reply | 32 KiB / 8 MiB (including 90-day numeric histories) |
| SSE queue / WS continuation IDs | 8 chunks / 1024 IDs per socket |
| Translated output / output items / content parts | 1 MiB / 1024 / 1024 |
| Control IO / gateway exchange / client SSH | 10 / 15 / 45 seconds |
| Graceful shutdown | 15 seconds |
| Upstream connect / idle receive / slow send | 10 / 900 inference (300 metadata) / 10 seconds |
| Native WS pre-response silence / ping interval | 90 / 15 seconds |
| Recoverable upstream WS idle / maximum age | 30 / 300 seconds; retired only between requests |
| Client WS peer silence / application idle | 90 / 300 seconds |
| Current WS recovery representation / global / decoded recovery | 16 / 64 / 64 MiB; 16,384 items; large windows compressed in memory |

Upstream WS framing/event, translated-output and SSE-queue defaults are shared in `src/payload.rs`. Input acceptance and output bounds remain independent: accepting a larger image does not expand output buffering. There is no router-defined inference request-size ceiling or aggregate input-memory budget; parsed JSON copies and total in-flight bytes are not measured or separately admitted. Concurrency, decoder slots/deadlines and retained recovery/output bounds remain independent resource protections, not model token quotas.

The upstream-only WebSocket transport uses the exact compression-capable forks pinned by Codex 0.164.0; Axum and synthetic downstream clients retain the crates.io implementation. Compressed writes use the library's message encoder, not raw continuation frames, which would bypass compression. One send is flushed before another message is admitted; its transient encoder/write buffers scale with input. A declined extension uses uncompressed fragmentation. Both wire frames and decoded incoming events retain the 1 MiB bound. A fixed `upstream_websocket_transport` event records only the negotiated compression boolean.

Prepared complete generation JSON reaching 16 MiB selects upstream HTTP SSE before submission, not rejection or a post-failure retry. Native WS and HTTP/SSE share the same account, observation/accounting machinery, bounded SSE decoder and event forwarding. The native loop owns the HTTP response directly, with no bridge worker or replay queue. Client controls, cancellation and shutdown remain serviced while awaiting HTTP headers/stream events; HTTP uses the 900-second inference read budget rather than WS pre-response silence. Headers are projected after `response.created`. HTTP incremental requests require bounded full-window expansion; unavailable recovery returns local `previous_response_not_found` without a usage row. `generate=false` stays native WS. This does not claim a provider HTTP/WS limit.

Completed recovery input/output above 16 MiB uses zstd level 1 in transient memory, bounded to 16 MiB encoded storage per window and the same 64 MiB global budget. Smaller windows retain parsed input with serialized-byte accounting. Compression charges encoded buffer capacity and never materializes an additional uncompressed serialization; decoding verifies the measured length before parsing, with a 64 MiB output and 8 MiB history bound. Work/input count remain bounded, with no detached codec task. The latest window can therefore reconstruct successive large HTTP tool turns without repeated client recovery. Optional recovery failure never imposes a full-request admission ceiling.

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

One bounded runtime worker refreshes live account metadata independently of the dashboard and applies the [automatic expiry rules](cli.md#reset-credits). Metadata reads themselves never redeem a credit. A randomized delay precedes a second live preflight; the selected explicit credit and host-independent idempotency key stay stable. SQLite claims the digest before POST; submitted/unknown attempts survive restart and fence another credit until verified recovery. Mutations are never automatically retried. A successful reset releases only matching subscription cooldown through a fresh allowed-usage read, retaining transport backoff and observation-order fences. Legacy `ResetPrepare`/`ResetConfirm` are decoded solely for explicit rejection. See [security controls](security-deployment.md#reset-credit-controls) and [migration](automatic-routing-migration.md).
