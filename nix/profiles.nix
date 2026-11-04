{
  lib,
  pkgs,
  cfg,
  stateHome,
  configPath,
}: let
  inherit (lib) mkOption types;
  command = name:
    if name == "default"
    then "exr"
    else "exr-${name}";
  stateDirectory = name: "${stateHome}/exr${lib.optionalString (name != "default") "-${name}"}";
  standalone = lib.filterAttrs (_: c: c.mode == "standalone") cfg.connections;
  script = name: arguments: let
    c = cfg.connections.${name};
  in ''
    set -eu
    umask 077
    ${pkgs.coreutils}/bin/mkdir -p -m 700 ${lib.escapeShellArg c.stateDirectory}
    export EXR_PRIVACY_KEY=${lib.escapeShellArg "${c.stateDirectory}/privacy.key"}
    exec ${cfg.package}/bin/exr ${arguments} "$@"
  '';
  wrappers = lib.mapAttrs (name: _: pkgs.writeShellScriptBin (command name) (script name "--config ${lib.escapeShellArg (configPath name)}")) cfg.connections;
  clients = lib.mapAttrs (name: _: pkgs.writeShellScript "exr-${name}-client" (script name "")) cfg.connections;
in {
  inherit command wrappers clients;
  options = {
    enable = lib.mkEnableOption "declarative ExetRouter connections";
    package = mkOption {
      type = types.package;
      default = pkgs.exetrouter or (pkgs.callPackage ./package.nix {});
      defaultText = lib.literalExpression "pkgs.exetrouter";
      description = "Package providing exr and exrd.";
    };
    connections = mkOption {
      default = {};
      description = "Connection profiles; default provides exr, other names provide exr-NAME.";
      type = types.attrsOf (types.submodule ({
        name,
        config,
        ...
      }: {
        options = {
          mode = mkOption {
            type = types.enum ["remote" "standalone"];
            default = "remote";
            description = "Connection mode.";
          };
          autoStart = mkOption {
            type = types.bool;
            default = config.mode == "standalone";
            defaultText = lib.literalExpression ''config.mode == "standalone"'';
            description = "Serve a standalone profile automatically using systemd (Linux only). NixOS uses system services; Home Manager uses user services.";
          };
          host = mkOption {
            type = types.str;
            default = "localhost";
            description = "Remote SSH host.";
          };
          port = mkOption {
            type = types.port;
            default = 2222;
            description = "Remote SSH port.";
          };
          sshUser = mkOption {
            type = types.str;
            default = "routercli";
            description = "Remote SSH gateway user.";
          };
          identity = mkOption {
            type = types.nullOr types.str;
            default = null;
            description = "Absolute runtime SSH identity path, including agent-backed public keys. Must have owner-only permissions. Never store private keys in Nix.";
          };
          apiUrl = mkOption {
            type = types.nullOr types.str;
            default = let
              address =
                if config.mode == "standalone"
                then config.listenAddress
                else config.host;
              host =
                if lib.hasInfix ":" address && !(lib.hasPrefix "[" address)
                then "[${address}]"
                else address;
            in
              if config.mode == "standalone"
              then "http://${host}:${toString config.listenPort}/v1"
              else "https://${host}/v1";
            defaultText = lib.literalExpression ''"https://router.example.com/v1" for host = "router.example.com", or the standalone loopback endpoint'';
            description = "HTTP(S) API URL ending in /v1, without credentials, query or fragment. Remote defaults use the SSH host with HTTPS, not the SSH port. Override for separate HTTP deployments; null omits the URL from the CLI profile.";
          };
          transport = mkOption {
            type = types.enum ["websocket" "http"];
            default = "websocket";
            description = "OpenCode provider transport for this connection.";
          };
          stateDirectory = mkOption {
            type = types.str;
            default = stateDirectory name;
            defaultText = lib.literalExpression ''"STATE_HOME/exr[-NAME]"'';
            description = "Absolute mutable private state directory, also holding the client privacy key.";
          };
          listenAddress = mkOption {
            type = types.enum ["127.0.0.1" "::1"];
            default = "127.0.0.1";
            description = "Standalone loopback listener.";
          };
          listenPort = mkOption {
            type = types.port;
            default = 8787;
            description = "Standalone API port; unique per standalone connection.";
          };
        };
      }));
    };
  };
  configs = lib.mapAttrs (_: c:
    builtins.toJSON (
      {mode = c.mode;}
      // (
        if c.mode == "standalone"
        then {
          state_dir = c.stateDirectory;
          listen = "${
            if c.listenAddress == "::1"
            then "[::1]"
            else c.listenAddress
          }:${toString c.listenPort}";
        }
        else {
          inherit (c) host port identity;
          ssh_user = c.sshUser;
        }
      )
      // lib.optionalAttrs (c.apiUrl != null) {api_url = c.apiUrl;}
    ))
  cfg.connections;
  services = lib.mapAttrs' (name: _:
    lib.nameValuePair (command name) {
      Type = "exec";
      ExecStart = "${wrappers.${name}}/bin/${command name} serve";
      Restart = "on-failure";
      RestartSec = 5;
      UMask = "0077";
      NoNewPrivileges = true;
      LimitCORE = 0;
    }) (lib.filterAttrs (_: c: c.autoStart) standalone);
  package = pkgs.runCommand "exetrouter-profiles" {} ''
    mkdir -p "$out/bin"
    ${lib.concatMapStringsSep "\n" (name: ''ln -s ${wrappers.${name}}/bin/${command name} "$out/bin/${command name}"'') (builtins.attrNames wrappers)}
    ${lib.optionalString (!(cfg.connections ? default)) ''ln -s ${cfg.package}/bin/exr "$out/bin/exr"''}
    ln -s ${cfg.package}/bin/exrd "$out/bin/exrd"
  '';
  assertions =
    lib.concatLists (lib.mapAttrsToList (name: c: [
        {
          assertion = builtins.match "[A-Za-z0-9][A-Za-z0-9_-]*" name != null;
          message = "programs.exetrouter: invalid connection name ${name}.";
        }
        {
          assertion = lib.hasPrefix "/" c.stateDirectory;
          message = "programs.exetrouter: stateDirectory must be absolute.";
        }
        {
          assertion = c.mode != "remote" || (c.identity != null && lib.hasPrefix "/" c.identity && builtins.match "[A-Za-z0-9_][A-Za-z0-9_.-]*" c.sshUser != null && c.host != "" && !(lib.hasPrefix "-" c.host) && lib.all (s: lib.elem s (lib.stringToCharacters "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_:[]")) (lib.stringToCharacters c.host) && c.port > 0);
          message = "programs.exetrouter: remote connection ${name} requires a valid host/user, nonzero port and absolute identity path.";
        }
        {
          assertion = c.apiUrl == null || builtins.match "https?://[^/@?#[:space:]]+(/[^@?#[:space:]]*)?/v1/?" c.apiUrl != null;
          message = "programs.exetrouter: invalid apiUrl for ${name}.";
        }
        {
          assertion = c.mode != "standalone" || c.listenPort > 0;
          message = "programs.exetrouter: standalone listenPort must be nonzero.";
        }
        {
          assertion = !c.autoStart || (c.mode == "standalone" && pkgs.stdenv.hostPlatform.isLinux);
          message = "programs.exetrouter: autoStart requires a standalone connection on Linux; set autoStart = false otherwise.";
        }
      ])
      cfg.connections)
    ++ [
      {
        assertion = let paths = map (c: c.stateDirectory) (builtins.attrValues cfg.connections); in builtins.length paths == builtins.length (lib.unique paths);
        message = "programs.exetrouter: connections must have distinct state directories.";
      }
      {
        assertion = let ports = map (c: c.listenPort) (builtins.attrValues standalone); in builtins.length ports == builtins.length (lib.unique ports);
        message = "programs.exetrouter: standalone connections must have distinct ports.";
      }
    ];
}
