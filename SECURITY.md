# Security and privacy

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/mocki-toki/exetrouter/security/advisories/new). Do not put tokens, private keys, user prompts or raw request/response captures in a public issue. If private reporting is unavailable, open an issue containing only a request for a private contact method.

Use a dedicated unprivileged server account, restricted gateway UID, protected state/keys and reviewed TLS ingress. See [deployment](deploy/README.md).

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

Unavailable-upstream requests use the fixed model label `unavailable`; unvalidated free-form model input is not saved. Operational application logs contain a fixed event vocabulary, server-generated correlation IDs, numerical statuses/counts and timing. Upstream error messages, HTTP header values and arbitrary JSON keys are not logged. Stream diagnostics include the last allowlisted upstream event type, its router-observed Unix timestamp in milliseconds and its age measured by a monotonic clock. Unknown event types become `other`; no event payload is retained. Downstream heartbeats do not update upstream receipt times. The executable fixes its log target/level allowlist; `RUST_LOG=trace` cannot enable dependency body traces.

Ingress templates disable access and error logs for the API virtual host, buffering, caching and upstream retries. Keep payload logging, debug traces, packet/body capture, WAF body inspection and third-party APM capture disabled in your own ingress. Client applications and OpenAI are separate systems with their own retention policies. The bundled native-client tests use isolated temporary profiles and synthetic prompts; never feed other users' content into debug/test tooling.

No management command retrieves conversations or request bodies. A server operator may see account identities, usage totals, model IDs and operational metadata. As a routing proxy, the process handles plaintext content in memory after TLS termination; an operator with host access can change the software or inspect memory. This design does not promise end-to-end confidentiality against that operator. For that property, the client must connect directly to a trusted provider without an untrusted intermediary.

## Verification and publication

Privacy regression tests exercise HTTP JSON, SSE, WebSocket, compaction, tool content, malformed JSON and upstream errors under `RUST_LOG=trace`. They search logs and persisted state for synthetic payload markers. Offline CI also audits tracked publication files and Cargo packaging. Runtime state, credentials, local logs and downloaded compatibility clients are excluded from Git and package allowlists.

When reporting a failure, include versions, transport, status and generated request ID. Do not attach body captures or complete native-client output. Real OAuth/inference/reset-credit tests are never run automatically by GitHub Actions.
