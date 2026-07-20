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
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        knixl = pkgs.rustPlatform.buildRustPackage {
          pname = "knixl";
          version = cargoToml.workspace.package.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.makeWrapper ];
          postInstall = ''
            wrapProgram $out/bin/knixl \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.nixfmt-rfc-style ]}
          '';
          meta = with pkgs.lib; {
            description = cargoToml.workspace.package.description or
              "Compile opinionated KDL into maintainable, committed NixOS module source.";
            homepage = "https://github.com/1stvamp/knixl";
            license = with licenses; [ mit asl20 ];
            mainProgram = "knixl";
          };
        };
      in {
        packages.default = knixl;
        packages.knixl = knixl;
        devShells.default = pkgs.mkShell {
          inputsFrom = [ knixl ];
          packages = [ pkgs.nixfmt-rfc-style pkgs.cargo-workspaces pkgs.cargo-dist ];
        };
      })
    // {
      overlays.default = final: prev: {
        knixl = self.packages.${final.system}.default;
      };
    };
}
