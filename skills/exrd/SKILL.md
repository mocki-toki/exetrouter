---
name: exrd
description: "Install, configure and operate an ExetRouter server: Docker or native deployment, users, SSH public keys, OAuth accounts and private backups. Use exr for client or standalone operation."
---

# ExetRouter server

Inspect `exrd --help`, `exrd admin --help` and the relevant subcommand help. Reuse the installed version, existing configuration, state and keys. Commands describe their purpose and accept `--json` for reports; OAuth login is interactive and never returns credentials.

Configuration precedence: explicit path flags override `--config PATH`, `EXRD_CONFIG`, `/etc/exetrouter/config.json`, then `$XDG_CONFIG_HOME/exrd/config.json` (default `~/.config/exrd/config.json`). Paths in saved config are absolute; keys are stored separately. Never initialize over existing state or reuse a copied refresh-token pool in two concurrently running servers.

Read the repository's `docs/server-cli.md` for commands/configuration, `deploy/docker/README.md` for Docker Compose, or `deploy/README.md` for native systemd. Docker Compose is the primary operator interface: `docker compose exec -T exrd exrd admin user-list --json`. Use an interactive `exec` for OAuth device login. An optional host `exrd` launcher executes admin commands inside the unprivileged container; its restricted gateway path uses a root-owned native binary without Docker access. Do not grant the gateway user the Docker group/socket.

Common read-only checks:

```sh
exrd admin user-list --json
exrd admin ssh-key-list --json
exrd admin oauth list --json
```

Register only the public Ed25519 key supplied for the requested user. `ssh-authorized-keys` exports forced commands; review/install it root-owned into the dedicated SSH instance. Preserve administrative SSH and verified host keys. Never disable strict host-key checks, enable passwords or expose management over HTTP.

Use `exrd admin oauth add --device` or `exrd admin oauth reauth ID --device` in the user's interactive terminal. The owner completes the browser login. Do not request copied access/refresh tokens or capture login secrets in agent output. Account disable, SSH revocation and user creation must stay within the user's task.

Use `exrd admin oauth policy ID --enabled false --locked true` (or `oauth disable ID`) for reversible deactivation for all users; `oauth enable ID` restores routing without removing the lock. `--priority 1 --locked true` enforces account priority, and `--locked false` permits personal preferences again. Locked changes are rejected in the service, not merely hidden in the dashboard. Preserve OAuth state; this is not account deletion. Schema 007 requires explicit migration/rollback review in `docs/account-preferences-migration.md`; routine packaging updates refuse schema changes.

Backups contain the database and both private keys. Create/verify them at a private path, protect or encrypt external copies, and restore only into a fresh destination. Verification of a snapshot does not prove that upstream credentials are currently valid.

For installation/migration inspect processes, service/Compose configuration, UIDs, file permissions, listeners and ingress before changes. Keep a rollback binary/config and a consistent private snapshot. Run offline tests and read-only API/Doctor/model checks. Live Limits/Overview can submit a weekly-activation request and require authorized inference scope. Device login and reset-credit consumption are not unattended checks. Preserve existing ingress/ports unless the user requests changing them.

Privacy: log only fixed events/statuses/timing/IDs; do not add payload/header/error-message traces, HTTP access logs, body-capturing APM or conversation inspection. Never put runtime state, keys, tokens, user content or private deployment addresses into the repository.

## Software updates

Use `exrd update --check --json` for a read-only release check. Install only within the user's requested update scope; preserve config/state/credentials. Source installations rebuild a locked published tag. Docker updates use versioned GHCR images, verified private backup, schema comparison and a matching native gateway. The optional helper adds digest pinning and health rollback. Native services need their normal restart after replacement. The restricted gateway must never gain Docker access.
