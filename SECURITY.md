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
| Usage | User/token IDs, validated model ID, numeric usage, fixed statuses, timings and request/response identifiers |
| Conversation affinity | User-scoped HMAC digests and derived quota-transfer portability digests; bounded current WS history exists only in transient memory |
| Subscription limits | Percentages, durations, reset/observation times and cooldowns |
| Account identity | Operator-owned account metadata and display email extracted from encrypted issuer credentials |

### Logs

Operational logs contain fixed event names, server-generated correlation IDs, numeric statuses/counts and timing. Unavailable models use the fixed label `unavailable`; unvalidated model input, upstream error messages, header values and arbitrary JSON keys are never saved.

Stream diagnostics retain only the last allowlisted upstream event type, its Unix receipt timestamp in milliseconds and monotonic age. Unknown types become `other`; downstream heartbeats do not refresh these times. The executable fixes allowed log targets/levels, so `RUST_LOG=trace` cannot enable dependency body traces.

## Deployment and operator access

Use separate unprivileged service/gateway UIDs, protected state/keys and reviewed TLS ingress. [Deployment templates](deploy/README.md) disable API access/error logging, buffering, caching and upstream retries. Keep payload/debug/packet capture, WAF body inspection and APM body capture disabled in your ingress. Client applications and OpenAI have their own retention policies.

Management exposes account identities, usage totals, model IDs and operational metadata, never conversations or request bodies. Content passes through memory after TLS termination. A host operator can inspect memory or replace software, so this is not end-to-end confidentiality against that operator. Connect directly to a trusted provider if that property is required.

## Verification and publication

Privacy regression tests exercise HTTP JSON, SSE, WebSocket, compaction, tool content, malformed JSON and upstream errors under `RUST_LOG=trace`. They search logs and persisted state for synthetic payload markers. Offline CI also audits tracked publication files and Cargo packaging. Runtime state, credentials, local logs and downloaded compatibility clients are excluded from Git and package allowlists.

Use isolated profiles and synthetic prompts in debug/test tooling, never other users' content. When reporting a failure, include versions, transport, status and generated request ID. Do not attach body captures or complete native-client output. Real OAuth/inference/reset-credit tests are never run automatically by GitHub Actions.
