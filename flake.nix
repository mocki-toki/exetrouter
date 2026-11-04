{
  description = "ExetRouter client and shared API server";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # Used only to evaluate the Home Manager module in checks.
  inputs.home-manager.url = "github:nix-community/home-manager";
  inputs.home-manager.inputs.nixpkgs.follows = "nixpkgs";

  outputs = {
    self,
    nixpkgs,
    home-manager,
  }: let
    systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
    forAllSystems = nixpkgs.lib.genAttrs systems;
    packageFor = system: nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix {};
  in {
    overlays.default = import ./nix/overlay.nix;
    nixosModules.default = self.nixosModules.exetrouter;
    nixosModules.exetrouter = import ./nix/nixos.nix;
    homeManagerModules.default = self.homeManagerModules.exetrouter;
    homeManagerModules.exetrouter = import ./nix/home-manager.nix;
    packages = forAllSystems (system: {
      exetrouter = packageFor system;
      exetrouter-opencode-plugin = nixpkgs.legacyPackages.${system}.callPackage ./nix/opencode-plugin.nix {};
      default = self.packages.${system}.exetrouter;
    });
    apps = forAllSystems (system: {
      default = self.apps.${system}.exr;
      exr = {
        type = "app";
        meta.description = "ExetRouter client and standalone API";
        program = "${self.packages.${system}.exetrouter}/bin/exr";
      };
      exrd = {
        type = "app";
        meta.description = "ExetRouter shared API server";
        program = "${self.packages.${system}.exetrouter}/bin/exrd";
      };
    });
    checks = forAllSystems (system: {
      package = self.packages.${system}.exetrouter;
      opencode-plugin = self.packages.${system}.exetrouter-opencode-plugin;
      modules = import ./nix/tests.nix {
        inherit nixpkgs home-manager;
        pkgs = nixpkgs.legacyPackages.${system};
      };
    });
    devShells = forAllSystems (system: let
      pkgs = nixpkgs.legacyPackages.${system};
    in {
      default = pkgs.mkShell {
        inputsFrom = [self.packages.${system}.exetrouter];
        packages = [pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy pkgs.python3];
      };
    });
  };
}
