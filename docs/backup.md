# Backup and restore

Local operator commands run without SSH or upstream access. A snapshot contains SQLite, the bearer HMAC key, the OAuth encryption key and backup.json. It contains existing operational state, not new conversation bodies.

## Create

```sh
mkdir -m 700 /path/to/backups
exrd --db /path/to/state/exetrouter.sqlite \
  --key /path/to/state/exetrouter.key \
  --oauth-key /path/to/state/exetrouter.oauth.key \
  admin backup create --to /path/to/backups/new-snapshot
```

Run as the owner of source files. The destination must be new; its parent must already exist, be current-user-owned and not group/other writable. Default state filenames can replace explicit globals when running from that directory.

The service can remain running. SQLite Online Backup API includes committed WAL, incrementally within a 120-second deadline. The result is a standalone DELETE-journal database without source sidecar dependence. Integrity, foreign keys, schema and decryption of every OAuth record are checked. Changing keys during backup aborts creation.

Directory mode is 0700, files 0600. Data/keys are synced before the manifest is written, then directories are synced. Manifest format 1 records schema/time and SHA-256/size for three fixed files. Ordinary failures remove the operation's partial output; crash leftovers without a valid manifest are not completed backups.

## Verify

```sh
exrd admin backup verify --from /path/to/backups/new-snapshot
```

Current working state is not required. Checks include manifest/hashes, owner/private modes, SQLite and OAuth decryption. Symlinks, sidecars, incompatible schema, missing/damaged files and wrong keys are rejected. The default report is readable text; `--json` reports counts/time/versions without secrets or account identities.

Hashes detect inconsistency, not authenticity: the manifest is not signed. A snapshot includes both secret keys and must be protected as secret material. No additional archive encryption is performed; use encrypted storage/archive for external retention.

## Restore

```sh
exrd admin backup restore --from /path/to/backups/new-snapshot \
  --to /path/to/new-restored-state
```

Restore validates the source, copies into a new private directory, validates again and records nonsecret restore.json. Existing service/state/snapshot are untouched. Stop the old service before switching its db/key/oauth-key paths to the restored files. Preserve service listen/gateway/socket configuration separately; it is not in the snapshot. Do not init restored state.

Use a binary compatible with the snapshot schema, then apply normal migrations for future updates. Startup turns pending accepted/sent records into aborted_unknown, without replay. Token revocations/SSH bindings/quotas/health/catalog/context bindings reflect the snapshot moment; later changes are absent.

Offline decryption does not prove upstream still accepts old credentials. Subsequent refresh/reauth can invalidate them; perform new operator login if required. Do not run two cloned instances of one OAuth state concurrently. Restore cannot recover an open upstream WS session.

## Verification scope

Tests cover live committed WAL, transaction consistency, private modes/keys/OAuth, active and revoked bearers, quotas/health/context bindings, actual restored service startup/auth and shutdown socket cleanup. Damaged files/sidecars/symlinks/wrong keys/schema/permissions/existing destinations fail without overwriting.

On 2026-10-01 an actual private two-account fixture snapshot (37 test users, 195 usage, no pending requests) passed offline verify/restore. The temporary restore was removed; the private snapshot was retained. No OAuth refresh/network/inference was triggered. Scheduling, encrypted off-host retention and Pi power-failure recovery remain operations work.
