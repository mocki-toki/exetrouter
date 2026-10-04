# Account preferences: schema 6 to 7

This is an explicit operator migration, not a routine packaging update. Migration 007 adds two tables with foreign keys and bounded numeric priorities. It changes no OAuth ciphertext, generations, model metadata, usage, tokens, keys or user identity. Both server and standalone initialization apply it transactionally. Older binaries refuse schema 7; deploy matching server and native SSH gateway binaries together. Clients without account-action support continue existing inspection/routing commands; new preference commands require the updated server.

## Before migration

Before migration, build/test the new binaries and image, preserve the original image, native gateway and deployment settings, and create/verify a private schema-6 snapshot using the installed binary. Stop the router before switching. Preserve host services, SSH peer UID boundaries, configuration, keys and volumes. Start the new image, check health and authenticated diagnostics without inference, then apply reviewed account policy. Verify that locked preference changes are rejected.

## Rollback

For a failed immediate rollout, stop the new router. Preserve a private snapshot of the current state before rollback. No pre-existing table or column changed, so an operator can drop only `account_preferences` and `account_policy` and set `PRAGMA user_version=6` in one transaction, then restore the prior gateway/image/environment. This retains current OAuth credentials and usage, avoiding an old refresh-token snapshot. This narrow rollback is suitable only for this immediate rollout before accepting preference changes. Once users configure preferences, preserve/export them and review rollback separately; do not silently discard user settings. The original verified backup remains disaster recovery, not an automatic overwrite of newer credentials/accounting.
