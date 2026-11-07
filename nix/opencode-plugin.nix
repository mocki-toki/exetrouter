{
  lib,
  buildNpmPackage,
}: let
  manifest = builtins.fromJSON (builtins.readFile ../clients/opencode-desktop/package.json);
in
  buildNpmPackage {
    pname = "exetrouter-opencode-plugin";
    version = manifest.version;
    src = lib.cleanSourceWith {
      src = lib.cleanSource ../clients/opencode-desktop;
      filter = path: type: !(type == "directory" && builtins.elem (builtins.baseNameOf path) ["node_modules" "dist"]);
    };
    npmDepsHash = "sha256-od1Cy4AjtV0LM++rAXQAZNdBl3SeVNVj5DbyYgtMYJw=";
    npmFlags = ["--ignore-scripts"];
    doCheck = true;
    checkPhase = ''
      runHook preCheck
      npm run typecheck
      npm test
      runHook postCheck
    '';
    installPhase = ''
      runHook preInstall
      npm prune --omit=dev --ignore-scripts --offline
      mkdir -p "$out"
      cp -r package.json src dist node_modules README.md "$out/"
      runHook postInstall
    '';
    doInstallCheck = true;
    installCheckPhase = ''
      runHook preInstallCheck
      node --input-type=module <<'JS'
      import assert from "node:assert/strict"
      import { existsSync, readFileSync } from "node:fs"
      import { join } from "node:path"
      const root = process.env.out
      const manifest = JSON.parse(readFileSync(join(root, "package.json"), "utf8"))
      assert.equal(manifest.name, "opencode-exetrouter")
      for (const entry of Object.values(manifest.exports)) assert.ok(existsSync(join(root, entry)))
      for (const name of Object.keys(manifest.dependencies)) assert.ok(existsSync(join(root, "node_modules", name)))
      assert.ok(!existsSync(join(root, "node_modules", "typescript")))
      JS
      runHook postInstallCheck
    '';
    meta = {
      description = "ExetRouter provider and experimental GUI plugin for OpenCode V2";
      homepage = "https://github.com/mocki-toki/exetrouter";
      license = lib.licenses.mit;
      platforms = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
    };
  }
