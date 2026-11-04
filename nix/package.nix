{
  lib,
  rustPlatform,
  makeWrapper,
  openssh,
  wl-clipboard,
  xclip,
  stdenv,
}: let
  manifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
in
  rustPlatform.buildRustPackage {
    pname = "exetrouter";
    version = manifest.package.version;
    src = lib.cleanSourceWith {
      src = lib.cleanSource ../.;
      # Raw/path-flake consumers may have local Cargo build artifacts.
      filter = path: type: !(type == "directory" && builtins.baseNameOf path == "target");
    };
    cargoLock.lockFile = ../Cargo.lock;
    cargoLock.outputHashes = {
      "tokio-tungstenite-0.28.0" = "sha256-V1xmnrfRWOcZZogelZEA4vvyMj2awCfHVA5/glQ6KAI=";
      "tungstenite-0.27.0" = "sha256-VVHhk7l9J/sEmG3q/UuV/sQ3f+fGsmq5vumSy8vbMvw=";
    };
    nativeBuildInputs = [makeWrapper];
    # CLI fixtures require host timezone data; run Rust tests in the dev shell.
    doCheck = false;
    postInstall = ''
      wrapProgram "$out/bin/exr" --prefix PATH : ${lib.makeBinPath (
        [openssh] ++ lib.optionals stdenv.hostPlatform.isLinux [wl-clipboard xclip]
      )}
    '';
    meta = {
      description = manifest.package.description;
      homepage = manifest.package.repository;
      license = lib.licenses.mit;
      mainProgram = "exr";
      platforms = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
    };
  }
