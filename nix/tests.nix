{
  nixpkgs,
  home-manager,
  pkgs,
}: let
  inherit (pkgs) lib;
  stub = pkgs.runCommand "exetrouter-stub" {} ''
    mkdir -p "$out/bin"
    cat > "$out/bin/exr" <<'EOF'
    #!${pkgs.runtimeShell}
    printf '%s\n' "$EXR_PRIVACY_KEY" "$@"
    EOF
    chmod +x "$out/bin/exr"
    ln -s exr "$out/bin/exrd"
  '';
  connections = {
    default = {mode = "standalone";};
    remote = {
      host = "router.example.com";
      identity = "/home/exetrouter/.ssh/router.pub";
    };
    sandbox = {
      mode = "standalone";
      listenAddress = "::1";
      listenPort = 8788;
    };
    manual = {
      mode = "standalone";
      listenPort = 8789;
      autoStart = false;
    };
  };
  hm = overrides:
    (home-manager.lib.homeManagerConfiguration {
      inherit pkgs;
      modules = [
        ./home-manager.nix
        {
          home = {
            username = "exetrouter";
            homeDirectory = "/home/exetrouter";
            stateVersion = "24.11";
          };
          programs.exetrouter = {
            enable = true;
            package = stub;
            inherit connections;
          };
        }
        overrides
      ];
    }).config;
  home = hm {};
  named = hm {programs.exetrouter.connections = lib.mkForce {work = connections.remote;};};
  namedPackage = lib.findFirst (p: p.name == "exetrouter-profiles") (throw "missing named profile package") named.home.packages;
  invalid = connections: hm {programs.exetrouter.connections = lib.mkForce connections;};
  valid = cfg: lib.all (a: a.assertion) cfg.assertions;
  rejected = connections: let result = builtins.tryEval (valid (invalid connections)); in !result.success || !result.value;
  pluginStub = pkgs.runCommand "opencode-plugin-stub" {} ''mkdir -p "$out"; echo '{}' > "$out/package.json"'';
  integrated = overrides:
    hm {
      config = lib.mkMerge [
        {
          programs.exetrouter.connections = lib.mkForce {
            selected = {
              mode = "standalone";
              listenAddress = "::1";
              listenPort = 8790;
              autoStart = false;
            };
          };
          programs.opencode.exr = {
            enable = true;
            connection = "selected";
            package = pluginStub;
          };
          programs.opencode.settings.plugins = ["unrelated-plugin"];
          programs.opencode.package = stub // {version = "2.2.1";};
        }
        overrides
      ];
    };
  integration = integrated {};
  pluginEntry = lib.findFirst builtins.isAttrs (throw "missing plugin entry") integration.programs.opencode.settings.plugins;
  rejectsIntegration = overrides: let result = builtins.tryEval (valid (integrated overrides)); in !result.success || !result.value;
  withAPI = apiUrl:
    integrated {
      programs.exetrouter.connections = lib.mkOverride 40 {
        selected = {
          mode = "standalone";
          autoStart = false;
          inherit apiUrl;
        };
      };
    };
  rejectsAPI = url: let result = builtins.tryEval (valid (withAPI url)); in !result.success || !result.value;
  remoteIntegration = integrated {programs.exetrouter.connections = lib.mkOverride 40 {selected = connections.remote // {transport = "http";};};};
  remoteEntry = lib.findFirst builtins.isAttrs (throw "missing remote plugin") remoteIntegration.programs.opencode.settings.plugins;
  nixos =
    (nixpkgs.lib.nixosSystem {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        ./nixos.nix
        {
          programs.exetrouter = {
            enable = true;
            package = stub;
            inherit connections;
          };
          system.stateVersion = "24.11";
          boot.loader.grub.enable = false;
          fileSystems."/" = {
            device = "/dev/test";
            fsType = "ext4";
          };
        }
      ];
    }).config;
  imported =
    (nixpkgs.lib.nixosSystem {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [./nixos.nix];
    }).pkgs;
  isLinux = pkgs.stdenv.hostPlatform.isLinux;
  testHome =
    if isLinux
    then home
    else
      hm {
        programs.exetrouter.connections = lib.mapAttrs (_: _: {autoStart = lib.mkForce false;}) connections;
      };
  package = lib.findFirst (p: p.name == "exetrouter-profiles") (throw "missing profile package") testHome.home.packages;
  runtime = import ./profiles.nix {
    inherit lib pkgs;
    cfg = {
      package = stub;
      connections = {
        default = testHome.programs.exetrouter.connections.default // {stateDirectory = "/tmp/exr-nix-test-placeholder/default";};
        remote = testHome.programs.exetrouter.connections.remote // {stateDirectory = "/tmp/exr-nix-test-placeholder/remote";};
      };
    };
    stateHome = "/unused";
    configPath = _: "/synthetic/config.json";
  };
in
  assert valid testHome;
  assert valid named;
  assert valid integration;
  assert rejectsIntegration {programs.opencode.package = lib.mkForce (stub // {version = "1.18.33";});};
  assert rejectsIntegration {programs.opencode.package = lib.mkForce (stub // {version = "2.3.0";});};
  assert valid (integrated {programs.opencode.web.enable = true;});
  assert valid (withAPI "https://api.example.com/v1");
  assert rejectsAPI "http://localhost.example.com/v1";
  assert rejectsAPI "http://[::1]:8790/v1#fragment";
  assert rejectsAPI null;
  assert valid remoteIntegration;
  assert remoteEntry.options.baseURL == "https://router.example.com/v1";
  assert remoteEntry.options.transport == "http";
  assert integration.programs.opencode.enable && !integration.programs.opencode.web.enable;
  assert !(integration.systemd.user.services ? opencode-web);
  assert !(lib.any (p: (p.pname or "") == "opencode-desktop") integration.home.packages);
  assert builtins.elem "unrelated-plugin" integration.programs.opencode.settings.plugins;
  assert pluginEntry.package == toString pluginStub;
  assert pluginEntry.options.baseURL == "http://[::1]:8790/v1";
  assert pluginEntry.options.exrConfig == "/home/exetrouter/.config/exr/connections/selected.json";
  assert rejectsIntegration {programs.opencode.exr.connection = lib.mkForce "missing";};
  assert rejectsAPI "http://router.example.com/v1";
  assert rejectsAPI "https://user:password@api.example.com/v1";
  assert rejectsAPI "https://api.example.com/v1?secret=test";
  assert !(named.xdg.configFile ? "exr/config.json");
  assert builtins.fromJSON testHome.xdg.configFile."exr/connections/sandbox.json".text
  == {
    mode = "standalone";
    api_url = "http://[::1]:8788/v1";
    listen = "[::1]:8788";
    state_dir = "/home/exetrouter/.local/share/exr-sandbox";
  };
  assert !(testHome.systemd.user.services ? exr-remote);
  assert !(testHome.systemd.user.services ? exr-manual);
  assert !isLinux || (testHome.systemd.user.services ? exr && testHome.systemd.user.services ? exr-sandbox);
  assert rejected {a = connections.remote // {identity = null;};};
  assert rejected {"bad/name" = connections.remote;};
  assert rejected {a = connections.remote // {autoStart = true;};};
  assert rejected {a = connections.remote // {stateDirectory = "relative";};};
  assert rejected {
    a = {
      mode = "standalone";
      listenPort = 0;
    };
  };
  assert rejected {a = connections.remote // {apiUrl = "https://user:password@api.example.com/v1";};};
  assert rejected {
    a.mode = "standalone";
    b.mode = "standalone";
  };
  assert rejected {
    a = connections.remote // {stateDirectory = "/same";};
    b = connections.remote // {stateDirectory = "/same";};
  };
  assert !isLinux || (valid nixos && nixos.systemd.services.exr.serviceConfig.User == "exetrouter" && !(nixos.systemd.services ? exr-manual) && !(nixos.systemd.user.services ? exr));
  assert !isLinux || builtins.elem "multi-user.target" nixos.systemd.services.exr.wantedBy;
  assert !isLinux || nixos.users.users.exetrouter.isSystemUser;
  assert !isLinux || (builtins.fromJSON nixos.environment.etc."exetrouter/default.json".text).state_dir == "/var/lib/exetrouter/exr";
  assert !isLinux || lib.hasInfix "User=exetrouter" nixos.systemd.units."exr.service".text;
  assert !isLinux || builtins.isString nixos.system.build.toplevel.drvPath;
  assert !isLinux || imported.exetrouter.pname == "exetrouter";
    pkgs.runCommand "exetrouter-module-tests" {} ''
      test -x ${package}/bin/exr
      test -x ${package}/bin/exr-remote
      test -x ${package}/bin/exrd
      test "$(readlink ${namedPackage}/bin/exr)" = ${stub}/bin/exr
      test -x ${namedPackage}/bin/exr-work
      test -x ${pluginEntry.options.exrBinary}
      # Execute a wrapper with private state in the sandbox, never a real home.
      substitute ${runtime.wrappers.default}/bin/exr wrapper \
        --replace-fail /tmp/exr-nix-test-placeholder "$TMPDIR/state"
      ${pkgs.runtimeShell} wrapper models --json > actual
      printf '%s\n' "$TMPDIR/state/default/privacy.key" --config /synthetic/config.json models --json > expected
      diff -u expected actual
      # Plugin passes exrConfig itself: its binary must not inject --config twice.
      substitute ${runtime.clients.remote} plugin-client \
        --replace-fail /tmp/exr-nix-test-placeholder "$TMPDIR/state"
      ${pkgs.runtimeShell} plugin-client --config /synthetic/remote.json doctor --json > actual
      printf '%s\n' "$TMPDIR/state/remote/privacy.key" --config /synthetic/remote.json doctor --json > expected
      diff -u expected actual
      test "$(${pkgs.coreutils}/bin/stat -c %a "$TMPDIR/state/default")" = 700
      substitute ${runtime.wrappers.remote}/bin/exr-remote remote-wrapper \
        --replace-fail /tmp/exr-nix-test-placeholder "$TMPDIR/state"
      ${pkgs.runtimeShell} remote-wrapper usage 'argument with spaces' > actual
      printf '%s\n' "$TMPDIR/state/remote/privacy.key" --config /synthetic/config.json usage 'argument with spaces' > expected
      diff -u expected actual
      touch "$out"
    ''
