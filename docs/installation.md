# Installation

Choose a role: **client** runs `exr` standalone or connects to an existing router; **server** hosts `exrd` and an OAuth pool. The supported systems are macOS and Linux (x86_64/ARM64). Windows/WSL gateway deployment is not verified. Never initialize or overwrite an existing state directory during an upgrade.

## Existing server: client

You need the server's SSH host/port/user, your protected Ed25519 private key, operator key registration and an independently verified host-key fingerprint. The client defaults to localhost, port 2222 and user routercli; it does not select a third-party deployment.

### Release binaries

When a GitHub release is available, clone the repository, review its small installer and run it:

```sh
git clone https://github.com/mocki-toki/exetrouter.git
cd exetrouter
less scripts/install.sh
sh scripts/install.sh --role client
```

Linux release binaries may require a newer glibc than the target system provides. For older systems, use the locked source installation below; the Docker image builds against Debian 12. The installer checks that both selected binaries run before replacing anything.

The installer detects OS/architecture, downloads the matching public GitHub release and verifies its SHA-256 checksum before extracting/installing. Default destination: `~/.local/bin`. It does not change shell profiles, SSH, system services or state. Add that directory to PATH if needed; no alias is required. `--prefix /your/root` changes the installation root; `--version v0.1.0` selects a specific release. Checksums establish release integrity, not independent trust in the publisher. The server role installs `exrd`; `--role both` installs both binaries.

### Locked source build

If no release exists yet, use Rust 1.88 or newer and a C compiler:

```sh
cargo install --locked --git https://github.com/mocki-toki/exetrouter --bin exr
# Or, from the reviewed clone:
sh scripts/install.sh --from-source --role client
```

`cargo install` uses `~/.cargo/bin`; the script uses `~/.local/bin`. To build a tagged version directly, add `--tag v0.1.0` to the Cargo command. The installer source mode builds the selected tag when `--version` is supplied, otherwise main.

Debian/Ubuntu build prerequisites: `sudo apt-get install build-essential pkg-config ca-certificates curl git`. macOS needs Xcode Command Line Tools (`xcode-select --install`). If Rust is missing, follow the [official Rust installation guide](https://www.rust-lang.org/tools/install); inspect any downloaded installer before executing it. You do not need Node, Python, OpenAI Platform keys or a SQLite server to run the client. Python is needed only for development/audit tooling.

### Configure once

```sh
ssh-add ~/.ssh/exetrouter_ed25519  # If your private key has a passphrase
exr configure --host api.example.com --port 2222 --ssh-user routercli \
  --identity ~/.ssh/exetrouter_ed25519
exr doctor
exr models
exr limits
exr
```

Replace the domain. Get the public SSH host key/fingerprint from the operator, verify it independently, and add the matching `[api.example.com]:2222` entry to known_hosts. An unverified `ssh-keyscan` is not proof of identity; never use StrictHostKeyChecking=no. Only the public user key is shared with the operator. The saved config contains a key path, not a private key or bearer token.

In your interactive desktop terminal run `exr token create --name laptop`; its secret is copied to the system clipboard without being displayed. Paste it into your service's secret store, then configure the service's OpenAI base URL to the router's `/v1`. Do not use an agent/headless session to print or capture that secret. See [SDK/client configuration](compatibility.md) and [CLI/TUI](cli.md).

## New server

Install `exrd` using `sh scripts/install.sh --role server`, or build/install locked source with `--from-source --role server`. Server installation also requires systemd/OpenSSH and a TLS reverse proxy; follow the concrete [Debian/Ubuntu server guide](../deploy/README.md). Binary installation alone does not create users, initialize state, open ports or set up OAuth.

The operator supplies ChatGPT OAuth using device login. Platform API keys are not a replacement. The human completes the login in their browser; neither a client nor an agent should ask for pasted OAuth tokens. Configure clients only after verifying the API/SSH boundary. Real inference consumes subscription quota; model discovery, limits and health checks do not generate inference.

## Upgrade and rollback

Back up and verify private state with the documented [backup procedure](backup.md). Keep the old binary/version, install the new binary atomically, and use systemd's graceful restart. Do not replace or rotate keys just to upgrade. Review schema/version compatibility before rolling back; an older binary refuses a newer schema. Preserve current connection settings. Restart an already open `exr` after replacing its binary.

## Agent installation

Copy [the client prompt](../prompts/install-client.md) for an existing server, or [the server prompt](../prompts/install-server.md) for self-hosting. Fill in the connection/domain/public-key details before giving it to an agent. OAuth login and clipboard token issuance require the user's interaction. Install the bundled [exr skill](../skills/exr/SKILL.md) in your agent's local skills directory; it covers all management commands and mutation/secret handling.

## Standalone: no server or SSH

Install only `exr`, run it in a terminal, choose Standalone in the first-run wizard, review the private state path/local port and save. In Settings press Enter and choose Add account to sign in through the browser. In Tokens create/copy a local API bearer. Use the endpoint shown in Settings; keep the TUI open or use `exr serve` in a foreground service.

For scripted configuration: `exr configure --standalone`. Defaults are `~/.local/share/exr` (or `$XDG_DATA_HOME/exr`) and `127.0.0.1:8787`. `--state-dir PATH` and `--listen ADDRESS` override them. API listening is loopback only. Existing remote config remains valid; Settings can change modes. Account add/reauth and credit consumption still require human interaction.

## Docker server

Follow [Docker Compose installation](../deploy/docker/README.md). It retains restricted host SSH and puts the API in an unprivileged container, with separate state/config/credential mounts. The host launcher provides `exrd admin user-list` without repeated path flags or aliases. [Server command reference](server-cli.md) explains every operation.

Install the separate server skill by copying `skills/exrd` into your agent's skill directory; `skills/exr` handles standalone/remote client use.

Opening exr while exr serve is already running attaches the dashboard to that local profile; closing the attached dashboard does not stop the API. Stop the headless API before changing its listen address.

## Updates

`exr` checks the latest public GitHub release in the background when the dashboard opens. The top-right header shows the client version; an available update highlights **Settings ↑**. `v` checks again; Enter opens Settings actions, where Update exr opens the update confirmation. Your existing installation is updated in place; quit and reopen the dashboard afterward. Set `EXR_NO_UPDATE_CHECK=1` to disable the automatic metadata request. Checks send only an application user agent, with no router credentials, accounts or usage data.

```sh
exr update --check          # Read-only version check
exr update --check --json   # For agents
exr update                 # Install the latest release
exrd update --check         # Server operator: read-only check
exrd update                 # Server operator: update the installed server
```

Release installations reuse the checksummed binary installer and the existing prefix. Cargo installations are recognized through Cargo's package receipt and updated from the published tag using a locked source build; Rust and a C compiler are required for that method. Settings, account state, keys and tokens stay in their existing locations. Source builds can take several minutes. `update` never silently installs an older version.

On a Docker host, install the root-owned `deploy/docker/update-host` beside `compose.yaml`. The `exrd` launcher builds the latest published tag, creates/verifies a private backup, updates the application image and native SSH gateway, then restarts and checks the existing service. Failed health validation restores the previous image/gateway. Updates that introduce another database migration are refused for a planned operator upgrade. Docker/Buildx, curl and Python 3 must be available on the host. No Docker access is granted to the restricted SSH gateway.

Native `exrd update` atomically replaces a standard `PREFIX/bin/exrd` installation; restart your native service afterward, using the deployment's normal backup/restart procedure. For a binary in a custom managed path, use the same installer/deployment method that placed it there. Package-manager-controlled installations should be upgraded through their package manager.

Versions before 0.2.0 do not have a built-in update command: run the reviewed installer again with the **same prefix** to upgrade once; later releases support `update` directly. A shared-server client's Settings updates its own `exr`, not the operator's server.
