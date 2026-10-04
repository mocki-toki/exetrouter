# Weekly activation: schema 7 to 8

This is an explicit operator migration, not a routine packaging update. Migration 008 adds only `quota_activation_attempts` and its account/time indexes. It preserves credentials, keys, users, tokens, preferences, existing usage and context. Claims are persisted before possible inference submission; nullable numeric counters and fixed operational status are retained without prompts or generated content. Older binaries refuse schema 8. Deploy matching server and native SSH gateway binaries together. Standalone initialization also applies the migration transactionally.

## Before deployment

Before deployment, review the weekly-activation behavior in [weekly activation](cli.md#weekly-activation), build and validate the new binaries/image, retain the previous image and gateway, and create/verify a private schema-7 snapshot with the installed binary. Stop the router before switching; preserve configuration, volumes, credentials and SSH peer UID boundaries. The Docker updater must continue to refuse a mismatched schema. Start the new version and verify health and Doctor without inference. A Limits/Overview refresh can now trigger activation under the documented condition; include it only when inference is authorized.

## Rollback

For an immediate rollback before any activation has been claimed, stop the router, verify that the new table is empty, preserve a private current snapshot, and drop only `quota_activation_attempts` while setting `PRAGMA user_version=7` in a single transaction. Restore the prior image/gateway/configuration. Do not restore an old credential snapshot over newer refreshed credentials.

After an activation attempt exists, this simple rollback is not allowed: dropping the table would lose numeric accounting and the durable no-replay guard. Preserve the current database and review a rollback that retains these records and suppresses automatic activation on any subsequent upgrade. A previous backup is disaster recovery, not an automatic overwrite of newer credentials or usage. No migration or production deployment is performed by ordinary tests.
