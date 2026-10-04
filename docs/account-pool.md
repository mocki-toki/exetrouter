# OAuth account pool

Each active ChatGPT OAuth account has independent encrypted credentials, generation, refresh lease, model catalog, quota observations and operation health. Adding the same upstream account updates its record instead of creating another pool entry.

## Selection

The visible model list is a union of active account catalogs. An independent full-context request chooses an eligible account for its model, respecting current credentials/catalog, cooldown, operational pauses, quota observations and active load. Explicit cache/session hints provide a stable soft preference among eligible accounts. Stale quota is not treated as confirmed current capacity; percentages are not combined across subscriptions.

HTTP text/tool requests can choose independently. Known user-scoped opaque context and a bound WebSocket retain account affinity during normal operation. Confirmed quota cooldown or a current 100% window with a future reset permits automatic account failover when another eligible account exists. Preferences still govern eligible alternatives; exhausted subscriptions are skipped when other capacity is available. If every subscription is at 100% without explicit cooldown, credits may still permit a request on the original account. Stale observations and an approaching reset do not trigger failover.

## User preferences and operator policy

`exr account set ID --enabled false` deactivates an account for the current user without deleting it or changing OAuth credentials. `--enabled true` reverses this. `--priority 10` prefers the account for new independent requests; priorities are whole numbers from -255 through 255, with a default of 1. Higher priorities win before quota, load and soft cache hints within the same threshold tier. Existing saved numbers are preserved on upgrade. Equal priorities retain existing pool selection. Deactivation rejects future requests on an owning account; quota failover does not bypass this policy. Already submitted responses finish.

`exrd admin oauth policy ID --enabled false --locked true` excludes an account for everyone and locks client changes. `--priority 10 --switch-at 20 --locked true` enforces priority and switching rules for everyone. Global deactivation always overrides user activation. Unlocked global priority is the default where a user has no preference. Unlocking restores stored personal preferences. Policy changes do not reauthorize OAuth accounts disabled through the older credential-state command. Doctor/Overview exposes effective enabled, priority and locked fields, without credentials.

### Soft switching thresholds

`exr account set ID --switch-at 20` prefers another eligible account when a current window has **20% or less remaining**. `--switch-at-short 25` overrides the general threshold for reported durations shorter than 10080 minutes; `--switch-at-weekly 15` overrides exactly 10080 minutes. Duration, rather than primary/secondary position, determines the window. Other or unknown durations use the general threshold. A single matching current window is sufficient; stale/reset-elapsed observations do not trigger switching and unknown capacity is not zero.

For each field, personal settings override unlocked operator settings; missing personal settings inherit operator defaults. After resolving settings, an explicit window threshold replaces the general threshold. `off` disables a threshold, including a window-specific exception to the general rule. `default` removes the field's override: personal settings inherit operator values; operator priority resets to 1, general threshold turns off and window overrides inherit the general threshold. All thresholds are off initially. Values are whole percentages from 0 through 100, inclusive.

Independent requests prefer accounts without a reached threshold, then use priority and the existing pool ranking. If preferred accounts cannot pass eligibility checks, the router uses the remaining eligible tier. Soft rules never consume reset credits, create cooldowns or override deactivation, authentication or operational backoff. Real quota exhaustion still follows the existing rules below, even when alternatives have reached their soft thresholds.

An owned continuation switches only to an eligible alternative without a reached threshold and with complete recoverable current context. Otherwise a healthy original account continues, including saved backend conversations and older WS branches without a recoverable window. Raising another account's priority alone does not move an owned session. A refused replacement WS handshake during a proactive soft switch falls back to the healthy original connection before any generation submission. Started responses are never interrupted or replayed. On a completed transfer, the downstream WS and scoped identities remain stable, account-specific turn state is removed, and concurrent forks retain their ownership.

Doctor/Limits advertise `capabilities.account_routing_rules: 1`; account rows contain effective `preference.rules`, raw `preference.settings` for editing inheritance, and `threshold_reached`. Rules use null for inheritance and -1 for explicit off in effective diagnostic fields; control patches use a number, `"off"` or `"default"`. `AccountSet.routing` is optional; omitted fields remain unchanged. Legacy `AccountSet.priority` remains accepted, but cannot be supplied together with `routing.priority`. New settings require updated server and native SSH gateway binaries. See [migration 8 to 9](account-routing-migration.md).

## Failure behavior

Explicit quota rejection creates durable cooldown. Before a new generation, an owned HTTP/compact/WS continuation can select an eligible alternative with its full current context. An existing downstream WS stays open while its upstream connection is replaced. The latest completed input/output window reconstructs incremental `previous_response_id` requests; complete tool items and encrypted context pass unchanged. Account-specific `x-codex-turn-state` is dropped on transfer, while scoped session/thread/cache identities remain stable. The new account's native rate-limit events are forwarded without pooling percentages.

A handshake 429 precedes inference. An HTTP 429 body or WS `error` with a recognized quota code may also permit another account only before generation acceptance: no response ID, usage, `response.created`, output or uncertain event may have been observed. Generic 429 responses, late quota errors, authentication failures, backoff, 5xx, EOF and timeouts do not permit inference replay. A request considers at most four quota-refused inference attempts; each replacement handshake considers at most four accounts. Each refused attempt has its own durable record with unknown counters. All eligible alternatives cooling down returns a quota error with retry information.

A known idle WebSocket closure permits a replacement handshake on the same account for a separate next operation, after authorization and eligibility checks. Recover only the latest completed window or accept full input; never submit old-socket previous IDs to a replacement connection. This transport recovery neither replays inference nor authorizes account migration.

Recovery retains at most 16 MiB of serialized current context and 16,384 input/output items per WS, under a shared 64 MiB serialized-data budget. Completed-item assembly is separately bounded to 1 MiB. Exceeding recovery capacity does not interrupt ordinary operation on the original socket. A required quota switch of an unrecoverable incremental request returns `context_recovery_unavailable`; reconnect with the full current context. Older response branches require a full input on transfer. History is transient and disappears when the connection closes or the router restarts.

Temporary catalog/refresh/Responses failures produce operation-specific durable backoff: 5, 10, 20, 40, 80, 160, then 300 seconds, extended by valid Retry-After. A new operation after expiry is the probe; there are no background probes. Any current pause excludes new operations. Known pauses return upstream_backoff and retry information, retaining account affinity.

Ordering and credential generation prevent late successes/failures from overwriting newer observations. Catalog success does not clear a Responses failure. Refresh release and pause publication are atomic, including across multiple workers.

Upstream authentication failure is redacted as upstream_authentication_error (HTTP 502 or a started stream error), distinct from the client's invalid bearer. Native WS closes; the current inference is not replayed. A separate next operation performs leased refresh. Repeated rejection of the refreshed generation requires operator reauth. Handshake 401/429 never triggers Chat HTTP fallback.

## Opaque context

The router saves a domain-separated user-scoped HMAC digest, internal account ID and expiry for `encrypted_content` and `encrypted_function_args` output. Empty encrypted argument arrays are valid. No checkpoint, text, summary or tool arguments are persisted. Bindings survive restart with the same database/HMAC key; refresh preserves account identity.

Lifetime is 24 hours from the last observation of that item in output, not extended by merely reading input. Bounds: 128 distinct opaque elements per snapshot, 4096 live bindings per user, 65536 total. Live bindings are not evicted to make room; failure is explicit and inference is not replayed.

All opaque input must have live bindings for the current user. Unknown/expired/foreign context yields context_not_found. Quota transfer records an additional derived portability digest in the same bounded registry, without extending the input's expiry. Portable ancestors can be shared by concurrent forks; newer untransferred output retains its own account affinity. Conflicting untransferred account owners or an incompatible bound WS yield context_account_mismatch. Malformed opaque input is rejected. Another bearer of the same user can continue; another user cannot. Context imported from clients bypassing this router is unsupported.

Bindings are saved before exposing the corresponding output. Storage failure stops projection/stream explicitly and retains independently known terminal usage. The router never exposes an unbound checkpoint. Quota or configured-threshold transfer validates existing user ownership atomically and consumes registry capacity for its derived portability digests; no schema migration or raw history storage is required.

### Saved conversations

Saved backend conversation references use a separate user-scoped HMAC domain in the bounded context registry. Only IDs observed in that user's Responses events are accepted for 24 hours, and continuation remains pinned to the issuing account. Stored conversations cannot migrate on quota rejection because their backend-owned history is unavailable to the router. Raw IDs remain transient and are not persisted.

## Operator and diagnostics

Operators use local `exrd admin oauth add --device`, list, reauth and disable. Remote clients never receive upstream account IDs or credentials. Doctor shows pool health counts and email-labeled per-account quota summaries; `server.quota` retains the older single-account-only field. `exr limits`/Overview TUI show actual 8-hour/weekly/other reported windows with freshness and resets.

Recorded two-account routing, checkpoint portability and synthetic quota-switch evidence is in [live testing](live-testing.md#cross-account-continuity-and-synthetic-quota-failover-2026-10-02). Actual external quota/authentication outages, live WS encrypted-tool portability, full context and sustained concurrent failover remain unverified.

## Starting an inactive weekly window

Live limits may activate an apparently unused weekly window once with a minimal `gpt-5.6-sol` request. Eligibility requires fresh 100% remaining, a reported 10080-minute duration, and a reset exactly seven days ahead at minute precision. Selection is pinned to the account and respects user deactivation, operator policy, catalog support, quota cooldown and operational backoff. A durable claim prevents another attempt for seven days, including after an uncertain result or restart. This heuristic is separate from reset credits and never consumes them. See [CLI behavior](cli.md#weekly-activation) and [schema migration](quota-activation-migration.md).
