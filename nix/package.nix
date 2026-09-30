# knixl, in the callPackage shape nixpkgs uses, so it can move to
# pkgs/by-name/kn/knixl/package.nix with `src` swapped for a fetchFromGitHub of the release tag
# (and `cargoLock` for a `cargoHash`).
{
  lib,
  rustPlatform,
  makeWrapper,
  nixfmt,
}:

rustPlatform.buildRustPackage {
  pname = "knixl";
  version = (lib.importTOML ../Cargo.toml).workspace.package.version;

  # Only what the build and tests read (the tests use the goldens in examples/), so an edit to
  # docs/ or site/ doesn't rebuild and re-test the package.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../examples
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ makeWrapper ];

  # knixl formats what it generates with nixfmt. The bundled one goes on the end of PATH, so a
  # nixfmt the user already has wins: a project's lock records its formatter version, and a
  # bundled one taking over would show up there as version skew.
  postInstall = ''
    wrapProgram $out/bin/knixl --suffix PATH : ${lib.makeBinPath [ nixfmt ]}
  '';

  meta = {
    description = "Generate maintainable, human-readable Nix from small amounts of opinionated KDL";
    homepage = "https://knixl.dev";
    changelog = "https://github.com/1stvamp/knixl/blob/main/CHANGELOG.md";
    license = with lib.licenses; [
      mit
      asl20
    ];
    mainProgram = "knixl";
  };
}
