# Installation

Choose a role: **client** runs `exr` standalone or connects to an existing router; **server** hosts `exrd` and an OAuth pool. The supported systems are macOS and Linux (x86_64/ARM64). Windows/WSL gateway deployment is not verified. Never initialize or overwrite an existing state directory during an upgrade.

## Existing server: client

You need the server's SSH host/port/user, your protected Ed25519 private key, operator key registration and an independently verified host-key fingerprint. The client defaults to localhost, port 2222 and user routercli; it does not select a third-party deployment.

### Homebrew on macOS

Install the client from the project's tap. Homebrew downloads the release binary and verifies the formula's SHA-256 checksum; Rust, Cargo and a compiler are not needed:

```sh
brew tap mocki-toki/exetrouter https://github.com/mocki-toki/exetrouter
brew install mocki-toki/exetrouter/exr
exr
```

The formula installs only `exr`, supports Apple Silicon and Intel Macs, and does not modify connection settings, accounts or SSH. Update with `brew upgrade mocki-toki/exetrouter/exr`, then reopen the dashboard. The built-in updater detects Homebrew's receipt and directs you to Homebrew rather than replacing a managed binary.

### Release binaries on Linux or macOS

Without Homebrew, download and review the installer, then run it. The default path downloads a ready-made binary; source compilation is opt-in:

```sh
curl --fail --location --proto '=https' --tlsv1.2 \
  https://raw.githubusercontent.com/mocki-toki/exetrouter/main/scripts/install.sh -o /tmp/exetrouter-install.sh
less /tmp/exetrouter-install.sh
sh /tmp/exetrouter-install.sh --role client
```

For a fixed reviewed version, replace `main` in the URL with its `vMAJOR.MINOR.PATCH` tag and pass the same tag as `--version`. Only curl, tar and a SHA-256 utility are required for release installation.

Linux release binaries may require a newer glibc than the target system provides. For older systems, use the locked source installation below; the Docker image builds against Debian 12. The installer checks that both selected binaries run before replacing anything.

The installer detects OS/architecture, downloads the matching public GitHub release and verifies its SHA-256 checksum before extracting/installing. Default destination: `~/.local/bin`. It does not change shell profiles, SSH, system services or state. Add that directory to PATH if needed; no alias is required. `--prefix /your/root` changes the installation root; `--version v0.1.0` selects a specific release. Checksums establish release integrity, not independent trust in the publisher. The server role installs `exrd`; `--role both` installs both binaries.

### Nix (Linux or macOS)

Enable Nix's `nix-command` and `flakes` experimental features. The flake builds from source using the committed `Cargo.lock` and pins the compiler/build dependencies in `flake.lock`. It exports `packages.<system>.exetrouter` (also `default`), apps for `exr` (also `default`) and `exrd`, a package check and a development shell. Outputs are provided for x86_64/ARM64 Linux and Apple Silicon macOS.

Try the CLI without installing it, or add both binaries to your user profile:

```sh
nix run github:mocki-toki/exetrouter -- --help
nix run github:mocki-toki/exetrouter#exrd -- --help
nix profile add github:mocki-toki/exetrouter#exetrouter
exr --version
exrd --version
```

These commands follow the repository's default branch. For a reproducible installation, replace `github:mocki-toki/exetrouter` with `github:mocki-toki/exetrouter/COMMIT_SHA`, using a reviewed commit that contains the flake. Older release tags without a flake cannot be used with these commands. The first source build can take several minutes; no separate Rust installation is needed.

To build and check a reviewed local checkout without installing it:

```sh
nix build .#exetrouter
./result/bin/exr --version
./result/bin/exrd --version
nix flake check
```

The package check builds the package; it does not run the Rust test suite. Tests are disabled in the build sandbox because upstream CLI fixtures assume host timezone data. Run the standard tests separately in the development shell below; live tests stay opt-in. The client wrapper supplies OpenSSH and, on Linux, `wl-copy` and `xclip` for desktop clipboard operations. Clipboard access still requires a running desktop session. On macOS, the client uses the system `pbcopy`.

For declarative installation, add this input to your NixOS or Home Manager flake:

```nix
inputs.exetrouter.url = "github:mocki-toki/exetrouter";
```

Pass `inputs` into your configuration module (for example with NixOS `specialArgs = { inherit inputs; };` or Home Manager `extraSpecialArgs = { inherit inputs; };`), then choose the appropriate package list:

```nix
{ inputs, pkgs, ... }: {
  environment.systemPackages = [
    inputs.exetrouter.packages.${pkgs.stdenv.hostPlatform.system}.exetrouter
  ];
}
```

For Home Manager, use `home.packages` instead of `environment.systemPackages`. Keep the input's own pinned nixpkgs for its toolchain. Commit your configuration's lock file, then rebuild with your usual NixOS/Home Manager command. Installing the package does not initialize state, configure accounts, create services or open ports. For personal use run `exr configure --standalone`, then `exr` or `exr serve`; shared-server deployment still requires the setup described below.

Upgrade profile installations using `nix profile list` to find the entry's name, followed by `nix profile upgrade NAME`. A commit-pinned profile must be replaced with the newly reviewed revision. For declarative installations, run `nix flake update exetrouter` in your configuration repository and rebuild. Use Nix to upgrade, rather than `exr update` or `exrd update`: the Nix store is immutable. Existing user configuration, account state and credentials stay outside the store and are preserved. Restart the dashboard/service after upgrading; review state-schema compatibility before rolling back.

For development and the repository's standard checks:

```sh
nix develop
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
python3 scripts/check-publication.py
```

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

Follow [Docker Compose installation](../deploy/docker/README.md). It retains restricted host SSH and puts the API in an unprivileged container, with separate state/config/credential mounts. Use `docker compose exec exrd exrd admin ...` for operator commands. The host launcher is an optional compatibility convenience; the native restricted SSH gateway is a separate component. [Server command reference](server-cli.md) explains every operation.

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

On a Docker host, use the [Compose update procedure](../deploy/docker/README.md#updates-with-compose): verify a private snapshot, compare schema versions, select a published GHCR image, update the separate native gateway, and apply it with `docker compose pull` and `docker compose up -d --wait`. Official images starting with 0.2.0 support Linux AMD64/ARM64; the server does not need Rust or an on-host build. An optional root-owned `update-host` helper automates digest pinning, version/schema checks, backup and health rollback through the existing systemd Compose service. Existing `exrd-host` launchers may keep `exrd update` as a convenience. Install the reviewed new helpers once when migrating older source-building helpers. No Docker access is granted to the restricted SSH gateway.

Native `exrd update` atomically replaces a standard `PREFIX/bin/exrd` installation; restart your native service afterward, using the deployment's normal backup/restart procedure. For a binary in a custom managed path, use the same installer/deployment method that placed it there. Homebrew installations use `brew upgrade mocki-toki/exetrouter/exr`; the built-in updater preserves that ownership. Other package-manager-controlled installations should be upgraded through their package manager.

For an older binary without `update`, run the reviewed installer again with the **same prefix**. A shared-server client's Settings updates its own `exr`, not the operator's server.
