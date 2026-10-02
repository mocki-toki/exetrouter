# OAuth account pool

Each active ChatGPT OAuth account has independent encrypted credentials, generation, refresh lease, model catalog, quota observations and operation health. Adding the same upstream account updates its record instead of creating another pool entry.

## Selection

The visible model list is a union of active account catalogs. An independent full-context request chooses an eligible account for its model, respecting current credentials/catalog, cooldown, operational pauses, quota observations and active load. Explicit cache/session hints provide a stable soft preference among eligible accounts. Stale quota is not treated as confirmed current capacity; percentages are not combined across subscriptions.

HTTP text/tool requests can choose independently. Known user-scoped opaque context and a bound WebSocket retain account affinity during normal operation. Confirmed quota cooldown or a current 100% window with a future reset permits automatic account failover when another eligible account exists. Preferences still govern eligible alternatives; exhausted subscriptions are skipped when other capacity is available. If every subscription is at 100% without explicit cooldown, credits may still permit a request on the original account. Stale observations and an approaching reset do not trigger failover.

## User preferences and operator policy

`exr account set ID --enabled false` deactivates an account for the current user without deleting it or changing OAuth credentials. `--enabled true` reverses this. `--priority 1` prefers the account for new independent requests; higher priorities win before quota, load and soft cache hints. Equal priorities retain existing pool selection. Deactivation rejects future requests on an owning account; quota failover does not bypass this policy. Already submitted responses finish.

`exrd admin oauth policy ID --enabled false --locked true` excludes an account for everyone and locks client changes. `--priority 1 --locked true` enforces priority for everyone. Global deactivation always overrides user activation. Unlocked global priority is the default where a user has no preference. Unlocking restores stored personal preferences. Policy changes do not reauthorize OAuth accounts disabled through the older credential-state command. Doctor/Overview exposes effective enabled, priority and locked fields, without credentials.

## Failure behavior

Explicit quota rejection creates durable cooldown. Before a new generation, an owned HTTP/compact/WS continuation can select an eligible alternative with its full current context. An existing downstream WS stays open while its upstream connection is replaced. The latest completed input/output window reconstructs incremental `previous_response_id` requests; complete tool items and encrypted context pass unchanged. Account-specific `x-codex-turn-state` is dropped on transfer, while scoped session/thread/cache identities remain stable. The new account's native rate-limit events are forwarded without pooling percentages.

A handshake 429 precedes inference. An HTTP 429 body or WS `error` with a recognized quota code may also permit another account only before generation acceptance: no response ID, usage, `response.created`, output or uncertain event may have been observed. Generic 429 responses, late quota errors, authentication failures, backoff, 5xx, EOF and timeouts do not permit inference replay. A request considers at most four quota-refused inference attempts; each replacement handshake considers at most four accounts. Each refused attempt has its own durable record with unknown counters. All eligible alternatives cooling down returns a quota error with retry information.

Recovery retains at most 16 MiB of serialized current context and 16,384 input/output items per WS, under a shared 64 MiB serialized-data budget. Completed-item assembly is separately bounded to 1 MiB. Exceeding recovery capacity does not interrupt ordinary operation on the original socket. Switching an unrecoverable incremental request returns `context_recovery_unavailable`; reconnect with the full current context. Older response branches require a full input on transfer. History is transient and disappears when the connection closes or the router restarts.

Temporary catalog/refresh/Responses failures produce operation-specific durable backoff: 5, 10, 20, 40, 80, 160, then 300 seconds, extended by valid Retry-After. A new operation after expiry is the probe; there are no background probes. Any current pause excludes new operations. Known pauses return upstream_backoff and retry information, retaining account affinity.

Ordering and credential generation prevent late successes/failures from overwriting newer observations. Catalog success does not clear a Responses failure. Refresh release and pause publication are atomic, including across multiple workers.

Upstream authentication failure is redacted as upstream_authentication_error (HTTP 502 or a started stream error), distinct from the client's invalid bearer. Native WS closes; the current inference is not replayed. A separate next operation performs leased refresh. Repeated rejection of the refreshed generation requires operator reauth. Handshake 401/429 never triggers Chat HTTP fallback.

## Opaque context

The router saves a domain-separated user-scoped HMAC digest, internal account ID and expiry for encrypted_content output. No checkpoint, text, summary or tool arguments are persisted. Bindings survive restart with the same database/HMAC key; refresh preserves account identity.

Lifetime is 24 hours from the last observation of that item in output, not extended by merely reading input. Bounds: 128 distinct opaque elements per snapshot, 4096 live bindings per user, 65536 total. Live bindings are not evicted to make room; failure is explicit and inference is not replayed.

All opaque input must have live bindings for the current user. Unknown/expired/foreign context yields context_not_found. Quota transfer records an additional derived portability digest in the same bounded registry, without extending the input's expiry. Portable ancestors can be shared by concurrent forks; newer untransferred output retains its own account affinity. Conflicting untransferred account owners or an incompatible bound WS yield context_account_mismatch. Malformed opaque input is rejected. Another bearer of the same user can continue; another user cannot. Context imported from clients bypassing this router is unsupported.

Bindings are saved before exposing the corresponding output. Storage failure stops projection/stream explicitly and retains independently known terminal usage. The router never exposes an unbound checkpoint. Quota-only transfer validates existing user ownership atomically and consumes registry capacity for its derived portability digests; no schema migration or raw history storage is required.

## Operator and diagnostics

Operators use local `exrd admin oauth add --device`, list, reauth and disable. Remote clients never receive upstream account IDs or credentials. Doctor shows pool health counts and email-labeled per-account quota summaries; `server.quota` retains the older single-account-only field. `exr limits`/Overview TUI show actual 8-hour/weekly/other reported windows with freshness and resets.

Short actual two-account routing and preserved affinity around a synthetic operational pause passed. Separate October 2 direct HTTP and classic WS probes confirmed cross-account encrypted compaction and classic tool continuity. Current native Codex/OpenCode HTTP/WS quota-switch tests passed against synthetic refusals. A newly built router also passed six real HTTP/WS requests across synthetic quota boundaries in isolated access-only state, with preserved canaries and exact durable account transitions. Actual external quota/authentication outage recovery and live WS encrypted-tool portability are not established by these tests. Full context, sustained sessions, model meters/credits, polling and load remain separate work. See [live testing](live-testing.md).
