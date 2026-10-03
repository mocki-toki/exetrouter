# Server commands

`exrd` hosts the model API. `exr` is its user-facing client and also has an independent standalone mode. Use `exrd --help`, `exrd admin --help` or deeper `--help`; every command describes why it exists.

## Configure paths once

Save `/etc/exetrouter/config.json` (or `~/.config/exrd/config.json` for a user-owned installation):

```json
{
  "db": "/var/lib/exetrouter/exetrouter.sqlite",
  "key": "/run/exetrouter/hmac.key",
  "oauth_key": "/run/exetrouter/oauth.key",
  "control_socket": "/run/exetrouter/control.sock",
  "gateway_uid": 10003,
  "listen": "127.0.0.1:8787"
}
```

Replace paths and gateway UID with your installation. Only paths/settings belong here; private key material stays in protected files. Saved paths must be absolute. Config must be a regular file, not writable by group or others. CLI path flags override saved settings. `--config PATH` or `EXRD_CONFIG` selects another profile; explicit profiles must exist.

Then commands are short:

```sh
exrd admin user-list
exrd admin oauth list
exrd serve
```

The command still needs permission to its private state. In native deployment, execute as the service UID (`sudo -u exetrouter exrd admin user-list`). Config cannot grant filesystem access. In Docker deployment use `docker compose exec -T exrd exrd admin user-list`. An optional host launcher provides the shorter `exrd admin user-list` form and uses `sudo` for Docker access if needed. It is an executable, not a shell alias. The SSH gateway never gets Docker privileges.

Without saved config, local development defaults to files in the working directory. `init` refuses to overwrite existing state. In deployed services, runtime keys are delivered privately by systemd or the Compose credentials helper.

## Commands and purpose

| Command | Purpose |
| --- | --- |
| `init` | Initialize fresh private database and independent bearer/OAuth keys |
| `serve` | Run loopback model API and authenticated Unix management socket |
| `admin user-create NAME` | Create a user scope for API tokens and public SSH keys |
| `admin user-list` | List local users/IDs |
| `admin ssh-key-add --user-id ID --public-key-file PATH` | Register an Ed25519 public key for one user |
| `admin ssh-key-list` | Inspect public key bindings |
| `admin ssh-key-revoke ID` | Immediately deny this management identity |
| `admin ssh-authorized-keys --gateway-bin PATH` | Export restricted forced-command lines for dedicated sshd |
| `admin oauth add --device` | Browser sign-in; save encrypted upstream credentials |
| `admin oauth reauth ID --device` | Reauthorize the same upstream account |
| `admin oauth list` | Show account email and state without tokens (`email` is null when unavailable in the saved credentials) |
| `admin oauth policy ID --enabled false --locked true` | Reversibly deactivate routing for all users and prevent client changes |
| `admin oauth policy ID --enabled true --priority 1 --locked true` | Activate routing and enforce priority for everyone (OAuth must remain healthy) |
| `admin oauth policy ID --locked false` | Unlock personal preference changes, preserving existing preferences |
| `admin oauth disable ID` | Reversibly deactivate routing for everyone and lock client changes; retain OAuth state |
| `admin oauth enable ID` | Reactivate routing while preserving the operator lock |
| `admin backup create --to PATH` | Snapshot live SQLite state and both keys |
| `admin backup verify --from PATH` | Verify private snapshot integrity/decryptability offline |
| `admin backup restore --from PATH --to PATH` | Restore into a fresh private directory |
| `gateway --identity ID` | Internal restricted SSH bridge; not an interactive admin command |

Reports use readable text by default and `--json` for scripts. Secrets and OAuth login are excluded from JSON. Exporting `authorized_keys` is intentionally machine-readable text. User API-token creation belongs to interactive `exr`; secrets go directly to the clipboard.

`serve --gateway-uid` is required unless saved in config. Service/gateway UIDs must differ; same-UID mode is only for local tests. Wildcard listening is rejected unless `--allow-container-listen` is explicitly selected for an isolated container with loopback-published ingress. Never expose it directly to the Internet.

## Update the software

`exrd update --check` reports the latest published version without opening state or contacting OpenAI; add `--json` for automation. `exrd update` upgrades an installed standard binary, or invokes the root-owned Docker host updater through the host launcher. The optional Docker helper snapshots state, pulls and pins a versioned GHCR image, verifies the live schema and matching gateway, and validates/reverts the service switch. Compose is the primary operator interface. A native binary replacement requires the usual service restart afterward. This operator action is unavailable through the restricted user gateway. See [updates](installation.md#updates).
