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

An owned continuation switches only to an eligible alternative without a reached threshold and with complete recoverable current context. Otherwise a healthy original account continues, including saved backend conversations and older WS branches without a recoverable window. Raising another account's priority alone does not move an owned session. A refused replacement WS handshake during a proactive soft switch falls back to the healthy original connection before any generation submission. On a completed transfer, the downstream WS and scoped identities remain stable, account-specific turn state is removed, and concurrent forks retain their ownership.

Doctor/Limits advertise `capabilities.account_routing_rules: 1`; account rows contain effective `preference.rules`, raw `preference.settings` for editing inheritance, and `threshold_reached`. Rules use null for inheritance and -1 for explicit off in effective diagnostic fields; control patches use a number, `"off"` or `"default"`. `AccountSet.routing` is optional; omitted fields remain unchanged. Legacy `AccountSet.priority` remains accepted, but cannot be supplied together with `routing.priority`. New settings require updated server and native SSH gateway binaries. See [migration 8 to 9](account-routing-migration.md).

## Failure behavior

Explicit quota rejection creates durable cooldown. Before a new generation, an owned HTTP/compact/WS continuation can select an eligible alternative with its full current context. An existing downstream WS stays open while its upstream connection is replaced. The latest completed input/output window reconstructs incremental `previous_response_id` requests; complete tool items and encrypted context pass unchanged. Account-specific `x-codex-turn-state` is dropped on transfer, while scoped session/thread/cache identities remain stable. The new account's native rate-limit events are forwarded without pooling percentages.

A handshake 429 precedes inference. An HTTP 429 body or WS `error` with a recognized quota code may also permit another account only before generation acceptance: no response ID, usage, `response.created`, output or uncertain event may have been observed. A request considers at most four quota-refused inference attempts; each replacement handshake considers at most four accounts. Each refused attempt has its own durable record with unknown counters. All eligible alternatives cooling down returns a quota error with retry information.

A known idle WebSocket closure permits a replacement handshake on the same account for a separate next operation, after authorization and eligibility checks. Recover only the latest completed window or accept full input; never submit old-socket previous IDs to a replacement connection.

Recovery retains at most 16 MiB of serialized current context and 16,384 input/output items per WS, under a shared 64 MiB serialized-data budget. Completed-item assembly is separately bounded to 1 MiB. Exceeding recovery capacity does not interrupt ordinary operation on the original socket. A required quota switch of an unrecoverable incremental request returns `context_recovery_unavailable`; reconnect with the full current context. Older response branches require a full input on transfer. History is transient and disappears when the connection closes or the router restarts.

Temporary catalog/refresh/Responses failures produce operation-specific durable backoff: 5, 10, 20, 40, 80, 160, then 300 seconds, extended by valid Retry-After. A new operation after expiry is the probe; there are no background probes. Any current pause excludes new operations. Known pauses return upstream_backoff and retry information, retaining account affinity.

Ordering and credential generation prevent late successes/failures from overwriting newer observations. Catalog success does not clear a Responses failure. Refresh release and pause publication are atomic, including across multiple workers.

Upstream authentication failure is redacted as upstream_authentication_error (HTTP 502 or a started stream error), distinct from the client's invalid bearer. Native WS closes. A separate next operation performs leased refresh. Repeated rejection of the refreshed generation requires operator reauth. Handshake 401/429 never triggers Chat HTTP fallback.

## Opaque context

The router returns an authenticated `exrctx1` envelope around `encrypted_content`, each `encrypted_function_args` element, issued turn state and saved conversation IDs. Empty encrypted argument arrays remain valid. Encrypted metadata identifies the router user, issuing account and expiry; the exact upstream value and field kind are authenticated. Internal account IDs are not exposed. The original upstream value is extracted only in transient memory before dispatch. Clients must preserve these opaque values exactly.

Normal output creates **no context-binding rows**. Proofs survive restart with the existing HMAC key and expire 24 hours after request admission. Added/done/completed events for the same value in one request carry identical proofs. Reading input does not extend validity. Incoming opaque snapshots retain the bound of 128 distinct elements. New proofs remain usable when the legacy registry is full.

Unknown legacy values, foreign/tampered/expired proofs return `context_not_found` before inference. Valid proofs retain account affinity, subject to account authorization and eligibility. Conflicting untransferred owners or an incompatible bound WS return `context_account_mismatch`. A different bearer belonging to the same user can continue. Context imported from clients bypassing this router remains unsupported.

Legacy raw values continue to resolve through the existing user-scoped HMAC digest registry until their original expiry. No live entries are evicted and no schema/key migration is required. Ordinary output does not renew or add legacy entries.

Quota and configured soft-threshold transfers still require complete current context. Only a transfer saves keyed digest overrides and portability markers for its input, preserving input expiry. Portable ancestors can be shared by concurrent forks; newer untransferred output retains its issuing account. The bounded transfer/legacy registry permits 32768 rows per user and 65536 total. A full registry atomically rejects a transfer with `context_transfer_storage_full` before replacement generation; healthy continuations on the original account do not need a new record. Handshakes may precede this rejection, but inference is not resubmitted.

### Saved conversations

Saved backend conversation IDs and `x-codex-turn-state` also carry user/account proofs. The router unwraps IDs and turn state before forwarding. Saved conversations stay pinned to their issuing account and cannot migrate because their backend-owned history is unavailable. Legacy issued IDs/state retain their existing digest-based validation. Raw IDs and state are not persisted.

### Upgrade and rollback

Deploy server, native gateway and standalone binaries built from the same source. Existing client-held raw context continues to work on upgrade. Preserve the HMAC key: it now authenticates both legacy digests and client-carried proofs. A pre-proof binary cannot accept new envelopes on rollback; clients must reconnect with full text/tool history if that rollback is necessary. A database backup alone cannot make old binaries understand proofs. Do not strip or silently accept unverified envelopes as a fallback.

## Operator and diagnostics

Operators use local `exrd admin oauth add --device`, list, reauth and disable. Remote clients never receive upstream account IDs or credentials. Doctor shows pool health counts and email-labeled per-account quota summaries; `server.quota` retains the older single-account-only field. `exr limits`/Overview TUI show actual 8-hour/weekly/other reported windows with freshness and resets.

Recorded two-account routing, checkpoint portability and synthetic quota-switch evidence is in [live testing](live-testing.md#cross-account-continuity-and-synthetic-quota-failover-2026-10-02). Actual external quota/authentication outages, live WS encrypted-tool portability, full context and sustained concurrent failover remain unverified.

## Starting an inactive weekly window

Live limits may activate an apparently unused weekly window once with a minimal `gpt-5.6-sol` request. Eligibility requires fresh 100% remaining, a reported 10080-minute duration, and a reset exactly seven days ahead at minute precision. Selection is pinned to the account and respects user deactivation, operator policy, catalog support, quota cooldown and operational backoff. A durable claim prevents another attempt for seven days, including after an uncertain result or restart. This heuristic is separate from reset credits and never consumes them. See [CLI behavior](cli.md#weekly-activation) and [schema migration](quota-activation-migration.md).
