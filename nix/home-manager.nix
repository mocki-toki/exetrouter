{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.exetrouter;
  relativePath = name:
    if name == "default"
    then "exr/config.json"
    else "exr/connections/${name}.json";
  profiles = import ./profiles.nix {
    inherit lib pkgs cfg;
    stateHome = config.xdg.dataHome;
    configPath = name: "${config.xdg.configHome}/${relativePath name}";
  };
in {
  imports = [./opencode.nix];
  options.programs.exetrouter = profiles.options;
  config = lib.mkIf cfg.enable {
    assertions = profiles.assertions;
    home.packages = [profiles.package];
    xdg.configFile = lib.mapAttrs' (name: text: lib.nameValuePair (relativePath name) {inherit text;}) profiles.configs;
    systemd.user.services =
      lib.mapAttrs (_: Service: {
        Unit = {
          Description = "ExetRouter standalone API";
        };
        Install.WantedBy = ["default.target"];
        inherit Service;
      })
      profiles.services;
  };
}
