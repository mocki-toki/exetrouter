# Docker Compose server

Docker Compose is the primary operator interface. Releases starting with 0.2.0 publish `ghcr.io/mocki-toki/exetrouter:VERSION` for Linux AMD64 and ARM64. Docker selects the host architecture. Use a fixed release version or the `CONTAINER_IMAGE` digest attached to the GitHub release; there is no moving `latest` tag.

The API runs in an unprivileged container. A separate, restricted host OpenSSH instance runs a native gateway, which forwards management requests through a private Unix socket. The gateway has no Docker, database or credential access. TLS/ACME remains your ingress's responsibility.

This guide targets Debian/Ubuntu with Docker Engine 28+, Compose v2, systemd and OpenSSH. Docker's [older localhost port-publishing behavior](https://docs.docker.com/engine/network/port-publishing/) is unsuitable for this loopback boundary. Inspect existing ports, SSH, users and ingress before installation. Examples are for new state; never initialize over an existing deployment.

## 1. Prepare the host and versioned files

Choose a published release, review its files, and copy `compose.yaml` and `prepare-credentials.sh` from that tag into `/opt/exetrouter/docker`. Do not install the host-network override on a new deployment. Create separate host service and gateway accounts:

```sh
sudo groupadd --system exetrouter-gateway
sudo useradd --system --gid exetrouter-gateway --home-dir /var/lib/exetrouter \
  --shell /usr/sbin/nologin exetrouter
sudo useradd --system --gid exetrouter-gateway --home-dir /var/empty/exetrouter \
  --shell /bin/sh routercli
sudo install -d -m 700 -o exetrouter -g exetrouter-gateway /var/lib/exetrouter
sudo install -d -m 750 -o root -g exetrouter-gateway /etc/exetrouter /var/empty/exetrouter
sudo install -d -m 755 /opt/exetrouter/docker
sudo install -m 644 deploy/docker/compose.yaml deploy/docker/prepare-credentials.sh /opt/exetrouter/docker/
```

Reuse/review existing accounts instead of recreating them. The gateway's shell only executes the forced command; it does not grant interactive access. Keep administrative SSH separate and never add `routercli` to the Docker group.

Save a root-owned `0600` `/opt/exetrouter/docker/.env` with `sudoedit`. Replace UID/GID values with `id -u exetrouter` and `getent group exetrouter-gateway`; these numbers must match the host ownership:

```text
EXETROUTER_IMAGE=ghcr.io/mocki-toki/exetrouter:0.2.0
EXETROUTER_UID=10001
EXETROUTER_GID=10002
EXETROUTER_STATE=/var/lib/exetrouter
EXETROUTER_RUNTIME=/run/exetrouter
EXETROUTER_CONFIG=/etc/exetrouter/config.json
EXETROUTER_HMAC_KEY=/etc/exetrouter/exetrouter.key
EXETROUTER_OAUTH_KEY=/etc/exetrouter/exetrouter.oauth.key
```

Save `/etc/exetrouter/config.json` root-owned, mode `0644`. Use the actual `id -u routercli` as `gateway_uid`:

```json
{
  "db": "/var/lib/exetrouter/exetrouter.sqlite",
  "key": "/run/exetrouter/hmac.key",
  "oauth_key": "/run/exetrouter/oauth.key",
  "control_socket": "/run/exetrouter/control.sock",
  "gateway_uid": 10003,
  "listen": "0.0.0.0:8787"
}
```

The application listens on the container network; Compose publishes it only at host `127.0.0.1:8787`.

## 2. Initialize new state using the container

Pull the chosen version. Bootstrap creates the database and keys in the private state mount without starting a server or using the network:

```sh
sudo docker pull ghcr.io/mocki-toki/exetrouter:0.2.0
sudo docker run --rm --network none \
  --user "$(id -u exetrouter):$(id -g exetrouter)" \
  --mount type=bind,src=/var/lib/exetrouter,dst=/var/lib/exetrouter \
  ghcr.io/mocki-toki/exetrouter:0.2.0 \
  --db /var/lib/exetrouter/exetrouter.sqlite \
  --key /var/lib/exetrouter/exetrouter.key \
  --oauth-key /var/lib/exetrouter/exetrouter.oauth.key init
sudo mv /var/lib/exetrouter/exetrouter.key /etc/exetrouter/exetrouter.key
sudo mv /var/lib/exetrouter/exetrouter.oauth.key /etc/exetrouter/exetrouter.oauth.key
sudo chown root:root /etc/exetrouter/exetrouter.key /etc/exetrouter/exetrouter.oauth.key
sudo chmod 600 /etc/exetrouter/exetrouter.key /etc/exetrouter/exetrouter.oauth.key
```

The `credentials` Compose service runs once as root with no network. It copies the two keys to service-owned `0600` runtime files, then exits. The main service has a read-only filesystem, dropped capabilities and bounded memory/PIDs. State and keys live on the host and survive image replacement.

## 3. Start Compose and operate the server

```sh
cd /opt/exetrouter/docker
sudo docker compose config --quiet
sudo docker compose pull
sudo docker compose up -d --wait
sudo docker compose ps
curl --fail http://127.0.0.1:8787/healthz
sudo docker compose exec -T exrd exrd admin user-create owner
sudo docker compose exec -T exrd exrd admin user-list
```

Register the first user's public key without mounting private keys. Write only the public key into a service-owned private temporary file, then register it:

```sh
sudo docker compose exec -T exrd sh -c 'umask 077; cat > /tmp/owner.pub' < /path/to/owner.pub
sudo docker compose exec -T exrd exrd admin ssh-key-add --user-id 1 --public-key-file /tmp/owner.pub
sudo docker compose exec -T exrd rm /tmp/owner.pub
sudo docker compose exec exrd exrd admin oauth add --device
```

Replace user ID `1` with the returned ID. OAuth login uses your interactive terminal; complete the browser flow yourself. Never copy OAuth tokens into commands or logs. Do not run two routers with copies of the same refresh credentials.

Inspect the running server:

```sh
sudo docker compose exec -T exrd exrd admin user-list --json
sudo docker compose logs --tail 100 exrd
sudo docker compose ps
```

## 4. Install the separate host SSH gateway

Extract the gateway from the **same image**. This does not start another server:

```sh
sudo install -d -m 755 /usr/local/libexec/exetrouter
gateway_container=$(sudo docker create ghcr.io/mocki-toki/exetrouter:0.2.0)
sudo docker cp "$gateway_container":/usr/local/bin/exrd /usr/local/libexec/exetrouter/exrd
sudo docker rm "$gateway_container"
sudo chown root:root /usr/local/libexec/exetrouter/exrd
sudo chmod 755 /usr/local/libexec/exetrouter/exrd
/usr/local/libexec/exetrouter/exrd --version
sudo docker compose exec -T exrd exrd admin ssh-authorized-keys \
  --gateway-bin /usr/local/libexec/exetrouter/exrd \
  | sudo tee /etc/exetrouter/authorized_keys >/dev/null
sudo chown root:exetrouter-gateway /etc/exetrouter/authorized_keys
sudo chmod 640 /etc/exetrouter/authorized_keys
```

Keep the socket path in the exported forced commands consistent with the mounted runtime path. Review/install `deploy/sshd/sshd_config` at `/etc/exetrouter/sshd_config` and `deploy/systemd/exetrouter-sshd.service` in `/etc/systemd/system/`. Validate `sshd -t -f /etc/exetrouter/sshd_config` before enabling the dedicated service. See [native guide steps 5–6](../README.md#5-enable-restricted-management-ssh) for SSH host-key verification and TLS ingress. Native bootstrap and a native API service are not needed for this Docker installation.

Only `/v1/` goes through your reviewed TLS ingress. Preserve existing administrative SSH, certificate renewal, SSE/WS forwarding and disabled payload logging. Verify UID isolation, socket permissions, denied gateway access to database/keys, and public unauthorized API responses without inference.

## 5. Boot management

Docker's `restart: unless-stopped` restarts the application. For unattended boot with ephemeral runtime keys, install the `exetrouter-compose.service` as `exetrouter.service`, then run `systemctl daemon-reload` and `systemctl enable exetrouter.service`. Its start command is Compose `up -d --wait`; its stop command gracefully stops the stack. Do not enable a competing native API service.

For existing installations using `compose.host-network.yaml`, preserve it and include **both** files in every command:

```sh
sudo docker compose -f compose.yaml -f compose.host-network.yaml ps
```

The override requires `127.0.0.1` in the application config and clears published ports. It preserves a legacy host's loopback boundary at the cost of container network isolation; it is not the default installation.

## Updates with Compose

Read the target release notes and [schema/rollback guidance](../../docs/backup.md). Keep the same UID/GID, state, keys, SSH host keys and custom Compose/ingress settings. The following is a planned operator update; do not silently skip the schema comparison.

1. Create and verify a private snapshot with the **current** container:

   ```sh
   cd /opt/exetrouter/docker
   sudo docker compose exec -T exrd sh -c 'mkdir -p /var/lib/exetrouter/backups; chmod 700 /var/lib/exetrouter/backups'
   sudo docker compose exec -T exrd exrd admin backup create --to /var/lib/exetrouter/backups/pre-update
   sudo docker compose exec -T exrd exrd admin backup verify --from /var/lib/exetrouter/backups/pre-update --json
   ```

   Use a fresh backup path for every update. The report contains `snapshot.schema_version`, not secrets. Define encrypted off-host backup retention separately.

2. Pull the selected image without changing the running container. Compare its `io.exetrouter.schema-version` label with the snapshot's schema version. A mismatch requires a separate reviewed migration/rollback plan:

   ```sh
   sudo docker pull ghcr.io/mocki-toki/exetrouter:VERSION
   sudo docker image inspect --format '{{index .Config.Labels "io.exetrouter.schema-version"}}' ghcr.io/mocki-toki/exetrouter:VERSION
   ```

3. Preserve the old `.env` and native gateway in a root-owned private rollback directory. Extract and check the new gateway as in step 4 above, using the chosen version and a staging destination. Stop the dedicated management SSH service while replacing it; existing administrative SSH stays available.

4. Set `EXETROUTER_IMAGE` in `.env` to the chosen version or release digest with `sudoedit`. Atomically replace the gateway using `install` to a temporary sibling and `mv`. Apply the image through Compose:

   ```sh
   sudo docker compose pull
   sudo docker compose up -d --wait
   sudo systemctl start exetrouter-sshd.service
   sudo docker compose ps
   curl --fail http://127.0.0.1:8787/healthz
   ```

   Container and gateway must use the same release. Verify restricted management with `exr doctor`; no inference or reset credit is required. Updating interrupts active requests, so choose an appropriate maintenance time.

5. If startup/health fails with an unchanged schema, restore the old `.env` and gateway, then run Compose `up -d --wait` again and start the dedicated SSH service. Do not blindly run an older binary against a newer database. Keep the snapshot and rollback files until validation is complete. Never use `down -v` as an update step.

## Optional operator helper and old installations

`deploy/docker/update-host` automates the same procedure: it pulls the release image, pins its digest, checks the binary version and live snapshot schema, preserves rollback files, replaces the gateway and invokes the existing systemd Compose service. It does not compile Rust on the server. It requires the standard `exetrouter.service`, Python 3 and the documented host paths.

Existing hosts may keep `/usr/local/bin/exrd` as the optional `exrd-host` launcher. Its `admin` branch calls Compose `exec`; its `gateway` branch runs only the native gateway, never sudo/Docker. `exrd update` delegates to `update-host`. The launcher is optional for new installations.

To install the helper, review and install `update-host` at `/opt/exetrouter/docker/update-host` and `exrd-host` at `/usr/local/bin/exrd`, root-owned and mode `0755`. Preserve existing launchers and forced-command paths during migration. Older helpers build locally; install these reviewed versioned helper files once to switch to registry updates. Routine helper updates keep custom Compose files unchanged.

## Building from source

For contributors or private images, build from a reviewed checkout:

```sh
sudo docker buildx build --load --build-arg EXETROUTER_VERSION=0.2.0 \
  --tag exetrouter:0.2.0 -f deploy/docker/Dockerfile .
```

Set `.env` to the local image and use Compose. The optional registry updater is for official images; manage private/source deployments through your own reviewed build and Compose procedure.
