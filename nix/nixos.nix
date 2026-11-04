{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.exetrouter;
  profiles = import ./profiles.nix {
    inherit lib pkgs cfg;
    stateHome = "/var/lib/exetrouter";
    configPath = name: "/etc/exetrouter/${name}.json";
  };
in {
  options.programs.exetrouter = profiles.options;
  config = lib.mkMerge [
    {nixpkgs.overlays = [(import ./overlay.nix)];}
    (lib.mkIf cfg.enable {
      assertions = profiles.assertions;
      environment.systemPackages = [profiles.package];
      users.groups.exetrouter = {};
      users.users.exetrouter = {
        isSystemUser = true;
        group = "exetrouter";
        home = "/var/lib/exetrouter";
        createHome = true;
        homeMode = "0700";
      };
      systemd.tmpfiles.settings.exetrouter = lib.mapAttrs' (_: c:
        lib.nameValuePair c.stateDirectory {
          d = {
            mode = "0700";
            user = "exetrouter";
            group = "exetrouter";
          };
        })
      cfg.connections;
      environment.etc = lib.mapAttrs' (name: text: lib.nameValuePair "exetrouter/${name}.json" {inherit text;}) profiles.configs;
      systemd.services =
        lib.mapAttrs (_: serviceConfig: {
          description = "ExetRouter standalone API";
          wantedBy = ["multi-user.target"];
          wants = ["network-online.target"];
          after = ["network-online.target"];
          serviceConfig =
            serviceConfig
            // {
              User = "exetrouter";
              Group = "exetrouter";
            };
        })
        profiles.services;
    })
  ];
}
