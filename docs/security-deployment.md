# Security and deployment

See the [installation guide](installation.md) and [Linux deployment guide](../deploy/README.md) for the service boundary. Deployment templates must be adapted to the target host.

## Identity and secrets

Router bearers are user-scoped, stored by HMAC and returned once on creation/rotation to an interactive client, which copies them to the clipboard without rendering them. Revocation is checked before each generation, including subsequent WS response.create. Remote users cannot create users, obtain upstream credentials or manage another user's tokens. Shared usage consists of aggregate metadata; account presence additionally exposes pseudonymous client activity without foreign token names, raw token IDs or user identities.

ChatGPT OAuth credentials are XChaCha20-Poly1305 encrypted with account-bound associated data. The 0600 OAuth key is separate from the bearer HMAC key; equal keys are rejected. Refresh uses a lease and generation compare-and-swap; stale refresh cannot overwrite reauth. Terminal refresh failure requires operator login. No upstream API-key BYOK is implemented.

Do not put bearer/OAuth secrets into repository, command arguments, logs or skill files. Client config contains connection metadata and a key path. For clipboard failure/secret handling, see [client controls](cli.md#clipboard-and-uncertain-mutations).

## Deployment boundary

- `exrd` uses loopback on native hosts; Docker publishes its container listener only to host loopback; a reviewed TLS reverse proxy must preserve WS/SSE behavior without buffering streams.
- Management is restricted SSH plus authenticated Unix peer, not a public HTTP admin endpoint. The gateway can access its socket but not SQLite or keys.
- Run service/gateway under separate unprivileged UIDs. Use service-owned private state and root-controlled binaries/sshd/authorized_keys.
- Keep verified SSH host keys, Ed25519 public registration, per-key forced commands and disabled shell/forwarding/PTY. See [SSH guide](ssh-management.md).
- Resource concurrency/message/queue bounds protect process resources. No request-frequency or IP quotas are imposed.

## Host verification and maintenance

The repository includes systemd credential delivery, restricted SSH and TLS proxy templates. Operators must verify certificates, ingress, permissions and service isolation on their host. Backup scheduling, encrypted external retention and a usage retention policy remain operator responsibilities. The current backup includes both keys and is not additionally encrypted. Verify restoration separately; a snapshot's decryptability does not establish current upstream credential validity.

Before exposing a host, verify TLS/WSS/SSE, restricted SSH rejection paths, token/key revocation and startup/shutdown/restart. Load testing remains separate acceptance work. Test full-context/multi-hour sessions and external quota/authentication recovery separately from synthetic health pauses. Logs exclude authorization headers, OAuth codes and request/response bodies; nullable usage must not become invented zero consumption.

This backend's technical compatibility is not an official guarantee of long-term stability or permission to share personal subscriptions. The API Platform and registered ChatGPT token-sharing grant are separate integrations, not interchangeable credentials. See [upstream contract](upstream-contract.md).

## Reset-credit controls

Live usage and credit reads use OAuth over the same authenticated Codex backend boundary. The bounded runtime worker automatically spends only expiring supported credits under the [72/24-hour rules](cli.md#reset-credits), never longer-lived strategic credits. Fresh usage, inventory, generation, activation and operational-health checks follow a randomized delay. An explicit credit ID and host-independent account/weekly-cycle UUID prevent distinct credits being redeemed for the same observed quota cycle when upstream honors idempotency. Persist the digest-only claim before POST; do not retry ambiguous mutations. Every potentially successful result fences further credits until fresh explicitly allowed limits prove recovery and a changed weekly cycle. Durable inventory-change and final-day cohort guards prevent cascades on observed resets. See the contract for independent-host history limits. [Manual redemption](cli.md#interactive-local-reset) requires local private-state access and interactive operator or standalone owner confirmation. It uses the same claim and fences and is rejected at the service boundary.

Account emails are display metadata extracted from encrypted issuer-provided credentials, as requested by the operators. Numeric pool IDs are local management metadata; OAuth tokens and upstream credit/account IDs are not exposed. Schema 12 preserves a durable attempt ledger and sanitized metadata; follow the [reviewed migration](automatic-routing-migration.md). Production credits must not be consumed deliberately for unattended verification. Starting the new runtime enables its documented automatic behavior.
