# Upstream contract

ExetRouter uses `https://chatgpt.com/backend-api/codex` with ChatGPT Codex OAuth. OpenAI Platform keys and registered ChatGPT token-sharing grants are separate credentials. Backend compatibility does not guarantee stability or permission to share subscriptions.

Reviewed client revisions live in [protocol-sources.json](protocol-sources.json). The [October 2 audit](native-protocol-audit.md) covers Codex 0.160.0 and OpenCode V2 2.0.22; the earlier October 1 live matrix used 0.159.3/2.0.21. See [compatibility](compatibility.md#verified-versions-and-scope) for scope and [live testing](live-testing.md) for measured evidence. Inspect exact current release sources before changing adapters.

## Protocol

| Operation | Upstream behavior |
| --- | --- |
| OAuth | `https://auth.openai.com`, public client ID `app_EMoamEEZ73f0CkXaXp7hrann`; device usercode/token flow, PKCE `/oauth/token` exchange and refresh-token grant. Credentials use a separate AEAD key. |
| Catalog | `/models?client_version=0.159.3`, selected bearer and `chatgpt-account-id`; 60-second cache and allowlisted metadata. |
| Responses | `/responses` over HTTP SSE or native WS. `store` defaults false; explicit options pass through for backend validation. |
| Native WS | `OpenAI-Beta: responses_websockets=2026-02-06`; sequential `response.create`, including `generate=false` warmup; service ping/pong between requests and replace known idle-closed sockets before a new submission on the same account. |
| Compaction | One Responses SSE request ending with `compaction_trigger`; project exactly one opaque checkpoint as `response.compaction`. No separate upstream `/responses/compact`. |
| Chat | Translate supported HTTP JSON/SSE to Responses. Try WS first; ordinary handshake fallback is allowed only before generation submission. Handshake 401/429 prevents fallback. |
| Accounting | Create a durable event before possible submission; retain terminal counters or an explicit unknown outcome. Never replay inference. |

Redirects and inference retries are disabled. Mock upstream requires explicitly enabled literal loopback HTTP. Server-owned headers never forward the client bearer, arbitrary account selection or an arbitrary upstream URL.

## Native identity and continuation

Scope `session-id`, `thread-id`, legacy `session_id`, reviewed frame fields and serialized `x-codex-turn-metadata` consistently with user/model HMACs. Cache affinity uses its own digest: child threads may share a cache while retaining distinct identities. Raw client IDs are not forwarded. Preserve tool inventory and other turn fields; reject malformed serialized metadata before submission without echoing values.

`x-codex-turn-state` is an issued continuation value, accepted only after owner/account validation. HTTP returns it as a header; WS exposes handshake metadata after `response.created`. Persist only a user-scoped digest and keep raw values transient. The [API contract](openai-api.md#native-routing-metadata) defines header projection, binding failures and quota-transfer behavior.

Opaque reasoning, compaction and encrypted function arguments pass unchanged, with domain-separated keyed ownership digests. The [account-pool contract](account-pool.md#opaque-context) defines expiry, bounds and concurrent-fork portability.

## Quotas and cooldown

The [pinned rate-limit parser](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/codex-api/src/rate_limits.rs) defines primary/secondary headers and `codex.rate_limits` events. Store used percentage, reported duration, reset time and observation order. Remaining percentage is `max(100-used, 0)`, not a token/credit estimate. Duration labels use actual minutes; stale or elapsed observations remain historical, and missing values stay unknown.

Only the general Codex meter is implemented. Separate model meters/credits need a verified bucket contract. A 100% observation alone does not prohibit generation because credits may allow it.

HTTP/handshake/wrapped WS 429 or recognized explicit limit errors create cooldown. Expiry uses the latest valid `Retry-After` and corresponding exhausted-window/error reset; without either, use a marked local 60-second backoff. Inspect bounded errors for reset information without persisting or exposing their contents.

Owned continuations may transfer at a quota boundary only with complete current context. Explicit pre-generation quota refusal permits bounded fallback; accepted/ambiguous generations and transport failures never permit replay. Without an eligible alternative, retain the original quota error. See [selection and recovery bounds](account-pool.md#failure-behavior).

Observation order and credential generation reject old updates; parallel success does not erase cooldown. A quota-storage failure is logged separately from usage, and unavailable preflight storage rejects new operations. Durable pauses depend on a healthy database.

## Operational failures

Catalog, refresh and Responses have separate 5–300-second backoff, extended by valid `Retry-After`. Upstream 401 returns a redacted authentication error and schedules refresh for a separate next operation; repeated rejection of refreshed credentials requires reauthorization. Quota transfer never bypasses authentication, deactivation or operational backoff.

Tool execution belongs to clients. OpenCode's shell tool may produce an auxiliary HTTP title request even during a WS session. Completed tool items may run before terminal completion. A lost downstream connection may prevent delivery of a non-retryable error; reviewed interruption fixtures do not cover every client recovery path.

Image references are forwarded or translated in memory without fetching/decoding. Offline forwarding evidence does not establish live vision support. Full field, error and transport rules are in [openai-api.md](openai-api.md).
