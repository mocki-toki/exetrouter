# Architecture

The router separates the public model API from management:

```text
Codex / OpenCode / OpenAI SDK
    │ Router bearer; HTTP/JSON/SSE or WebSocket
    ▼
TLS proxy (deployment pending) → exrd on loopback → ChatGPT Codex OAuth backend
                                  │
                                  └→ SQLite + separate private keys

exr CLI/TUI → restricted SSH → forced exrd gateway → authenticated Unix control socket
```

`exrd` authenticates router tokens, chooses an eligible account, forwards Responses or translates supported Chat requests, and records operational usage metadata. Agent tools run on the client; the router does not execute them. `exr` configures its SSH connection locally, manages the user's tokens and displays stored diagnostics/usage/limits. Operator commands run locally on the server.

## Components

The shared Rust library contains auth, backup, store, usage, control, doctor, OAuth, quota, health, pool, affinity, cache, catalog, upstream, Chat, server transport/output/limits and client config/render/TUI modules. SQLite runs on a dedicated bounded worker. Migrations are transactional; schema 6 retains users/credentials/accounting while invalidating the older metadata-free catalog.

HTTP authenticates before JSON parsing. Management resolves the forced server-side SSH identity to its user and verifies Unix peer UID; client-supplied user IDs do not select authority. Public management endpoints do not exist.

## Routing and transport

Catalogs are merged across active accounts. Selection respects the requested model, quota/health and active load. Explicit cache/session hints provide a soft preference; known opaque context and an existing WS provide strong account affinity.

Native WS opens upstream after the first model-bearing response.create. HTTP Responses uses upstream SSE and can return raw SSE or assembled terminal JSON. Chat first tries upstream WS, with HTTP/SSE fallback only after an ordinary handshake failure **before** inference submission. Handshake authentication/quota failures prevent fallback. A possibly submitted request is never replayed.

Opaque output is fingerprinted with a user-scoped HMAC and bound to its originating account for 24 hours. Checkpoint data and prompts are not persisted. WS response IDs belong to that socket only. New sockets require full context or a known checkpoint; HTTP previous_response_id is unsupported.

## Storage and accounting

One durable event is created before possible upstream submission and finalized from the terminal outcome. Interruptions and startup recovery preserve unknown consumption. Input/output/cached/reasoning counters remain nullable rather than invented zeros. Raw content is bounded in transient memory where projection needs it, not stored in SQLite.

Quota observations preserve actual durations/reset/observation times. Explicit upstream rejection produces account cooldown; temporary errors produce operation-specific backoff. Already submitted responses continue. Diagnostic reads are metadata-only and do not probe network readiness.

Per-user/global concurrency limits and bounded queues/messages protect resources. There is no request-frequency quota for the current private use case. See [implementation](implementation.md), [pool](account-pool.md) and [upstream](upstream-contract.md) for bounds and failure contracts.
