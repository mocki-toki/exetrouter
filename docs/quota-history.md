# Subscription history

Account details in the native TUI and OpenCode Desktop show short and weekly
subscription windows together, with separate colors, a shared 0–100% quota-left
axis. Both native TUI and OpenCode Desktop offer rolling **24 hours,
7 days, 30 days and 90 days**, with earlier/later interval paging. The web chart
supports pointer/keyboard selection; TUI details use Left/Right and Home/End, with Up/Down for scrolling.
Lines and cursor values show only the remaining percentage: a full window is
100%, an exhausted window is 0%, and quota recovery moves the line upward.
Storage keeps upstream used percentages; display converts them without changing
the collected percentages or routing behavior.
Usage charts use the same quiet containers, fine lines, distinct identity-based
colors and interval values (Shift+Left/Right and Home/End in the TUI, without
changing Left/Right tab navigation).

The router stores the **latest accepted numeric observation in each UTC ten-minute interval**,
not an average, token-derived estimate or historical upstream query. Both
general-meter kinds retain their actual duration, reset time and observation
time. A new quota cycle is not smoothed away. Charts leave missing intervals empty
and do not connect across gaps or duration changes. Display dates use the
client's local time zone; a tooltip identifies its bucket, not an exact
measurement at its start. The current bucket can change in the background.

Collection runs wherever quota observations already arrive: HTTP/WS metadata
and the existing passive limits worker. No new inference, metadata poll, OAuth
operation or reset-credit action is introduced. It works while dashboards are
closed and survives restart. History starts after upgrade; old days cannot be
reconstructed. Updated clients also accept older routers' hourly history. Older
clients require an update for ten-minute history; current account details remain
available in the native TUI.

Authenticated Doctor/Limits expose optional `quota_accounts[].quota_history`
with `from_utc`, exclusive `to_utc`, `interval_seconds: 600` and bounded `points`.
Each point includes `kind`, `hour`, `observed_at`, `used_percent`, nullable
`window_minutes` and nullable `reset_at`. The legacy name `hour` now denotes the
start of a ten-minute bucket. The current bucket and preceding 1,007 buckets
are returned, up to 2,016 points per account; no client identities, request
contents or credentials are retained in history. Capability
`ten_minute_quota_history: 1` identifies the new resolution; the legacy
`hourly_quota_history: 1` collection flag is retained. Ordinary CLI reports
remain text-only; JSON is the automation interface.

OpenCode Desktop also accepts ten-minute (`interval_seconds: 600`) reports from
compatible routers. It preserves their bucket alignment, observation times and
gaps, labels the reported interval, and permits ten-minute-aligned paging ends.
Both resolutions are bounded to 90 days and two window kinds (at most 25,920
ten-minute points); Doctor and explicit history reads have an 8 MiB output bound
in plugin 0.4.1 and native client 0.9.2. Other plugin reads retain a 1 MiB bound. Unsupported
or malformed optional snapshot history is shown as unavailable without hiding
current account details. Explicit history reads still reject invalid reports.

`exr quota-history ACCOUNT --period 90d --json` reads a wider interval through the
authenticated management channel, without refreshing upstream. `--before UNIX`
sets its exclusive, ten-minute-aligned end; subtract the selected duration to page back.
Each response contains at most 90 days / 25,920 points. Desktop paging needs updated
native and server binaries; an older router's seven-day snapshot remains visible.
Numeric observations are retained for at most **365 days**, up to 52,560 rows per
account/window. Previously pruned observations cannot be recovered; retention expands
only as new observations arrive. Rolling windows have fixed durations even
across DST. Collection still uses the existing roughly 30-second passive worker;
changing bucket size adds no polls.

## Schema 15 to 16: ten-minute buckets

Migration 016 transactionally rebuilds only the numeric history table with
600-second alignment and observation constraints. Each old hourly sample moves
to the bucket containing its actual `observed_at`. No samples are invented for
the other five buckets; old gaps remain gaps. Credentials, keys, tokens, usage,
current quotas, routing and reset ledgers are unchanged. Retention remains 365
days, with a sixfold maximum row count. Allow disk space for a temporary second
copy of the history table.

This is not a routine one-click deployment. With operator authorization, stop
the runtime, create and verify a private consistent backup of current state and
keys, and rehearse the migration on a private copy. Deploy matching server and
gateway binaries, verify schema 16, integrity/foreign keys, health, authentication
and retained numeric history without inference or credit consumption. Update
native clients and the Desktop plugin together; old Desktop plugins reject the
new interval. Publishing source does not change deployed state.
Desktop subprocess/stdio reports remain bounded to 8 MiB so a full 90-day
ten-minute history fits; concurrent read limits are unchanged.

For rollback, stop the runtime and preserve a verified **current** state/key
backup first. Do not restore a pre-upgrade database or simply lower user_version:
schema 15 cannot write ten-minute buckets. On a private copy, rehearse rebuilding
the table using migration 014's original constraints: retain the latest
`observed_at` (then `request_order` on ties) per account/kind/UTC hour, rounding
the bucket start down to 3,600 seconds. Preserve an offline private copy of all
fine-resolution rows before reducing them. Recreate the original pruning index,
set `PRAGMA user_version=15` in the same transaction, and verify integrity,
foreign keys and unchanged non-history state. Restore matching old server and
gateway binaries and check read-only health/authentication. Re-upgrade remaps
the retained hourly samples; it does not recover discarded fine-resolution rows.

## Original schema 13 to 14: reviewed migration and rollback

Migration 014 adds only `oauth_quota_history` and an hourly pruning index.
Existing credentials/generations, policies, limits, cooldown/order fences,
usage, reset/activation ledgers, search affinity and keys are untouched. Each
accepted quota write stores its history in the same transaction and under the
same generation/ordering/absence fences. Rejected or late observations cannot
overwrite history. At most 8,760 rows per account/kind survive a write, even after
a backward clock adjustment. Old hours are pruned at startup and the next quota
write; stale accounts' retained rows stay bounded and are excluded from reports.

Publishing this release does **not** migrate a deployed server. Routine Docker
updates must continue to refuse a schema mismatch. With operator authorization:

1. Inspect the actual deployment and preserve its units, Compose overrides,
   ingress, SSH boundaries and private configuration. Stop the old runtime and
   retain its exact server/gateway binaries or pinned image.
2. Use the old version to create and verify a consistent private state/keys
   snapshot. Rehearse restore into a private scratch directory, never an active
   second router. Preserve the snapshot and both existing keys.
3. Deploy matching new server/gateway/standalone binaries and allow the normal
   transactional startup migration. Verify schema 14, SQLite integrity/foreign
   keys, health and authenticated read-only diagnostics. Confirm retained
   policies/authentication and numeric history without sending inference or
   spending credits. Update native clients to retain the new Doctor field.

For rollback, stop the new runtime and back up **current** state/keys first.
Verify that the database is exactly schema 14 with only the reviewed migration
014 additions. In a reviewed offline transaction, retain the additive table/index
and their rows and set `PRAGMA user_version=13`; restore the old server/gateway
binaries together. Old code ignores the table and stops collecting history.
Do not restore the pre-upgrade database just to roll back code: doing so can
discard usage or rotated credentials. Run integrity/foreign-key and read-only
health/authentication checks. Re-upgrade is idempotent and retains in-range
history. No migration/rollback deploy is performed by the GUI or restricted SSH
gateway.
