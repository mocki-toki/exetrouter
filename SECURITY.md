# Security and privacy

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/mocki-toki/exetrouter/security/advisories/new). Do not put tokens, private keys, user prompts or raw request/response captures in a public issue. If private reporting is unavailable, open an issue containing only a request for a private contact method.

## What the server retains

| Data | Retention |
| --- | --- |
| Prompts, system instructions, messages and model text | Never logged or written by the router |
| Tool definitions, arguments/results and attachments | Forwarded in memory; never retained as payloads |
| Bearer/authorization headers | Never logged; router bearer verification uses keyed hashes |
| OAuth tokens | Encrypted with account-bound AEAD under a separate private key |
| Personal metadata | Client-encrypted token names and dashboard preferences; user key never sent to service. Retired encrypted account labels may remain in older state |
| Usage | User/token IDs, validated model ID, numeric usage, fixed statuses, timings and request/response identifiers |
| Conversation affinity | Client-carried authenticated ownership envelopes and encrypted digest-only transfer receipts; legacy digest overrides remain readable without new rows. Unwrapped client context is rejected. Bounded current WS history exists only in transient memory |
| Subscription limits | Percentages, durations, reset/observation times, bounded seven-day hourly numeric history, cooldowns, sanitized credit expiry/count metadata and digest-only automatic-redemption audit outcomes; upstream credit IDs remain transient |
| Native tools | Fixed account/feature pause metadata and bounded user-scoped search-session HMAC/account bindings; no image bytes, prompts, raw session IDs, queries, reference strings or search results |
| MCP | No persistent protocol sessions or payloads; authenticated user-scoped session correlations and bounded in-flight cancellation digests/signals in memory only. Same router bearer, account controls and accounting as native operations |
| Account identity | Operator-owned account metadata and display email extracted from encrypted issuer credentials |

### Logs

Operational logs contain fixed event names, server-generated correlation IDs, numeric statuses/counts and timing. Unavailable models use the fixed label `unavailable`; unvalidated model input, upstream error messages, header values and arbitrary JSON keys are never saved.

Client WebSocket diagnostics correlate upgrade receipt, session opening/closure, text-frame receipt and generation admission with a generated connection ID and numeric frame sequence. They record frame byte counts and preparation/write/first-event timings, not frame contents. Native upstream request start, successful socket write, first event and interruption share generated request/connection IDs. A successful write does not prove upstream acceptance. Session closure reasons are fixed categories; `session_ended` covers exits without a more specific observed client-side cause.

WebSocket liveness diagnostics use numeric ping/pong counts, monotonic last-frame age and a boolean response-started flag. Proactive upstream retirement records fixed idle/age expiry reasons and runs only between requests with a recoverable completed context. Peer pongs renew transport liveness without proving request acceptance or generation progress; router-generated heartbeats do not renew upstream receive deadlines.

Stream diagnostics retain only the last allowlisted upstream event type, its Unix receipt timestamp in milliseconds and monotonic age. Unknown types become `other`; downstream heartbeats do not refresh these times. The executable fixes allowed log targets/levels, so `RUST_LOG=trace` cannot enable dependency body traces.

Upstream WebSocket compression is negotiated with the server-owned transport, never by forwarding caller extension headers. Incoming compressed frames and decoded messages retain independent 1 MiB bounds. Outgoing compression buffers scale with accepted inference input; there is no router-defined input byte ceiling. The fixed transport diagnostic records only whether `permessage-deflate` was negotiated, never extension payloads or request contents.

Pre-submission HTTP selection for large complete generations does not change the client's WS authorization or account/context ownership. No new replay worker, payload persistence or unbounded recovery cache is introduced. An unrecoverable incremental HTTP continuation is refused locally before inference/accounting so the client can resend full current context. HTTP status bodies are parsed within the existing bounded error policy and never echoed; actual headers/status drive quota and health independently of redacted client projection. Disconnect or shutdown drops the in-flight HTTP future/response, retaining unknown accounting unless terminal counters were observed.

Large latest-window recovery is compressed only in transient memory, never in state files, snapshots or logs. Encoded storage remains bounded to 16 MiB per window and 64 MiB globally; decoded recovery is separately bounded to 64 MiB with an 8 MiB codec history limit. The router measures the original size itself and verifies decoded length before parsing. This does not add an input admission ceiling, retain older branches or replay inference. Full context and opaque tool/reasoning values are restored exactly; existing bearer, ownership, account and eligibility checks still govern every continuation.

WebSocket interruption diagnostics include a fixed reason, stage and failing boundary (`upstream`, `client` or `router`), elapsed time, a fixed transport-error category and an optional numeric close code. The boundary identifies the failing operation, not an inferred network or provider root cause. They distinguish upstream closure/read/write failures, invalid events, client closure/concurrent frames, storage stages and service shutdown. Close-reason text, underlying error messages and frame contents are never logged. An interrupted request with no observed events still has an unknown outcome. Native WS has one `upstream_stream_interrupted` summary correlating generated client/request/upstream connection IDs, frame sequence, connection age and the number of finished requests; this replaces the separate `websocket_request_interrupted` event. Client closure logs retain the observed interruption reason instead of the generic `session_ended`.

The summary separates last non-close peer-frame/pong ages from last allowlisted response/output-event ages. Metadata, control frames, unknown events and pongs do not update response/output progress. Missing observations are absent, not zero. Closing frames do not renew peer activity. Terminal/completed-event booleans report observation, not successful validation, accounting or downstream delivery. An output-item-done event alone is not response completion. These diagnostics add only bounded timestamps/booleans in memory and do not change timeout, routing or retry behavior. Idle closure and replacement-handshake events contain no frame contents or close-reason text.

## Deployment and operator access

Use separate unprivileged service/gateway UIDs, protected state/keys and reviewed TLS ingress. [Deployment templates](deploy/README.md) disable API access/error logging, buffering and caching. Keep payload/debug/packet capture, WAF body inspection and APM body capture disabled in your ingress. Client applications and OpenAI have their own retention policies.

Management exposes account identities, usage totals, model IDs and operational metadata, never conversations or request bodies. Content passes through memory after TLS termination. A host operator can inspect memory or replace software, so this is not end-to-end confidentiality against that operator. Connect directly to a trusted provider if that property is required.

Account client-presence reports share server-wide token counts and pseudonymous last-request/running activity with authenticated management users. Viewers receive their own token IDs and encrypted names; other users' clients expose only domain-separated keyed aliases and timing/count metadata, never names, ciphertext, raw token IDs or user identities. Token management and detailed per-token usage remain user-scoped.

Opt-in inference account observations expose a separate user/account HMAC alias,
matched only through the same user's authenticated diagnostics. They contain no
upstream identity and authorize nothing. The desktop plugin correlates primary
response metadata with its own session in bounded transient memory; it never
stores frames, response bodies, turn state or raw router session IDs for this
feature. No account-observation database or public management endpoint is added.

[Personal metadata](docs/private-metadata.md) uses a separate client-held key created automatically by the native client. Existing plaintext token names are encrypted when the native client next loads them; server snapshots do not contain the client key. Confidentiality depends on a trusted client/device, and does not hide access patterns or prevent server rollback/deletion.

## Verification and publication

Privacy regression tests exercise HTTP JSON, SSE, WebSocket, compaction, tool content, malformed JSON and upstream errors under `RUST_LOG=trace`. They search logs and persisted state for synthetic payload markers. Offline CI also audits tracked publication files and Cargo packaging. Runtime state, credentials, local logs and downloaded compatibility clients are excluded from Git and package allowlists.

Use isolated profiles and synthetic prompts in debug/test tooling, never other users' content. When reporting a failure, include versions, transport, status and generated request ID. Do not attach body captures or complete native-client output. Real OAuth/inference/reset-credit tests are never run automatically by GitHub Actions.

Client-carried context proofs encrypt user/account metadata with XChaCha20-Poly1305 under a domain-separated key derived from the existing HMAC key. The authenticated legacy expiry field is retained for wire compatibility but does not limit ownership validity, including for already-issued proofs; new proofs use the maximum signed 64-bit value. Field-specific user-scoped HMACs authenticate the exact upstream value as associated data. A separate HMAC domain derives the 192-bit nonce from field kind, metadata and value so duplicate item events carry identical proofs. Foreign users, altered values and wrong field kinds fail before inference. Every request still requires a currently valid bearer and an authorized, eligible account; an ownership proof does not bypass revocation, deactivation or operational backoff. Proofs are routing credentials: never log or persist their raw values.

New transfers issue client-carried `exrtr1` receipts, not database overrides. XChaCha20-Poly1305 uses a separate HMAC-derived encryption key and random 192-bit nonce; the user ID is authenticated as associated data. Encrypted metadata contains only user/destination IDs and exact field-specific keyed digests of authorized portable ancestors, not prompts or upstream values. Tampering, foreign users and unsigned context fail closed. Explicit account deactivation permits transferring the user's authenticated complete context to an eligible enabled account; it never reauthorizes disabled credentials. The empty reasoning carrier is authenticated and removed before upstream dispatch; no visible reasoning or successful tool result is fabricated. Receipt size, current-snapshot matches and output assembly have fixed bounds, independently of lifetime activity. Compaction replaces the previous receipts. Existing paired transfer overrides remain digests only, under the former finite registry's bound; no new sessions or transfers grow it, and no age-based eviction invalidates saved forks.
