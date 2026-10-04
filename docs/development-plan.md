# Development plan

The core service, standalone/remote dashboard, OAuth account pool, usage accounting, backup/restore and installers are implemented. Current contracts are documented in [architecture](architecture.md), [API](openai-api.md), [account pool](account-pool.md) and [installation](installation.md). This plan tracks remaining work.

## Client/server compatibility and diagnostics

- Show both client and remote server versions; report server updates separately.
- Negotiate management capabilities. Hide unsupported actions or explain why they are unavailable; do not require identical package versions.
- Test old-client/new-server and new-client/old-server combinations.
- Add an opt-in network Doctor for public TLS, HTTP and WS readiness. Keep stored configuration checks separate from connectivity and inference; ordinary diagnostics must not consume inference quota.

## Sustained sessions and failure behavior

The reviewed Codex 0.160.0/OpenCode V2 2.0.22 public HTTP/WS matrix passed two observed compaction cycles, process reopening and router restart. A separate Codex WS case reached 217,156 observed input tokens. Synthetic quota-switch continuations also passed. Counts, failed probes and limits are in [live testing](live-testing.md); source-level follow-up is in the [protocol audit](native-protocol-audit.md).

Remaining acceptance:

- Full advertised context windows, large custom-tool output, multi-hour sessions and pool saturation.
- Timeouts, slow clients, reconnects, concurrent services and unavailable WS recovery windows.
- Actual upstream authentication/quota failures when available, without deliberately exhausting a subscription.
- Live WS encrypted-function-argument portability and multi-hour/concurrent failover.

Require bounded memory/tasks/queues, correct unknown usage, normal affinity, authorized quota-only transfers and no duplicated submission. Give explicit recovery instructions when an owned WS cannot resume. The optional two-hour idle harness is implemented but has not been run for that duration.

Before updating compatibility claims, inspect exact current release sources and run the drift check:

```sh
python3 scripts/check-protocol-sources.py --fetch --check-current
```

Update the manifest deliberately, rerun isolated mock fixtures and keep real tests separately authorized under [live-testing.md](live-testing.md). The broader October 1 protocol/SDK matrix retains its recorded versions.

## Operations

Add backup scheduling, encrypted off-host retention and usage retention. Rehearse restore on each target host. Define migration/rollback procedures beyond automatic Docker updates' schema-change refusal.

Acceptance: operators can onboard users, manage public keys/OAuth accounts, recover state and diagnose stale quotas without exposing credentials. A valid backup does not prove current upstream credential validity.

## Distribution

Versioned AMD64/ARM64 GHCR images and four native Linux/macOS targets are available through release automation. Verify install/update paths on clean supported systems, broaden Linux binary portability beyond the current glibc requirement, and add publisher provenance/signature verification beyond SHA-256 integrity.

## Optional extensions

Separate model quota meters/credits need a verified upstream contract. OAuth removal and wider client recovery need their own tests. Overview already polls subscription metadata every 30 seconds. Request-frequency/IP limits are not planned for small trusted deployments. Automatic bearer installation or modification of another client's configuration remains outside the product contract.
