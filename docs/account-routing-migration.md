# Account routing rules: schema 8 to 9

Migration 009 rebuilds account_policy and account_preferences transactionally to widen priority constraints to -255…255 and add nullable thresholds. Existing numeric priorities, activation preferences and locks retain their exact values. Accounts without configured priorities now use 1 instead of 0; their relative ordering against explicitly configured accounts can change. Thresholds start off. OAuth ciphertext, credentials, keys, user identities, usage, context bindings, catalogs and quota observations are preserved.

## Before deployment

This is an explicit schema migration, not a routine one-click packaging update. Build and pass offline checks first. Inspect the real deployment, preserve operator configuration/service names and old image/native binaries, then use the installed schema-8 binary to create and verify a private snapshot containing the database and both keys. Stop the service before changing server and native SSH gateway binaries together. Starting a new server or standalone binary applies migration 009. Older binaries refuse schema 9. Ordinary Docker updates must continue refusing mismatched schemas.

Verify health and authenticated Doctor without inference. Confirm capability account_routing_rules is 1, inspect preserved settings, change synthetic/test-user rules where authorized, and verify operator locks. Client settings do not need reconfiguration; older clients keep their existing commands, while new routing-rule actions require updated server/gateway support. Review [selection rules](account-pool.md#soft-switching-thresholds).

## Immediate rollback

Stop the new service and create/verify a private snapshot of current schema-9 state using the new binary before rollback. Prefer converting that current database rather than restoring old refresh tokens or losing newer usage.

A schema-8 conversion is safe only if policy switch_at is -1, all policy window overrides and all personal threshold columns are NULL, and every saved priority is still between -100 and 100. Any other threshold settings need explicit review even if they currently mean off. In a reviewed operator transaction, rebuild account_policy and account_preferences using the exact definitions from migration 007, copy only their original columns with all values intact, drop the schema-9 tables, verify foreign keys and database integrity, and set PRAGMA user_version=8. Preserve every other table and both keys. The old implicit default becomes 0 again. Restart the original server and native gateway together, then verify health/Doctor without inference.

If any threshold setting exists or a priority exceeds the old range, do not silently discard it or clamp the number. Keep the current snapshot, export routing settings privately and review an explicit mapping and recovery procedure with the operator before conversion. Restoring the pre-upgrade snapshot is disaster recovery only; it can discard new accounting and invalidate refreshed credentials. Never operate both copies of the same OAuth pool concurrently.
