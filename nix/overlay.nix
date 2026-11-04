final: _prev: {
  exetrouter = final.callPackage ./package.nix {};
  exetrouter-opencode-plugin = final.callPackage ./opencode-plugin.nix {};
}
