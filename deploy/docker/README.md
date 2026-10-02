# Docker Compose server

The API is a container; SSH management is a small host gateway using the same private Unix socket. TLS/ACME remains your ingress's responsibility. State, configuration, credentials and application image are separate. No SSH server, Docker socket or user private keys are mounted into the application.

The `credentials` helper runs once with no network. It copies the two root-owned key files into private service-owned runtime files, then exits. The `exrd` application runs as the configured unprivileged UID/GID with a read-only filesystem, dropped capabilities, bounded memory/PIDs and a private `/tmp`.

## New installation

1. Complete accounts, fresh private state, public key registration and restricted host gateway setup from [native deployment](../README.md), Stop any temporary native bootstrap service before starting the container. Use distinct service/gateway UIDs and root-owned source keys. The default template IDs are examples, not accounts it creates automatically.
2. Copy the reviewed Docker files to `/opt/exetrouter/docker`; adapt `config.json` to the actual gateway UID. Save deployment values in a root-owned `0600` `.env` beside `compose.yaml`:

```text
EXETROUTER_IMAGE=exetrouter:0.1.0
EXETROUTER_UID=10001
EXETROUTER_GID=10002
EXETROUTER_STATE=/var/lib/exetrouter
EXETROUTER_RUNTIME=/run/exetrouter
EXETROUTER_CONFIG=/etc/exetrouter/config.json
EXETROUTER_HMAC_KEY=/etc/exetrouter/exetrouter.key
EXETROUTER_OAUTH_KEY=/etc/exetrouter/exetrouter.oauth.key
```

3. Build from the repository root as an authorized Docker operator:

```sh
sudo docker buildx build --load -t exetrouter:0.1.0 -f deploy/docker/Dockerfile .
sudo install -d -m 2710 -o exetrouter -g exetrouter-gateway /run/exetrouter
sudo docker compose --project-directory /opt/exetrouter/docker \
  -f /opt/exetrouter/docker/compose.yaml config --quiet
sudo docker compose --project-directory /opt/exetrouter/docker \
  -f /opt/exetrouter/docker/compose.yaml up -d --wait
```

The default Compose bridge publishes only `127.0.0.1:8787`. Use Docker Engine 28 or newer: [older Docker versions have a localhost-publishing exposure issue](https://docs.docker.com/engine/network/port-publishing/). Adapt existing ingress without starting a competing TLS listener. For an existing legacy host that must preserve a truly loopback-only listener, the supplied `compose.host-network.yaml` override is supported; include both Compose files and use `127.0.0.1` in config (the override clears `ports`). This trades container network isolation for the existing host's loopback boundary. Do not use host networking merely to fix build egress.

4. Install the root-owned native gateway binary as `/usr/local/libexec/exetrouter/exrd`, and the reviewed `exrd-host` launcher as `/usr/local/bin/exrd`. Existing forced-command paths continue to work. The gateway branch executes only the native binary, never sudo/Docker. Do not add the gateway user to the Docker group. The launcher uses `/opt/exetrouter/docker/compose.yaml` and executes admin commands as the application's UID:

```sh
exrd admin user-list
exrd admin oauth list
exrd admin oauth add --device
```

5. Install/review `exetrouter-compose.service` to start this Compose stack on boot. Preserve the dedicated host SSH service and administrative SSH. A clean stop drains requests and removes the socket; `SIGKILL` requires operator inspection of a stale socket, never blind deletion. Docker's restart policy is secondary to the Compose unit.

## Migration and upgrades

Build and test the new image first. Keep a verified private online backup, original binary, systemd unit and config. Do not run both routers against the same SQLite/refresh credentials concurrently. Stop the native router, preserve source keys/state, create the runtime directory if its unit removed it, then start Compose with matching UIDs and paths. No re-login or schema change is necessary for packaging migration.

Verify health, service UID, read-only mounts, socket peer authentication, gateway denial of database/key reads, public unauthorized responses, `exr doctor/models/limits`, and restart persistence. Do not consume credits or generate inference during unattended migration. Roll back by stopping Compose and starting the preserved native service with the same state/keys.

Inspect app metadata logs with `docker compose logs exrd`; no payload/error-message/header logs are enabled. Existing nginx templates disable API access/error logging. Define encrypted off-host backup scheduling and retention separately; Docker volumes are not backups.

## One-command updates

Install `update-host` root-owned and executable as `/opt/exetrouter/docker/update-host` together with `exrd-host`. Use `exrd update --check` to inspect the latest release, or `exrd update` to build and apply it. The standard systemd service must be named `exetrouter.service`. The script uses the host-network override only when that override file is installed; omit it for normal bridge deployments.

The update snapshots/verifies the private state, preserves the old image/environment/native gateway and validates the restarted application. A failed start or interrupted switch restores the previous deployment. State and source keys are preserved. A release with a new migration is refused instead of silently changing the database schema. See [installation and updates](../../docs/installation.md#updates).
