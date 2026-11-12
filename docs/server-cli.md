# Server commands

`exrd` hosts the model API. `exr` is its user-facing client and also has an independent standalone mode. Use `exrd --help` or subcommand `--help` for the installed version.

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

The command still needs permission to its private state. In native deployment, execute as the service UID (`sudo -u exetrouter exrd admin user-list`). Config cannot grant filesystem access. In Docker deployment use `docker compose exec -T exrd exrd admin user-list`. An optional host launcher provides the shorter `exrd admin user-list` form and uses `sudo` for Docker access if needed. The SSH gateway never gets Docker privileges.

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
| `admin oauth policy ID --enabled false` | Reversibly deactivate routing for all users |
| `admin oauth policy ID --enabled true` | Activate routing (OAuth must remain healthy) |
| `admin oauth policy ID --locked true` | Preserve the operator lock marker for older clients and rollback; personal availability changes are no longer supported |
| `admin oauth disable ID` | Reversibly deactivate routing for everyone; retain OAuth state and set the legacy lock marker |
| `admin oauth enable ID` | Reactivate routing while preserving the operator lock |
| `admin backup create --to PATH` | Snapshot live SQLite state and both keys |
| `admin backup verify --from PATH` | Verify private snapshot integrity/decryptability offline |
| `admin backup restore --from PATH --to PATH` | Restore into a fresh private directory |
| `gateway --identity ID` | Internal restricted SSH bridge; not an interactive admin command |

Reports use readable text by default and `--json` for scripts. Secrets and OAuth login are excluded from JSON. Exporting `authorized_keys` is intentionally machine-readable text. User API-token creation belongs to interactive `exr`; secrets go directly to the clipboard.

`serve --gateway-uid` is required unless saved in config. Service/gateway UIDs must differ; same-UID mode is only for local tests. Wildcard listening is rejected unless `--allow-container-listen` is explicitly selected for an isolated container with loopback-published ingress. Never expose it directly to the Internet.

Account policy controls availability and activation locks only. Priorities, switching thresholds and expiring-reset redemption are automatic; old manual routing/redemption mutations fail explicitly. Read [automatic selection](account-pool.md#ranking-and-switching) and the explicit [schema 11 to 12 migration](automatic-routing-migration.md) before deployment. Historical routing overrides remain stored only for rollback.

Fast requests are disabled by default, even when clients select Fast. Set `"fast_mode": true` in the saved server config, or use `exrd serve --fast-mode true` (and `--fast-mode false` to override a saved setting). A token owner can override either global value with `exr token fast-mode TOKEN_ID on|off|inherit`; `inherit` clears the override. `exr token show TOKEN_ID --json` exposes the nullable `fast_mode` preference, not a credential. Rotation preserves the preference. Changes take effect on the next request, including subsequent generations on an open WebSocket. Standalone defaults off and supports per-token overrides. Deploying this change upgrades schema 14 to 15; see [migration notes](fast-mode-migration.md).

`exr token disable TOKEN_ID` temporarily suspends a token; `exr token enable TOKEN_ID` restores the same secret. OpenCode Desktop Tokens actions offer the same toggle and per-token Fast overrides. Existing HTTP requests may finish, but new requests and further generations on open WebSockets must authenticate again and fail while disabled (a rejected WebSocket closes; reconnect after enabling). Revocation remains permanent. Token-list/show JSON includes `disabled`; rotation preserves it. A disabled token cannot issue new requests even if its Fast override is on.

## Update the software

`exrd update --check [--json]` reads the public release version without opening state or contacting OpenAI. `exrd update` replaces a standard native binary or delegates through the optional Docker host launcher. Native services need a restart; Docker uses snapshot/schema/gateway checks and health rollback. Updates are local operator actions, unavailable through the restricted gateway. See [update methods](installation.md#updates) and [Compose procedure](../deploy/docker/README.md#updates-with-compose).
