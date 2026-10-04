# Installation

Choose a role: **client** runs `exr` standalone or connects to an existing router; **server** hosts `exrd` and an OAuth pool. The supported systems are macOS and Linux (x86_64/ARM64). Windows/WSL gateway deployment is not verified. Never initialize or overwrite an existing state directory during an upgrade.

## Install the client

Install `exr` for either standalone use or an existing shared server.

### Homebrew on macOS

Install the client from the project's tap. Homebrew downloads the release binary and verifies the formula's SHA-256 checksum; Rust, Cargo and a compiler are not needed:

```sh
brew tap mocki-toki/exetrouter https://github.com/mocki-toki/exetrouter
brew install mocki-toki/exetrouter/exr
exr
```

The formula installs only `exr` on Apple Silicon/Intel Macs. It preserves connection settings, accounts and SSH. Upgrade through Homebrew; see [updates](#updates).

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

The installer selects the OS/architecture and verifies the release checksum before installation. It defaults to `~/.local/bin`; add it to `PATH` if needed. `--prefix /your/root` changes the root, `--version v0.1.0` pins a release, and `--role server`/`both` installs `exrd`/both binaries. Shell profiles, SSH, services and state stay unchanged. Checksums verify integrity, not independent publisher trust.

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

To build from source, use Rust 1.88 or newer and a C compiler:

```sh
cargo install --locked --git https://github.com/mocki-toki/exetrouter --bin exr
# Or, from the reviewed clone:
sh scripts/install.sh --from-source --role client
```

`cargo install` uses `~/.cargo/bin`; the script uses `~/.local/bin`. To build a tagged version directly, add `--tag v0.1.0` to the Cargo command. The installer source mode builds the selected tag when `--version` is supplied, otherwise main.

Debian/Ubuntu build prerequisites: `sudo apt-get install build-essential pkg-config ca-certificates curl git`. macOS needs Xcode Command Line Tools (`xcode-select --install`). If Rust is missing, follow the [official Rust installation guide](https://www.rust-lang.org/tools/install); inspect any downloaded installer before executing it. You do not need Node, Python, OpenAI Platform keys or a SQLite server to run the client. Python is needed only for development/audit tooling.

## Standalone: no server or SSH

Install only `exr`, run it in a terminal, choose Standalone in the first-run wizard, review the private state path/local port and save. In Settings press Enter and choose Add account to sign in through the browser. In Tokens create/copy a local API bearer. Use the endpoint shown in Settings; keep the TUI open or use `exr serve` in a foreground service.

For scripted configuration: `exr configure --standalone`. Defaults are `~/.local/share/exr` (or `$XDG_DATA_HOME/exr`) and `127.0.0.1:8787`. `--state-dir PATH` and `--listen ADDRESS` override them. API listening is loopback only. Existing remote config remains valid; Settings can change modes. Account add/reauth and credit consumption still require human interaction.

Opening `exr` while `exr serve` owns the profile attaches the dashboard to that API. Closing the attached dashboard leaves it running. Stop the headless API before changing its listen address.

## Configure once

For a shared server, obtain its SSH host/port/user, API URL, public host-key fingerprint and registration of your public Ed25519 key. Keep the private key on your device with owner-only permissions.

```sh
ssh-add ~/.ssh/exetrouter_ed25519  # If your private key has a passphrase
exr configure --host api.example.com --port 2222 --ssh-user routercli \
  --identity ~/.ssh/exetrouter_ed25519
exr doctor
exr models
exr
```

Replace the domain. Get the public SSH host key/fingerprint from the operator, verify it independently, and confirm the matching key when the interactive wizard checks SSH in the same terminal. For scripted configuration, add the matching `[api.example.com]:2222` entry to known_hosts. An unverified `ssh-keyscan` is not proof of identity; never use StrictHostKeyChecking=no. Only the public user key is shared with the operator. The saved config contains a key path, not a private key or bearer token.

In your interactive desktop terminal run `exr token create --name laptop`; its secret is copied to the system clipboard without being displayed. Paste it into your service's secret store, then configure the service's OpenAI base URL to the router's `/v1`. Do not use an agent/headless session to print or capture that secret. See [SDK/client configuration](compatibility.md) and [CLI/TUI](cli.md).

## New server

Follow [Remote Server setup](remote-server.md) for Docker/native installation guides and the agent prompt. Both deployments require restricted OpenSSH management and reviewed TLS ingress. Binary installation alone does not initialize state, open ports or set up OAuth.

The account owner completes device login in their browser. Never ask for pasted OAuth tokens; Platform API keys are not a replacement. Verify API/SSH boundaries before onboarding clients. Model discovery, Doctor and health checks do not generate inference. Live Limits/Overview may trigger [weekly activation](cli.md#weekly-activation); include it in agent verification only when inference is authorized.

## Upgrade and rollback

Back up and verify private state with the documented [backup procedure](backup.md). Keep the old binary/version, install the new binary atomically, and use systemd's graceful restart. Do not replace or rotate keys just to upgrade. Review schema/version compatibility before rolling back; an older binary refuses a newer schema. Preserve current connection settings. Restart an already open `exr` after replacing its binary.

## Agent installation

Copy [the client prompt](../prompts/install-client.md) for an existing server, or [the server prompt](../prompts/install-server.md) for self-hosting. Fill in the connection/domain/public-key details before giving it to an agent. OAuth login and clipboard token issuance require the user's interaction. Install [exr](../skills/exr/SKILL.md) for client/standalone work, or [exrd](../skills/exrd/SKILL.md) for server operation.

## Updates

The dashboard highlights **Settings ↑** when a client update is available. Use its update action or the commands below, then reopen the dashboard. `EXR_NO_UPDATE_CHECK=1` disables automatic release checks, which send no router credentials, account or usage data. See [dashboard controls](cli.md#software-updates).

```sh
exr update --check          # Read-only version check
exr update --check --json   # For agents
exr update                 # Install the latest release
exrd update --check         # Server operator: read-only check
exrd update                 # Server operator: update the installed server
```

| Installation | Update method |
| --- | --- |
| Release in `PREFIX/bin` | `exr update` / `exrd update`: checksummed atomic replacement in the same prefix |
| Cargo | Built-in update: locked source build of the published tag; requires Rust and a C compiler |
| Homebrew | `brew upgrade mocki-toki/exetrouter/exr`; built-in update preserves Homebrew ownership |
| Nix | Profile upgrade or flake update/rebuild, as [described above](#nix-linux-or-macos) |
| Docker | [Compose update procedure](../deploy/docker/README.md#updates-with-compose), with snapshot/schema checks and a matching native gateway |
| Other managed/custom path | The package manager or deployment method that installed it |

Settings, state, keys and tokens stay in place. Built-in update never silently downgrades. For older binaries without `update`, rerun the reviewed installer with the same prefix. A remote client's Settings updates only its `exr`; native servers need the normal service restart after replacement.

Docker's optional `update-host` helper automates digest pinning, backup/schema checks and health rollback. Review/install the new helpers once when migrating older source-building installations; preserve custom Compose files and keep Docker access away from the restricted gateway.
