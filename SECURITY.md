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
| Personal metadata | Client-encrypted token names, account labels and dashboard preferences; user key never sent to service |
| Usage | User/token IDs, validated model ID, numeric usage, fixed statuses, timings and request/response identifiers |
| Conversation affinity | Client-carried authenticated envelopes and explicit digest-only transfer/portability overrides. Unwrapped client context is rejected. Bounded current WS history exists only in transient memory |
| Subscription limits | Percentages, durations, reset/observation times and cooldowns |
| Account identity | Operator-owned account metadata and display email extracted from encrypted issuer credentials |

### Logs

Operational logs contain fixed event names, server-generated correlation IDs, numeric statuses/counts and timing. Unavailable models use the fixed label `unavailable`; unvalidated model input, upstream error messages, header values and arbitrary JSON keys are never saved.

Client WebSocket diagnostics correlate upgrade receipt, session opening/closure, text-frame receipt and generation admission with a generated connection ID and numeric frame sequence. They record frame byte counts and preparation/write/first-event timings, not frame contents. Native upstream request start, successful socket write, first event and interruption share generated request/connection IDs. A successful write does not prove upstream acceptance. Session closure reasons are fixed categories; `session_ended` covers exits without a more specific observed client-side cause.

WebSocket liveness diagnostics use numeric ping/pong counts, monotonic last-frame age and a boolean response-started flag. Proactive upstream retirement records fixed idle/age expiry reasons and runs only between requests with a recoverable completed context. Peer pongs renew transport liveness without proving request acceptance or generation progress; router-generated heartbeats do not renew upstream receive deadlines.

Stream diagnostics retain only the last allowlisted upstream event type, its Unix receipt timestamp in milliseconds and monotonic age. Unknown types become `other`; downstream heartbeats do not refresh these times. The executable fixes allowed log targets/levels, so `RUST_LOG=trace` cannot enable dependency body traces.

WebSocket interruption diagnostics include a fixed reason and stage, elapsed time, a fixed transport-error category and an optional numeric close code. They distinguish upstream closure/read/write failures, invalid events, client closure/concurrent frames, storage stages and service shutdown. Close-reason text, underlying error messages and frame contents are never logged. An interrupted request with no observed events still has an unknown outcome. Native WS logs also correlate requests with generated upstream connection IDs, connection age and the number of finished requests. Idle closure and replacement-handshake events contain no frame contents or close-reason text.

## Deployment and operator access

Use separate unprivileged service/gateway UIDs, protected state/keys and reviewed TLS ingress. [Deployment templates](deploy/README.md) disable API access/error logging, buffering and caching. Keep payload/debug/packet capture, WAF body inspection and APM body capture disabled in your ingress. Client applications and OpenAI have their own retention policies.

Management exposes account identities, usage totals, model IDs and operational metadata, never conversations or request bodies. Content passes through memory after TLS termination. A host operator can inspect memory or replace software, so this is not end-to-end confidentiality against that operator. Connect directly to a trusted provider if that property is required.

[Personal metadata](docs/private-metadata.md) uses a separate client-held key created automatically by the native client. Existing plaintext token names are encrypted when the native client next loads them; server snapshots do not contain the client key. Confidentiality depends on a trusted client/device, and does not hide access patterns or prevent server rollback/deletion.

## Verification and publication

Privacy regression tests exercise HTTP JSON, SSE, WebSocket, compaction, tool content, malformed JSON and upstream errors under `RUST_LOG=trace`. They search logs and persisted state for synthetic payload markers. Offline CI also audits tracked publication files and Cargo packaging. Runtime state, credentials, local logs and downloaded compatibility clients are excluded from Git and package allowlists.

Use isolated profiles and synthetic prompts in debug/test tooling, never other users' content. When reporting a failure, include versions, transport, status and generated request ID. Do not attach body captures or complete native-client output. Real OAuth/inference/reset-credit tests are never run automatically by GitHub Actions.

Client-carried context proofs encrypt user/account/expiry metadata with XChaCha20-Poly1305 under a domain-separated key derived from the existing HMAC key. Field-specific user-scoped HMACs authenticate the exact upstream value as associated data. A separate HMAC domain derives the 192-bit nonce from field kind, metadata and value so duplicate item events carry identical proofs. Foreign users, altered values, wrong field kinds and expired proofs fail before inference. Proofs are routing credentials: never log or persist their raw values. Transfer overrides remain digests only.
