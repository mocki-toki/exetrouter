# Selected upstream contract

ExetRouter uses `https://chatgpt.com/backend-api/codex`, as implemented in the pinned native clients. It is separate from API Platform `api.openai.com` and from the registered ChatGPT token-sharing grant. The router does not accept upstream Platform API keys or promise backend stability/shared-subscription authorization.

Pinned on 2026-10-01: Codex `0.159.3`, commit `01fc69f4026735edfdf6789820549727a4867b11`; OpenCode V2 `2.0.21`, commit `8a8bd622a3d7dc29ccf30ec17f84e363ed95ed72`. Resolve latest versions and inspect exact source before updating adapters.

The [October 2 protocol audit](native-protocol-audit.md) reviews Codex 0.160.0 and OpenCode V2 2.0.22 at pinned source revisions. Its current native mock matrix is separate from the October 1 full live/compaction evidence above. The [source manifest](protocol-sources.json) and `scripts/check-protocol-sources.py` provide a deliberate release-drift gate.

The audit follow-up passed current-client real HTTP/WS tool cycles and explicit compressed HTTP. Its later deployed matrix passed two observed compact/resume cycles, process reopening and verified router restart for Codex 0.160.0 and OpenCode V2 2.0.22 over public HTTPS/WS. A separate Codex WS case reached 217,156 observed input tokens and retained its canary after compaction. Handshake response metadata is emitted after `response.created` for OpenCode compatibility. These bounded results renew the current native compaction matrix, not every October 1 protocol/SDK scenario or multi-hour reliability; [live-testing.md](live-testing.md#measured-deployment-continuity-2026-10-02) records measured counts and remaining limits.

## Protocol

- OAuth issuer: https://auth.openai.com, public client ID app_EMoamEEZ73f0CkXaXp7hrann. Device usercode/token endpoints followed by PKCE /oauth/token exchange; refresh_token grant for refresh. Credentials are encrypted under a separate AEAD key.
- Catalog: /models?client_version=0.159.3 with upstream bearer and chatgpt-account-id; visible models, 60-second cache and metadata allowlist.
- Responses: /responses, store=false, upstream HTTP SSE or native WS. Native WS uses OpenAI-Beta: responses_websockets=2026-02-06 and sequential response.create.
- Compaction: one Responses SSE request with a final compaction_trigger; projected response.compaction requires exactly one opaque checkpoint. A separate upstream /responses/compact is not used.
- Chat: supported incoming HTTP JSON/SSE is translated to Responses; upstream WS first, ordinary handshake fallback only before generation submission.
- Usage: one durable event before possible submission; terminal counters or explicit unknown outcome. No automatic inference replay.

Server-owned headers never forward the client bearer, arbitrary account ID or arbitrary upstream URL. Native `x-codex-turn-state` is the explicit exception for continuation routing: return the bounded upstream value, persist only a user-scoped keyed digest, and forward it only after ownership/account validation. Redirects/inference retries are disabled. Mock upstream is restricted to explicitly enabled literal loopback HTTP.

Native Codex 0.160.0 sends both `session-id` and `thread-id`. HTTP and WS upstream requests scope the native session and thread hints independently with user/model HMACs, along with the legacy `session_id` alias. The cache key retains its existing digest and account preference; child threads can share that key while keeping distinct session/thread identities. Reviewed per-frame identity fields are scoped as well. Raw client thread identifiers are not forwarded. These headers preserve native request compatibility; they do not guarantee completion of a stalled upstream stream.

The client's serialized `client_metadata["x-codex-turn-metadata"]` also carries its own session/thread identities. These reviewed fields receive the same scoped values as the frame and transport headers, including the fixed identity on subsequent WS frames. Other fields, particularly tool namespace inventory and compaction flags, remain intact. Malformed serialized metadata or identity fields fail before inference; the router does not print their values. The exact [native metadata source](https://github.com/openai/codex/blob/a956835d020762cb2b570053af06f643a11c0ecc/codex-rs/core/src/responses_metadata.rs) is included in the drift manifest.

## Quotas and cooldown

The [pinned Codex rate-limit parser](https://github.com/openai/codex/blob/01fc69f4026735edfdf6789820549727a4867b11/codex-rs/codex-api/src/rate_limits.rs) defines primary/secondary headers and codex.rate_limits events. Store used_percent, window_minutes and reset_at with observation time/order. Remaining percentage is max(100-used,0); it is not an estimate of tokens or credits.

Duration is always reported, not assumed from the primary/secondary label. Client UX calls 480 minutes 8-hour, 10080 minutes Weekly, otherwise the actual duration. A past reset becomes reset_elapsed; stale observations retain historical percentages. Missing data remains unknown.

Only the general codex meter is implemented. Separate model meters/credits need a verified model/bucket contract. A 100% observation alone does not block generation: credits may permit it. HTTP/handshake/wrapped WS 429 or recognized explicit limit errors create cooldown. Its expiry is the latest valid Retry-After and corresponding exhausted-window/error reset; absent these, a marked local 60-second backoff applies.

Owned continuations can change account at a quota boundary with full current context. An open downstream WS reconstructs its latest completed incremental window before replacing the upstream socket. Explicit pre-generation quota refusals permit bounded fallback; each refused attempt retains unknown accounting. Started or ambiguous generations, generic errors and transport failures never permit replay. Without an eligible alternative, the original quota refusal/cooldown remains visible. See [account pool](account-pool.md) for recovery bounds and concurrent-fork ownership.

Observation ordering and credential generations reject older updates; parallel success does not erase cooldown. Bounded error bodies are inspected for reset, never persisted or exposed. A quota storage failure is logged independently from usage; unavailable preflight storage rejects a new operation. Durable pause guarantees require a healthy database.

## Operations and clients

Catalog/refresh/Responses temporary failures have separate 5–300 second backoff, extended by Retry-After. Upstream 401 is redacted, makes access refresh-due for a new operation, and repeated rejection of refreshed credentials requires reauth. Handshake 401/429 prevents Chat fallback. Checkpoints/open WS keep normal affinity; quota-only transfer does not bypass authentication or operational backoff.

Codex warmup generate=false and current tool/control fields are retained. Chat user image URLs/data URLs are translated to ordered Responses input_image parts with optional detail; the router does not fetch or decode them. Offline forwarding fixtures do not establish live vision or model-specific detail eligibility. OpenCode's command tool is shell; a WS tool cycle may still include an auxiliary HTTP title request. Native tests isolate profiles/auth and disable supported retry paths. Mock 503 tests confirmed one primary submission. Native Codex WS interruption tests confirm one submitted generation for early close, partial output and missing terminal; the router sends a wrapped terminal 400 for these ambiguous failures. A completed tool item may execute before the response terminal. A downstream disconnect that prevents delivery of the error remains a client recovery limitation.

Real device login/refresh/catalog, protocol/tool/compaction/Chat and latest native HTTP/WS cycles passed. Two-account runs confirmed selection and preserved continuity around a synthetic local pause. Actual external 401/429 recovery, full context and multi-hour sessions still need further verification. Deployment templates require per-host checks. See [compatibility](compatibility.md), [live tests](live-testing.md), [pool](account-pool.md) and [API contract](openai-api.md).

Opaque account affinity includes both `encrypted_content` and the string array `encrypted_function_args` on native function calls. Only separately domain-scoped keyed digests are retained; the values pass upstream unchanged. Empty argument arrays are valid. Unknown or foreign input fails before submission. Conflicting untransferred owners are rejected; quota transfers add derived portability bindings without storing context or extending input lifetime.
