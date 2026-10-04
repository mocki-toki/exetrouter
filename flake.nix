{
  description = "ExetRouter client and shared API server";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      packageFor = system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          manifest = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = "exetrouter";
          version = manifest.package.version;
          src = pkgs.lib.cleanSource self;
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [ pkgs.makeWrapper ];
          # The upstream suite assumes host timezone data in its CLI fixtures.
          # Run it in the development shell rather than the build sandbox.
          doCheck = false;

          postInstall = ''
            wrapProgram "$out/bin/exr" \
              --prefix PATH : ${pkgs.lib.makeBinPath (
                [ pkgs.openssh ]
                ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.wl-clipboard pkgs.xclip ]
              )}
          '';

          meta = {
            description = manifest.package.description;
            homepage = manifest.package.repository;
            license = pkgs.lib.licenses.mit;
            mainProgram = "exr";
            platforms = systems;
          };
        };
    in
    {
      packages = forAllSystems (system: {
        exetrouter = packageFor system;
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
      });
      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.exetrouter ];
            packages = [ pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy pkgs.python3 ];
          };
        });
    };
}
