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

Use `exrd admin oauth policy ID --enabled false` (or `oauth disable ID`) for reversible deactivation for all users; `oauth enable ID` restores routing without removing the legacy lock marker. Personal activation changes are no longer supported; retained user preferences never influence routing. Preserve OAuth state; this is not deletion. Priorities, switching thresholds and routine expiring-reset redemption are automatic. Schema 12 requires explicit migration/rollback review in `docs/automatic-routing-migration.md`; routine packaging updates still refuse schema changes. Starting the new runtime may consume expiring reset credits under the documented 72/24-hour rules; never spend credits deliberately for deployment testing.

`exrd admin oauth reset ACCOUNT` selects one credit by local ID or exact email when live weekly quota has at most 5% remaining. The operator must review and type `RESET ID` in a local terminal (omit `-T` with Compose). Never automate confirmation, call it through the restricted gateway or repeat an uncertain result.

Backups contain the database and both private keys. Create/verify them at a private path, protect or encrypt external copies, and restore only into a fresh destination. Verification of a snapshot does not prove that upstream credentials are currently valid.

For installation/migration inspect processes, service/Compose configuration, UIDs, file permissions, listeners and ingress before changes. Keep a rollback binary/config and a consistent private snapshot. Run offline tests and read-only API/Doctor/model checks. Live Limits/Overview can submit a weekly-activation request and require authorized inference scope. Device login and reset-credit consumption are not unattended checks. Preserve existing ingress/ports unless the user requests changing them.

Privacy: log only fixed events/statuses/timing/IDs; do not add payload/header/error-message traces, HTTP access logs, body-capturing APM or conversation inspection. Never put runtime state, keys, tokens, user content or private deployment addresses into the repository.

## Software updates

Use `exrd update --check --json` for a read-only release check. Install only within the user's requested update scope; preserve config/state/credentials. Source installations rebuild a locked published tag. Docker updates use versioned GHCR images, verified private backup, schema comparison and a matching native gateway. The optional helper adds digest pinning and health rollback. Native services need their normal restart after replacement. The restricted gateway must never gain Docker access.
