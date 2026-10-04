# Self-host an ExetRouter server

This guide targets Debian/Ubuntu with systemd, OpenSSH and nginx. The host-native service is intentional: restricted SSH and the Unix control socket authenticate actual host UIDs. Keep the router and gateway as separate unprivileged users. Other ingress/container layouts require their own UID/socket/TLS checks.

You need an API domain pointing to this host, administrative access and the first user's Ed25519 public key. The examples use `api.example.com`; replace it everywhere. They are for **new state**. Inspect existing listeners, SSH and firewall first; preserve administrative access and backups before changing a working host.

## 1. Install reviewed binaries and accounts

Follow [installation](../docs/installation.md), install both binaries into a user-owned staging prefix, then install `exrd` root-owned at `/usr/local/bin/exrd`. Do not run Cargo builds as root. Use an official checksummed release or locked tagged source.

```sh
sh scripts/install.sh --role server --from-source
sudo install -m 755 ~/.local/bin/exrd /usr/local/bin/exrd
sudo groupadd --system exetrouter-gateway
sudo useradd --system --gid exetrouter-gateway --home-dir /var/lib/exetrouter \
  --shell /usr/sbin/nologin exetrouter
sudo useradd --system --gid exetrouter-gateway --home-dir /var/empty/exetrouter \
  --shell /bin/sh routercli
sudo install -d -m 700 -o exetrouter -g exetrouter-gateway /var/lib/exetrouter
sudo install -d -m 750 -o root -g exetrouter-gateway /etc/exetrouter /var/empty/exetrouter
```

Skip/review existing accounts rather than recreating them. The gateway shell is needed to execute its forced command; SSH grants it no shell, forwarding or TTY. Keep its home and authorized_keys root-controlled. OpenSSH StrictModes requires non-writable ancestors.

## 2. Initialize private state once

```sh
sudo -u exetrouter /usr/local/bin/exrd \
  --db /var/lib/exetrouter/exetrouter.sqlite \
  --key /var/lib/exetrouter/exetrouter.key \
  --oauth-key /var/lib/exetrouter/exetrouter.oauth.key init
sudo mv /var/lib/exetrouter/exetrouter.key /etc/exetrouter/exetrouter.key
sudo mv /var/lib/exetrouter/exetrouter.oauth.key /etc/exetrouter/exetrouter.oauth.key
sudo chown root:root /etc/exetrouter/exetrouter.key /etc/exetrouter/exetrouter.oauth.key
sudo chmod 600 /etc/exetrouter/exetrouter.key /etc/exetrouter/exetrouter.oauth.key
```

Initialization refuses to overwrite existing database/keys. The two independent keys must not be disclosed or rotated during ordinary installation/upgrade. State is service-owned and private; keys are root-owned. The gateway group must never read the database or keys.

## 3. Start the loopback service

Save the paths and actual gateway UID once, then review/install the service:

```sh
sed "s/GATEWAY_UID/$(id -u routercli)/" deploy/config.json.in \
  | sudo tee /etc/exetrouter/config.json >/dev/null
sudo chmod 644 /etc/exetrouter/config.json
sudo install -m 644 deploy/systemd/exetrouter.service /etc/systemd/system/
sudo systemd-analyze verify /etc/systemd/system/exetrouter.service
sudo systemctl daemon-reload
sudo systemctl enable --now exetrouter.service
curl --fail http://127.0.0.1:8787/healthz
```

The service binds loopback only. systemd delivers keys with LoadCredential, then creates private `0600` runtime copies satisfying the binary's file checks. `/run/exetrouter` is service-owned, gateway-group `2710`; its control socket is `0660`. The runtime copies exist only while the service is active. The hardened unit restricts privileges/filesystem/network families.

## 4. Add the user, public key and OAuth account

The saved profile supplies all state/key/socket paths. Use the service UID for admin commands, preventing root-owned SQLite journal files:

```sh
sudo -u exetrouter exrd admin user-create owner
sudo install -m 644 /path/to/owner.pub /etc/exetrouter/owner.pub
sudo -u exetrouter exrd admin ssh-key-add --user-id 1 --public-key-file /etc/exetrouter/owner.pub
sudo -u exetrouter exrd admin ssh-authorized-keys --gateway-bin /usr/local/bin/exrd \
  | sudo tee /etc/exetrouter/authorized_keys >/dev/null
sudo chown root:exetrouter-gateway /etc/exetrouter/authorized_keys
sudo chmod 640 /etc/exetrouter/authorized_keys
sudo -u exetrouter exrd admin oauth add --device
```

Replace user ID `1` if it differs. The last command prints a login URL/code; the account owner signs in in their own browser. Do not ask for copied OAuth tokens. Do not run two live routers against copies of the same refresh credentials. Reuse state on restart.

## 5. Enable restricted management SSH

Keep administrative SSH separate. Review the dedicated HostKey/listen/port/UID and check the configuration **before** enabling or exposing it:

```sh
sudo install -m 644 deploy/sshd/sshd_config /etc/exetrouter/sshd_config
sudo /usr/sbin/sshd -t -f /etc/exetrouter/sshd_config
sudo /usr/sbin/sshd -T -f /etc/exetrouter/sshd_config
sudo install -m 644 deploy/systemd/exetrouter-sshd.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now exetrouter-sshd.service
sudo ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub
```

The generated authorized_keys binds each key to one registered identity and forced gateway command. Do not add a global ForceCommand. Ensure the same gateway keys are not accepted on a broader shell endpoint. Give the client the public host key and fingerprint over a trusted channel. Verify that shell, SFTP, forwarding, TTY and gateway reads of database/keys fail. Review any firewall opening for the management port; maintain default-deny and existing administrative access.

## 6. Configure TLS ingress

Provision a certificate for your chosen domain using your existing ACME/ingress workflow. Replace the example domain/certificate paths in `nginx/exetrouter.conf`, include it in nginx's `http` context, validate `nginx -t` and reload gracefully. The ordinary template owns 80/443. If your existing TCP ingress sends PROXY v2 to a loopback TLS listener, use `nginx/exetrouter-proxy-protocol.conf` instead. Do not install both blindly or start a competing public ingress.

Both templates expose `/v1/` only, disable access/error logs for that host, and disable body buffering, cache and upstream retries. Preserve WebSocket upgrades and SSE. Do not enable payload tracing or body capture in other ingress/APM components. See [privacy](../SECURITY.md). Plan certificate renewal and verify the renewal/reload path before relying on public access.

Open only the reviewed API/ACME/management ports; never expose the control socket or raw loopback API. Configure your client using [the installation guide](../docs/installation.md#configure-once).

## 7. Verify and maintain

Check service status/logs, expected listeners, actual UID/permissions, effective sshd/nginx configuration and the firewall. Verify the public endpoint returns 401 without a bearer, while `exr doctor` and `exr models` work through restricted SSH. These checks generate no inference. Check live limits only when [weekly-activation inference](../docs/cli.md#weekly-activation) is authorized. Issue a bearer in your own interactive client terminal; it is copied to the clipboard, not printed.

Use [backup/restore](../docs/backup.md) before upgrades, and retain a rollback binary/config. Use the service's existing state and keys. A verified snapshot does not guarantee that copied upstream refresh credentials are still valid. Stop old credential copies from refreshing concurrently. Do not consume reset credits during unattended verification.

Optional native-client checks in `tests/deployed.rs` require explicit `EXETROUTER_DEPLOY_HOST`, `EXETROUTER_DEPLOY_USER`, `EXETROUTER_DEPLOY_PORT`, `EXETROUTER_DEPLOY_URL` and `EXETROUTER_DEPLOY_IDENTITY`. They generate real inference and spend quota; keep them out of CI. No public host is hard-coded in the test.
