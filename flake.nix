{
  description = "Compile opinionated KDL into maintainable, committed NixOS module source.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        knixl = pkgs.callPackage ./nix/package.nix { };
      in {
        packages.default = knixl;
        packages.knixl = knixl;
        checks.knixl = knixl;
        devShells.default = pkgs.mkShell {
          inputsFrom = [ knixl ];
          packages = [ pkgs.nixfmt pkgs.cargo-workspaces pkgs.cargo-dist ];
        };
      })
    // {
      overlays.default = final: prev: {
        knixl = final.callPackage ./nix/package.nix { };
      };
    };
}
