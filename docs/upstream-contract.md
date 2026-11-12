# Upstream contract

ExetRouter uses `https://chatgpt.com/backend-api/codex` with ChatGPT Codex OAuth. OpenAI Platform keys and registered ChatGPT token-sharing grants are separate credentials. Backend compatibility does not guarantee stability or permission to share subscriptions.

Reviewed client revisions live in [protocol-sources.json](protocol-sources.json). The [October 2 audit](native-protocol-audit.md) covers Codex 0.160.0 and OpenCode V2 2.0.22; the earlier October 1 live matrix used 0.159.3/2.0.21. See [compatibility](compatibility.md#verified-versions-and-scope) for scope and [live testing](live-testing.md) for measured evidence. Inspect exact current release sources before changing adapters.

## Protocol

| Operation | Upstream behavior |
| --- | --- |
| OAuth | `https://auth.openai.com`, public client ID `app_EMoamEEZ73f0CkXaXp7hrann`; device usercode/token flow, PKCE `/oauth/token` exchange and refresh-token grant. Credentials use a separate AEAD key. |
| Catalog | `/models?client_version=0.159.3`, selected bearer and `chatgpt-account-id`; 60-second cache and allowlisted metadata. |
| Responses | `/responses` over HTTP SSE or native WS. `store` defaults false; explicit options pass through for backend validation. |
| Native WS | `OpenAI-Beta: responses_websockets=2026-02-06`; sequential `response.create`, including `generate=false` warmup; service ping/pong, bound pre-response peer silence to 90 seconds and retire recoverable idle sockets after 30 seconds/five minutes of age before a new submission on the same account. |
| Compaction | One Responses SSE request ending with `compaction_trigger`; project exactly one opaque checkpoint as `response.compaction`. No separate upstream `/responses/compact`. |
| Chat | Translate supported HTTP JSON/SSE to Responses. Prefer HTTP for large complete requests; otherwise try WS first. Ordinary handshake fallback is allowed only before generation submission. Handshake 401/429 prevents fallback. |
| Accounting | Create a durable event before possible submission; retain terminal counters or an explicit unknown outcome. |
| Images | Unary JSON `/images/generations` and `/images/edits`, direct Codex OAuth; reviewed native subset, separate feature pauses, no Responses emulation. |
| Standalone search | Unary JSON `/alpha/search`, user-scoped session ID and durable account pinning; no migration of backend references. |

Redirects are disabled. Mock upstream requires explicitly enabled literal loopback HTTP. Server-owned headers never forward the client bearer, arbitrary account selection or an arbitrary upstream URL.

Fast (`service_tier: "priority"`) is stripped from transient inference payloads unless permitted by the global/token policy, before transport selection or upstream submission. It is not a quota-reset or account-routing control. See [Fast-mode policy](server-cli.md).

The upstream WS handshake offers RFC 7692 `permessage-deflate`, using the exact transport forks pinned by the reviewed Codex 0.164.0 release. Compression is used only after valid negotiation; a declined extension retains plain continuation frames. Decoded upstream event bounds remain independent of the input/context size. See the [targeted source review](native-protocol-audit.md#targeted-websocket-compression-follow-up--october-26-2026); no inference is replayed to negotiate or recover from a size rejection.

Prepared complete generation JSON of at least 16 MiB selects HTTP SSE before submission, including behind a client-facing native WS. This heuristic avoids the WS whole-message boundary without claiming an exact provider limit or rejecting input. Keep the selected account, scoped identity, Lite mode, full input and durable accounting; never retry a submitted WS message over HTTP. HTTP continuations expand the bounded latest window or return a pre-submission `previous_response_not_found` for client-controlled full input. Native non-generating warmup stays on WS. HTTP status errors remain redacted; quota/auth/health are observed from the actual HTTP response, not reinterpreted from their downstream projection.

Large completed continuation windows are compressed only in transient router memory, preserving the existing 16 MiB retained-window/64 MiB shared budgets while allowing up to 64 MiB decoded recovery. The upstream receives the exact original input and opaque values after normal proof validation. This cache compression is separate from HTTP/WS wire compression and never persists a body, submits extra inference or bypasses account eligibility. Recovery beyond its bounds still requires client-carried full input; full inference admission remains independent.

## Native identity and continuation

Scope `session-id`, `thread-id`, legacy `session_id`, reviewed frame fields and serialized `x-codex-turn-metadata` consistently with user/model HMACs. Cache affinity uses its own digest: child threads may share a cache while retaining distinct identities. Raw client IDs are not forwarded. Preserve tool inventory and other turn fields; reject malformed serialized metadata before submission without echoing values.

`x-codex-turn-state` is an issued continuation value, accepted only after owner/account validation. HTTP returns it as a header; WS exposes handshake metadata after `response.created`. Require authenticated client-carried envelopes without per-output registry writes; reject raw unwrapped state and keep upstream values transient. The [API contract](openai-api.md#native-routing-metadata) defines header projection, binding failures and quota-transfer behavior.

Opaque reasoning, compaction and encrypted function arguments are returned in authenticated router envelopes and restored to their original values before upstream dispatch. Raw values remain unsupported even when legacy ownership digests exist. Authenticated ownership does not expire in the router, including for previously issued envelopes. The [account-pool contract](account-pool.md#opaque-context) defines persistence, bounds and concurrent-fork portability.

## Quotas and cooldown

The [pinned rate-limit parser](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/codex-api/src/rate_limits.rs) defines primary/secondary headers and `codex.rate_limits` events. Store used percentage, reported duration, reset time and observation order. Remaining percentage is `max(100-used, 0)`, not a token/credit estimate. Duration labels use actual minutes; stale or elapsed observations remain historical, and missing values stay unknown.

Only the general Codex percentage meter is implemented. Native images/search additionally retain endpoint-scoped rejection pauses without inventing feature percentages or spending credits; see [native tools and schema 13](native-tools.md). Separate model meters/credits need a verified bucket contract. A 100% observation alone does not prohibit generation because credits may allow it.

HTTP/handshake/wrapped WS 429 or recognized explicit limit errors create cooldown. Expiry uses the latest valid `Retry-After` and corresponding exhausted-window/error reset; without either, use a marked local 60-second backoff. Inspect bounded errors for reset information without persisting or exposing their contents.

Owned continuations may transfer after explicit deactivation, at a quota boundary or to a strictly better economic tier only with complete current context. A soft transfer preserves a healthy original session when no eligible alternative can safely take over; a deactivated account cannot be reused. Explicit pre-generation quota refusal permits bounded fallback. Without an eligible alternative, retain the original quota error. See [selection and recovery bounds](account-pool.md#failure-behavior). Reset-credit POSTs use explicit credit IDs and stable account/weekly-cycle redemption UUIDs under the [automatic reset contract](cli.md#reset-credits); metadata freshness alone never authorizes a mutation. The reset paths, explicit credit selection and four fixed response codes were checked against Codex source revision `970111f6cb62cc02fe36a82ea7d71b37a2889c97` (`backend-client/src/client/rate_limit_resets.rs` and its contract tests). These client sources do not establish an atomic cross-endpoint snapshot or server-side lock.

Observation order and credential generation reject old updates; parallel success does not erase cooldown. A quota-storage failure is logged separately from usage, and unavailable preflight storage rejects new operations. Durable pauses depend on a healthy database.

A dedicated live usage refresh with explicit `rate_limit.allowed: true` and valid general-meter windows may clear an `upstream_reset` cooldown, including after a reset outside the router. It never clears `retry_after`, `local_backoff` or operational health pauses. Recovery and window storage are atomic and require the same active credential generation. Any quota rejection saved for that account after the refresh order was allocated conservatively defers recovery until another refresh, even if the rejection has no windows or leaves the deadline unchanged. Newer stored windows also prevent recovery. Ordinary inference successes still cannot clear cooldown. See [schema 10 to 11](cooldown-recovery-migration.md) for the durable per-account fence and deployment/rollback requirements.

## Operational failures

HTTP read budgets are 900 seconds for inference and 300 seconds for metadata.
On platforms supporting `TCP_USER_TIMEOUT`, the socket's unacknowledged-data
budget matches the read budget instead of reqwest's shorter 30-second default.
TCP keepalive remains enabled; neither it nor downstream heartbeats renew the
application read deadline. A transport failure still never authorizes replay of
an ambiguously submitted inference.

Catalog, refresh and Responses have separate 5–300-second backoff, extended by valid `Retry-After`. Upstream 401 returns a redacted authentication error and schedules refresh for a separate next operation; repeated rejection of refreshed credentials requires reauthorization. Transfers never bypass authentication or operational backoff and always require an enabled replacement account.

Tool execution belongs to clients. OpenCode's shell tool may produce an auxiliary HTTP title request even during a WS session. Completed tool items may run before terminal completion. A lost downstream connection may prevent delivery of a error; reviewed interruption fixtures do not cover every client recovery path.

Image references are forwarded or translated in memory without fetching/decoding. Offline forwarding evidence does not establish live vision support. Full field, error and transport rules are in [openai-api.md](openai-api.md).
