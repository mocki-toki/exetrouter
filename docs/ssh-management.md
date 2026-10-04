# Restricted SSH management

Review and test the target host configuration before opening access. The [Linux deployment guide](../deploy/README.md) includes installation commands and templates.

## Trust boundary

exr connects as routercli and sends only exrd-gateway. Each public Ed25519 key is registered in SQLite. Exported authorized_keys applies restrict and a per-key forced command containing the server-assigned key identity. The gateway resolves that active identity to a user from the database; the client cannot choose a user ID.

exrd runs unprivileged. Private database/keys live in a service-owned 0700 directory. routercli can access only the control socket through a dedicated group: service-owned /run/exetrouter, gateway group, mode 2710; socket mode 0660 inherits the group. serve --gateway-uid validates Unix peer UID. Do not put secret state in the gateway-readable directory. Same-UID development mode is not for deployment.

The operator controls public key registration and authorized_keys. Private keys remain on devices. Only ordinary ssh-ed25519 is supported, not hardware sk keys. Use separate device keys and load protected keys into ssh-agent.

## Operator actions

With the [saved server profile](server-cli.md#configure-paths-once) supplying database/key/socket paths:

```sh
sudo -u exetrouter exrd admin ssh-key-add --user-id 1 --public-key-file /path/to/alice.pub
sudo -u exetrouter exrd admin ssh-authorized-keys --gateway-bin /usr/local/bin/exrd
```

Review/atomically install the export into a separate /etc/exetrouter/authorized_keys: directory root:exetrouter-gateway 0750, file root:exetrouter-gateway 0640. The technical user reads but cannot edit it. The command only prints an export; it does not modify system files. ssh-key-list shows bindings; ssh-key-revoke key_ID immediately denies gateway access, even before re-export. Re-export also removes SSH authentication. Bearer tokens are separate and must be revoked separately after compromise.

## Dedicated sshd requirements

Use a separate port/service; leave administrative SSH intact. This fragment requires reviewed HostKey/PidFile/ListenAddress, firewall and service settings:

```text
Port 2222
AllowUsers routercli
PubkeyAuthentication yes
AuthenticationMethods publickey
PasswordAuthentication no
KbdInteractiveAuthentication no
AuthorizedKeysFile /etc/exetrouter/authorized_keys
PermitRootLogin no
StrictModes yes
DisableForwarding yes
PermitTTY no
PermitUserEnvironment no
PermitUserRC no
```

Do not set a global ForceCommand that overrides per-key identities. routercli needs a shell capable of executing its forced command, but no arbitrary shell/SFTP/forwarding access. Validate sshd -t and effective sshd -T with the real configuration before starting it. Binary/configuration ownership must prevent routercli modification. Ensure these keys are not accepted on another, broader SSH endpoint.

Normal client requests require a verified known_hosts entry and use StrictHostKeyChecking=yes. The interactive connection wizard checks SSH with StrictHostKeyChecking=ask before saving, allowing first-time host trust to be confirmed in the same terminal. Verify the displayed fingerprint with the operator; changed host keys are rejected. Do not trust an unverified ssh-keyscan as independent identity evidence. Save the connection with `exr configure --identity PATH`; see [client configuration](cli.md#configure-once).
