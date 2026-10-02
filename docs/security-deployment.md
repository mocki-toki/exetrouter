# Security and deployment

See the [installation guide](installation.md) and [Linux deployment guide](../deploy/README.md) for the service boundary. Deployment templates must be adapted to the target host.

## Identity and secrets

Router bearers are user-scoped, stored by HMAC and returned once on creation/rotation to an interactive client, which copies them to the clipboard without rendering them. Revocation is checked before each generation, including subsequent WS response.create. Remote users cannot create users, obtain upstream credentials or manage another user's tokens. Shared usage consists of aggregate metadata.

ChatGPT OAuth credentials are XChaCha20-Poly1305 encrypted with account-bound associated data. The 0600 OAuth key is separate from the bearer HMAC key; equal keys are rejected. Refresh uses a lease and generation compare-and-swap; stale refresh cannot overwrite reauth. Terminal refresh failure requires operator login. No upstream API-key BYOK is implemented.

Do not put bearer/OAuth secrets into repository, command arguments, logs or skill files. Client config stores only connection metadata/key path. CLI/TUI issuance copies the bearer to the local system clipboard without rendering it. Clipboard failures never fall back to printing or OSC escapes; the TUI can retry copying its in-memory secret without repeating issuance. Headless clients cannot issue tokens.

## Deployment boundary

- exrd listens only on loopback; a reviewed TLS reverse proxy must preserve WS/SSE behavior without buffering streams.
- Management is restricted SSH plus authenticated Unix peer, not a public HTTP admin endpoint. The gateway can access its socket but not SQLite or keys.
- Run service/gateway under separate unprivileged UIDs. Use service-owned private state and root-controlled binaries/sshd/authorized_keys.
- Keep verified SSH host keys, Ed25519 public registration, per-key forced commands and disabled shell/forwarding/PTY. See [SSH guide](ssh-management.md).
- Resource concurrency/message/queue bounds protect process resources. No request-frequency or IP quotas are imposed.
- Inference is never replayed after possible submission; consuming clients must configure their own retry policy.

## Operations still to implement

The repository includes systemd credential delivery, restricted SSH and TLS proxy templates. Operators must verify certificates, ingress, permissions and service isolation on their host. Backup scheduling, encrypted external retention and a usage retention policy remain operator responsibilities. The current backup includes both keys and is not additionally encrypted. Verify restoration separately; a snapshot's decryptability does not establish current upstream credential validity.

Before publication test actual TLS/WSS/SSE, restricted SSH rejection paths, token/key revocation, startup/shutdown/restart and load on the target architecture. Test full-context/multi-hour sessions and external quota/authentication recovery separately from synthetic health pauses. Logs exclude authorization headers, OAuth codes and request/response bodies; nullable usage must not become invented zero consumption.

This backend's technical compatibility is not an official guarantee of long-term stability or permission to share personal subscriptions. The API Platform and registered ChatGPT token-sharing grant are separate integrations, not interchangeable credentials. See [upstream contract](upstream-contract.md).

## Reset-credit controls

Live usage and credit reads use OAuth over the same authenticated Codex backend boundary. Only SSH-authorized management users can prepare a reset. Server-side eligibility requires ≤5% remaining in a current ordinary subscription window with a future reset; stored percentages alone cannot authorize consumption. User-bound two-minute single-use confirmations and fresh usage/credit/account/SSH checks precede the POST. Upstream receives a UUID idempotency key, and the router never automatically retries a consume request. Unknown outcomes remain explicit. Only supported, unexpired credit detail rows are selected; earlier expirations take priority.

Account emails are display metadata extracted from encrypted issuer-provided credentials, as requested by the operators. Numeric pool IDs and reset confirmation handles are local management metadata; OAuth tokens and upstream credit/account IDs are not exposed. No database schema change is required. Production credits must not be consumed by unattended verification.
