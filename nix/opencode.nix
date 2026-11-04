{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.opencode.exr;
  exr = config.programs.exetrouter;
  connection = exr.connections.${cfg.connection} or null;
  relativePath = name:
    if name == "default"
    then "exr/config.json"
    else "exr/connections/${name}.json";
  profiles = import ./profiles.nix {
    inherit lib pkgs;
    cfg = exr;
    stateHome = config.xdg.dataHome;
    configPath = name: "${config.xdg.configHome}/${relativePath name}";
  };
in {
  options.programs.opencode.exr = {
    enable = lib.mkEnableOption "the packaged ExetRouter OpenCode V2 plugin";
    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.exetrouter-opencode-plugin or (pkgs.callPackage ./opencode-plugin.nix {});
      defaultText = lib.literalExpression "pkgs.exetrouter-opencode-plugin";
      description = "Plugin package directory, containing package.json, server sources and the built GUI bundle.";
    };
    connection = lib.mkOption {
      type = lib.types.str;
      default = "default";
      description = "Managed programs.exetrouter connection used by the native plugin bridge.";
    };
    refreshSeconds = lib.mkOption {
      type = lib.types.ints.between 30 3600;
      default = 60;
      description = "Catalog refresh interval in seconds.";
    };
  };
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = let version = config.programs.opencode.package.version or "0"; in lib.versionAtLeast version "2.2.0" && lib.versionOlder version "2.3.0";
        message = "programs.opencode.exr requires programs.opencode.package to provide OpenCode V2 2.2.x (the packaged plugin's API contract); the nixpkgs default may still be V1.";
      }
      {
        assertion = exr.enable && connection != null;
        message = "programs.opencode.exr requires an enabled programs.exetrouter and an existing connection.";
      }
      {
        assertion =
          (connection != null && connection.apiUrl != null)
          && !(lib.hasInfix "\\" connection.apiUrl)
          && (
            builtins.match "https://[^/@?#[:space:]]+(/[^@?#[:space:]]*)?/v1/?" connection.apiUrl
            != null
            || lib.any (
              host: let
                prefix = "http://${host}";
              in
                lib.hasPrefix prefix connection.apiUrl
                && builtins.match "(:[0-9]+)?(/[^@?#[:space:]]*)?/v1/?" (lib.removePrefix prefix connection.apiUrl) != null
            ) ["localhost" "127.0.0.1" "[::1]"]
          );
        message = "programs.opencode.exr requires the selected connection's apiUrl to be HTTPS (or loopback HTTP), ending in /v1 without credentials, query or fragment.";
      }
    ];
    programs.opencode = {
      enable = lib.mkDefault true;
      settings.plugins = [
        {
          package = toString cfg.package;
          options = {
            inherit (cfg) refreshSeconds;
            baseURL =
              if connection == null
              then null
              else connection.apiUrl;
            transport =
              if connection == null
              then "websocket"
              else connection.transport;
            exrBinary = toString (profiles.clients.${cfg.connection} or "");
            exrConfig = "${config.xdg.configHome}/${relativePath cfg.connection}";
          };
        }
      ];
    };
  };
}
