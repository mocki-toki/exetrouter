# NixOS and Home Manager

ExetRouter exports `nixosModules.exetrouter` and
`homeManagerModules.exetrouter` (both also available as `default`), and
`overlays.default`. Both modules use `programs.exetrouter` and can also be
imported directly from `nix/nixos.nix` or `nix/home-manager.nix` in a source
checkout. Raw modules do not require flake arguments.

## NixOS

Add an input to your system flake:

```nix
inputs.exetrouter.url = "github:mocki-toki/exetrouter";
inputs.exetrouter.inputs.nixpkgs.follows = "nixpkgs";
```

Import `exetrouter.nixosModules.default` into your NixOS modules:

```nix
{ ... }: {
  programs.exetrouter = {
    enable = true;
    connections = {
      default = {
        host = "router.example.com";
        identity = "/var/lib/exetrouter/router.pub";
      };
      local = {
        mode = "standalone";
        listenPort = 8787;
      };
      sandbox = {
        mode = "standalone";
        listenPort = 8788;
        autoStart = false;
      };
    };
  };
}
```

Importing the NixOS module installs the overlay even when the program is
disabled: `pkgs.exetrouter` is available for `environment.systemPackages` or
other consumers. The module installs the profile-aware commands system-wide,
without depending on Home Manager. Generated connection metadata lives under
`/etc/exetrouter/NAME.json`.

Standalone connections run as **system services**, under an automatically
created unprivileged `exetrouter` system account. There is no `user` option.
State defaults to `/var/lib/exetrouter/exr[-NAME]` and is private to this account.
Use `sudo -u exetrouter exr-local account add` for initial OAuth setup or a
dashboard attached to a system-managed standalone profile. Provision remote
identities for this account separately, including any necessary agent access.
Never run the standalone profile as root or point a personal user at its private
system state. For personal connections, use Home Manager instead.

System and Home Manager units have separate scopes, but avoid managing the
same state directory or listener in both modules. Home Manager commands take
precedence over system commands in the user's profile.

## Home Manager

Import `exetrouter.homeManagerModules.default` into your Home Manager modules.
The same connection options work: the current Home Manager user owns the
profiles, and standalone connections run as **user services**.

```nix
{
  programs.exetrouter = {
    enable = true;
    connections.default = {
      mode = "standalone";
      listenPort = 8787;
    };
  };
}
```

Home Manager generates `exr/config.json` and `exr/connections/NAME.json` under
`xdg.configHome`. It uses `pkgs.exetrouter` if available, otherwise builds the
bundled package expression. To install the overlay explicitly, add
`exetrouter.overlays.default` to `nixpkgs.overlays` where your package set is
constructed. Both modules accept `programs.exetrouter.package` overrides.

## OpenCode plugin integration

The flake also exports `packages.SYSTEM.exetrouter-opencode-plugin`, and the
overlay exposes `pkgs.exetrouter-opencode-plugin`. This is a local plugin
directory, not a CLI executable: it includes the existing V2 server plugin,
production npm dependencies and the prebuilt experimental GUI bundle. Its
dependencies are pinned by the committed npm lockfile and a Nix dependency
hash. Nix runs its typecheck and synthetic test suite during the build.

The Home Manager module extends `programs.opencode.exr`:

```nix
{
  programs.exetrouter = {
    enable = true;
    connections.default.mode = "standalone";
  };
  programs.opencode.exr.enable = true;
  programs.opencode.package = opencodeV2Package; # Supply your OpenCode 2.2.x derivation.
}
```

This enables OpenCode by default and appends
the packaged plugin to `programs.opencode.settings.plugins`, preserving other
plugins and settings. It does not enable the web service or install Desktop.
The same plugin package supplies the server provider and native desktop GUI.
The integration does not expose a listener to the
network, change a model, issue tokens, or restart the shared OpenCode service.
Supply `programs.opencode.package` from your OpenCode V2 package source. The
module requires version 2.2.x to match this plugin's experimental API contract;
the nixpkgs revision pinned here currently defaults to V1 and is rejected.
The example's `opencodeV2Package` is a placeholder for that derivation, not an
export of this flake. Later API versions need compatibility validation before
the guard is widened.

For Desktop, separately add `home.packages = [ pkgs.opencode-desktop ];` using
a package set that provides a compatible OpenCode 2.2.x Desktop. Desktop loads
the GUI part from the same global plugin registration. To also run the Home
Manager web service, independently set `programs.opencode.web.enable = true`;
the browser uses the server provider, not the native desktop dashboard.

Select a different managed profile and endpoint as follows:

```nix
{
  programs.exetrouter = {
    enable = true;
    connections.work = {
      host = "router.example.com";
      identity = "/home/exetrouter/.ssh/router.pub";
      apiUrl = "https://api.example.com/v1";
      transport = "http"; # Defaults to "websocket".
    };
  };
  programs.opencode.exr = {
    enable = true;
    connection = "work";
    refreshSeconds = 60; # 30–3600.
  };
}
```

`connection` defaults to `default`. The plugin consumes `apiUrl` and `transport`
from the selected `programs.exetrouter.connections` profile; these are not
separate OpenCode integration options. Remote `apiUrl` defaults to
HTTPS on the SSH host with a `/v1` path (with IPv6 brackets), independently of
the SSH port. For example, `router.example.com` gives `https://router.example.com/v1`.
Standalone profiles default to their HTTP loopback endpoint and configured port.
Override `apiUrl` for a different HTTP host, port or path. The derived remote
URL is a convention, not proof that HTTPS and SSH serve the same deployment.
The plugin rejects null URLs and remote HTTP: use HTTPS, or HTTP on loopback.
`package` overrides the plugin directory derivation when needed.

The generated plugin options include an absolute native client wrapper and
the selected configuration path. The wrapper preserves that connection's
mutable privacy-key location without injecting a second `--config` flag.
The read-only profile cannot be edited through the plugin Settings screen;
change it in Nix instead.

Connect the ExetRouter provider through OpenCode's account UI to save a router
bearer in its native credential store. Alternatively supply `EXETROUTER_TOKEN`
to the OpenCode server at runtime. Never put a bearer in Nix options. The
integration configures the existing plugin; it does not port the desktop
dashboard's native IPC bridge to browsers. See the
[plugin guide](opencode-desktop.md) for its GUI capabilities and limitations.

For manual loading without Home Manager, add an entry whose `package` is the
Nix plugin output directory to OpenCode V2's `plugins` array. A derivation is
not a plugin executable or a bearer credential.

## Connections and services

- `default` provides `exr`; other connections provide `exr-NAME`. Without a
  default connection, `exr` retains its upstream behavior. `exrd` remains
  available but these modules do not configure a shared server.
- Remote defaults are `localhost`, port `2222`, user `routercli`. Supply an
  absolute `identity` path. `apiUrl` defaults to HTTPS on the SSH host and must
  end in `/v1`; set it to null to omit it from the CLI profile. `transport`
  selects the OpenCode provider transport (`websocket` by default, or `http`).
- Standalone profiles default to `127.0.0.1:8787`; `listenAddress = "::1"`
  is also supported. Each standalone profile must use a distinct port.
- Each standalone connection automatically serves its configured port through
  a systemd unit: `exr.service` or `exr-NAME.service`. NixOS units are system
  services; Home Manager units are user services. There is no
  separate router selector or service. Set `autoStart = false` to omit a unit
  while keeping its command/configuration. Remote connections cannot autostart.
- NixOS services start at boot under the dedicated account. Home Manager
  services start with the user manager. Enable lingering separately if
  needed without a login. No firewall ports are opened. On macOS, set
  `autoStart = false` for standalone connections; launchd is not configured.
- Opening a dashboard for an already-served standalone profile attaches to
  that runtime via its lock and endpoint metadata, rather than creating a
  competing listener. `default` may be either remote or standalone.

```sh
systemctl status exr-local                      # NixOS
sudo -u exetrouter exr-local account add         # NixOS account setup
systemctl --user status exr                     # Home Manager
exr account add                                # Home Manager account setup
```

## Mutable state and security

Each connection has a distinct `stateDirectory`, defaulting to
`/var/lib/exetrouter/exr[-NAME]` on NixOS and `xdg.dataHome/exr[-NAME]`
(normally `~/.local/share/exr[-NAME]`) with Home Manager. Wrappers create it with mode
`0700` and set `EXR_PRIVACY_KEY` to its `privacy.key`. This keeps generated
privacy keys out of immutable configuration directories. Preserve these keys
and standalone state across upgrades; back them up privately. NixOS tmpfiles
rules enforce ownership by `exetrouter` and mode `0700` for configured state
directories. Home Manager leaves existing directory permissions unchanged;
the client rejects insecure standalone state.

Configuration is public Nix-store data: use only connection metadata and
runtime file paths, never private keys, OAuth credentials, passwords or bearer
tokens. Provision SSH identities separately with owner-only permissions; exr
requires these even for public keys backed by an SSH agent. Verify and trust
remote host keys separately, for example with NixOS
`programs.ssh.knownHosts`. The module does not trust scanned keys automatically.

Generated configurations are read-only. Change connection settings in Nix,
not through `exr configure` or the Settings editor. Account authorization,
tokens and other runtime operations still use mutable state. When migrating
an existing profile, preserve its original privacy key (normally beside the
old JSON configuration with a `.privacy-key` extension). Provision that same
key as `stateDirectory/privacy.key`, with owner-only permissions, before using
the generated wrapper. Do not generate a replacement key for existing data.

## Checks

`nix build .#checks.x86_64-linux.modules` evaluates both real module systems
with synthetic connections, checks validation and service selection, and
executes a wrapper with a stub client in a temporary private directory. The
Home Manager flake input is used only by checks, not by either raw module.
