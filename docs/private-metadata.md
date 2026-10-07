# Encrypted personal metadata

ExetRouter encrypts personal metadata on the native client with XChaCha20-Poly1305. The service stores ciphertext and never receives the privacy key. This covers token names, personal account labels and dashboard preferences. It does not encrypt routing rules, account issuer identities, usage counters, or connection settings required to reach the service.

The client creates its key automatically on first use. By default it is next to the selected client config, replacing its extension with `.privacy-key`; `EXR_PRIVACY_KEY` selects another local key file. Keys must be owner-only regular 32-byte files; symlinks are rejected. The key is never uploaded. Back it up separately and privately: server backups cannot recover it, and losing every copy makes encrypted data unreadable.

New token names are encrypted automatically in CLI and TUI. Rotation preserves ciphertext. Existing plaintext names are encrypted automatically the next time the native client loads the token list; no bearer secret changes. Old backups, SQLite free pages and WAL files may still contain previous plaintext names. Operators and older clients see ciphertext. A wrong key produces a fixed error, and cannot rotate an encrypted token.

Rename a token through Tokens → Enter → Rename token or `exr tokens rename ID "New label"`. The client verifies the existing encrypted name before replacing it and encrypts the new label without changing the secret or usage history. User-scoped token usage reports carry the current stored label, including for revoked tokens; only the client decrypts it. A missing/wrong key displays `Label unavailable` in Usage instead of ciphertext or a token fragment. Other usage groupings never include token labels.

Before migrating plaintext names, the client verifies that it can decrypt every encrypted name in the list. An unavailable privacy key does not block unrelated management operations such as model/usage reports or token revocation.

Use Enter on an account in Overview and choose “Edit personal account label”. Empty values remove the corresponding item. Values are limited to 8192 bytes and labels to 256 bytes. There are at most 128 objects and 512 KiB of ciphertext per user. IDs are keyed digests; object kind, original ID and value are all encrypted. The server still sees ownership, object counts, sizes and access patterns. AEAD rejects modified ciphertext, but does not prevent a malicious server from deleting data or replaying an earlier valid object. Token-name envelopes are portable across token rotation and are not bound to a particular token ID.

Dashboard tab, usage period/group and request/token metric selection load and save automatically in encrypted form. Connection host, SSH identity path and standalone state directory remain local connection configuration.

Use the same privacy key and remote host/port/SSH username on another device. Encryption is bound to that connection tuple (standalone uses a separate domain). Changing the tuple requires decrypting/exporting with the old profile and re-encrypting with the new one. The client executable and device must be trusted; installing a modified client can expose the key.

## Schema 9 to 10 and rollback

Migration 010 adds only `private_metadata`; existing keys, OAuth state, routing, accounting and identities remain intact. Existing token names are encrypted by the native client when it next loads the token list. Update server and native SSH gateway together; older services do not support the new control actions. Deployment requires inspection of the actual host and a verified schema-9 snapshot before applying the migration. This is not a routine one-click Docker update.

For rollback, stop the service and preserve a verified schema-10 snapshot plus every client privacy key. Older clients cannot display encrypted token names. Before downgrading, use a reviewed trusted client to decrypt/relabel token names if plaintext storage is acceptable. Otherwise retain their ciphertext. In a reviewed transaction, export encrypted private objects for later restoration, drop only `private_metadata`, set `PRAGMA user_version=9`, and verify foreign keys and database integrity. Restart the matching schema-9 server/gateway. Restoring an older database instead can lose newer accounting and OAuth refresh state; do not do that casually.
