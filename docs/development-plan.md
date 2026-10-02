# Development continuation plan

Current milestone: the core local service and human-facing client UX are implemented. This document records the current sequence; completed prototype plans are superseded by working code and tests.

## Completed foundations

1. User/token authentication, restricted SSH control, SQLite schema/migrations and usage reports.
2. Encrypted OAuth device login/refresh, account-visible models and latest native-client protocol adapters.
3. Responses HTTP/WS, limited Chat Completions and SDK contracts, streaming/error/shutdown behavior without replay.
4. Resource concurrency, durable subscription quota/cooldown, account pool selection and operational health.
5. User-scoped opaque context bindings across HTTP/new WS/restart, model metadata and cache affinity.
6. Actual one-account OAuth/protocol/native tests, automatic native compaction and cache observations.
7. Actual two-account routing, preserved WS/checkpoint affinity around a synthetic local pause and native clients on the pool.
8. Consistent online SQLite backup with keys, offline verification and restore to fresh state.
9. Readable CLI output, persistent SSH configuration, English TUI, per-account quota windows and separate exr/exrd skills.
10. Standalone embedded API, first-run wizard, self-contained Settings and clipboard-only token issuance.
11. Live subscription metadata refresh, confirmed reset credits and user-facing model descriptors.
12. Modular Docker deployment, installation prompts and in-place client/server updates with backup/health rollback.
13. Client version in the dashboard header and a highlighted Settings update indicator.
14. Bounded automatic quota account failover for full-context HTTP/compact and sequential native WS, including incremental tool-result recovery, user-scoped portability digests and concurrent forks.

Short actual runs are evidence for specific scenarios, not proof of full backend reliability. The current HTTP/WS tool and two-compaction/restart matrix, checked on 2026-10-02, is Codex `0.160.0` and OpenCode V2 `2.0.22`; resolve registry `latest` and inspect exact sources before any new compatibility release. The broader October 1 protocol/SDK evidence retains its recorded versions.

## Release and deployment

The repository includes a checksum-verifying installer, source installation, four native Linux/macOS release targets, CI, MIT licensing, an agent skill and prompts for client/server installation. Deployment templates separate the service UID, SSH gateway UID, private state and public TLS ingress. Opt-in deployed tests obtain their target from environment variables and are excluded from ordinary CI.

## Native protocol audit follow-up

The [October 2 source audit](native-protocol-audit.md) is the implementation checklist. Current mock fixtures use reviewed Codex 0.160.0 and OpenCode V2 2.0.22 from [protocol-sources.json](protocol-sources.json); the earlier live/compaction matrix is not silently renewed.

Implemented from the audit:

1. Bounded authenticated native `zstd` decoding, explicit coding errors and malformed/truncated/expansion rejection before inference; trace-level process privacy fixture includes compressed content.
2. Separate scoped cache, session and thread identities, including shared-cache child threads, token rotation, user isolation, HTTP/WS and restart. Serialized native turn metadata now carries the same scoped session/thread as transport headers and WS frame metadata while preserving tool inventory.
3. Current native early/partial/missing-terminal WS submission counts, wrapped terminal failures, WS heartbeats and cross-transport turn-state ownership. A lost downstream connection still prevents reliable delivery of a non-retryable error.
4. Explicit unsupported public WS/state errors, Lite negotiation, bounded native response metadata and custom/namespace/phase/multimodal/interleaved reasoning fixtures.
5. Opt-in two-compaction/restart native harnesses for isolated state and the existing HTTPS deployment, with retained canary checks and a 36-request budget. The deployed four-case matrix passed with actual compaction observed separately in each cycle. The deployment fixture also has a bounded large-context mode and an optional idle interval of up to two hours.

Validation on 2026-10-02: 206 ordinary tests, standard static/publication checks, four actual native mock tool cycles, two HTTP rejection cases and six WS interruption cases passed. Authorized current-client real tool cycles and two observed compaction cycles passed over public HTTPS/WS for all four client/transport cases, including five native process invocations and a verified router restart per case. A separate Codex WS case reached 217,156 observed input tokens, compacted and recalled the original canary through a local tool. Exact counts, the earlier failed WS case and limits are in [live-testing.md](live-testing.md#measured-deployment-continuity-2026-10-02).

Remaining acceptance: full advertised model windows, multi-hour sessions, saturation and actual external authentication/quota recovery. The idle harness has not yet been run for a multi-hour interval. Passing bounded public-ingress cases does not establish these guarantees or prove the private-backend cause of the earlier stall.

Before updating the matrix, run `python3 scripts/check-protocol-sources.py --fetch --check-current`, inspect release differences, update the reviewed manifest deliberately and rerun isolated native mock tests. This script only reads public source/metadata; real tests remain opt-in under [live-testing.md](live-testing.md).

## Next: client/server compatibility and diagnostics

Display both client and remote server versions, report server update availability separately and negotiate management protocol/capabilities. A newer client must hide unsupported actions or explain exactly why they are unavailable; matching package versions must not be an artificial requirement. Add fixtures for old client/new server and new client/old server.

Add an explicit, opt-in network doctor mode for public TLS, HTTP and WebSocket readiness. Keep stored configuration readiness distinct from connectivity and inference checks. Do not consume inference quota in ordinary diagnostics.

## Next: sustained sessions and failure behavior

Exercise full-context native compaction, multi-hour sessions, reconnect and server restart with checkpoints. Test timeouts, slow clients, pool saturation and concurrent services. Observe actual upstream authentication/quota failures when available, without deliberately exhausting a subscription or replaying inference.

Acceptance: normal account affinity and quota-only continuation transfer, explicit recovery instructions when an owned WS cannot resume, bounded memory/tasks/queues, correct unknown usage and no duplicated submission. Synthetic pause tests remain clearly separated from external failure evidence.

## Next: operations

Add backup scheduling and encrypted off-host retention, verify a restore rehearsal on each target host, and implement a usage retention policy. Define safe migration and rollback procedures beyond the current schema-change refusal in automatic Docker updates.

Acceptance: operator can onboard a user, register a public key, add/reauthorize an OAuth account, recover state and diagnose stale quotas without exposing credentials. Backup validity must not be confused with current upstream credential validity.

## Next: release distribution

Offer ready-to-pull versioned multi-architecture Docker images so server updates do not require an on-host Rust build. Broaden prebuilt Linux portability beyond the current client glibc requirement, and add publisher provenance/signature verification beyond SHA-256 integrity checks. Verify installation and update paths on clean supported systems.

## Optional extensions

Separate model meters/credits need a verified upstream contract. Ordinary subscription limits use authenticated metadata reads; Overview polls them every 30 seconds. OAuth removal, additional Chat parameters and wider client recovery require their own tests. Request-frequency/IP limits are not planned for the small trusted deployments. Automatic installation of bearer secrets or modification of client configuration is not part of the product contract.

## Quota failover validation, 2026-10-02

Current Codex 0.160.0 and OpenCode V2 2.0.22 each completed HTTP and WS tool continuations across a synthetic pre-generation quota refusal. Six native interrupted-stream cases still submitted exactly one generation with no HTTP fallback. Direct HTTP and classic WS compaction/tool-result continuation from A to B retained hidden random canaries. The new router also passed six actual HTTP/WS requests across isolated synthetic quota boundaries, with known durable usage and A/A/B/A/B/B ownership. Live WS encrypted-function-argument portability, actual subscription exhaustion and multi-hour concurrent failover remain acceptance work; subscriptions were not deliberately exhausted. The source drift check still matches both native clients.
