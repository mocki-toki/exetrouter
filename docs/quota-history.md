# Hourly subscription history

Account details in the native TUI and OpenCode Desktop show short and weekly
subscription windows together, with separate colors, a shared 0–100% used-quota
axis and a seven-day hourly timeline. The web chart supports pointer/keyboard
selection; TUI details use Left/Right and Home/End, with Up/Down for scrolling.
Usage charts use the same quiet containers, fine lines, distinct identity-based
colors and interval values (Shift+Left/Right and Home/End in the TUI, without
changing Left/Right tab navigation).

The router stores the **latest accepted numeric observation in each UTC hour**,
not an average, token-derived estimate or historical upstream query. Both
general-meter kinds retain their actual duration, reset time and observation
time. A new quota cycle is not smoothed away. Charts leave missing hours empty
and do not connect across gaps or duration changes. Display dates use the
client's local time zone; an hourly tooltip identifies its bucket, not an exact
measurement at the start of that hour. A current hour can change in the background.

Collection runs wherever quota observations already arrive: HTTP/WS metadata
and the existing passive limits worker. No new inference, metadata poll, OAuth
operation or reset-credit action is introduced. It works while dashboards are
closed and survives restart. History starts after upgrade; old days cannot be
reconstructed. Older routers/native clients show an unavailable-history caption,
without hiding current account details.

Authenticated Doctor/Limits expose optional `quota_accounts[].quota_history`
with `from_utc`, exclusive `to_utc`, `interval_seconds: 3600` and bounded `points`.
Each point includes `kind`, `hour`, `observed_at`, `used_percent`, nullable
`window_minutes` and nullable `reset_at`. The current hour and preceding 167
hours are returned, up to 336 points per account; no client identities, request
contents or credentials are retained in history. Capability
`hourly_quota_history: 1` identifies collection support. Ordinary CLI reports
remain text-only; JSON is the automation interface.

## Schema 13 to 14: reviewed migration and rollback

Migration 014 adds only `oauth_quota_history` and an hourly pruning index.
Existing credentials/generations, policies, limits, cooldown/order fences,
usage, reset/activation ledgers, search affinity and keys are untouched. Each
accepted quota write stores its history in the same transaction and under the
same generation/ordering/absence fences. Rejected or late observations cannot
overwrite history. At most 168 rows per account/kind survive a write, even after
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
