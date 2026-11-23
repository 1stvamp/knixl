# Installing

knixl is a single binary. Pick whichever install suits you, then make sure the two things it calls out to (Nix and a Nix formatter) are on your `PATH`.

## With Nix

```sh
nix run github:1stvamp/knixl -- --help      # try it without installing
nix profile install github:1stvamp/knixl    # install into your profile
```

On NixOS or home-manager you can use the flake's overlay instead: add `overlays.default` to your nixpkgs overlays and put `pkgs.knixl` in `environment.systemPackages` or `home.packages`.

The flake build brings its own `nixfmt`, which knixl only uses when you don't already have one on your `PATH`.

## With cargo

```sh
cargo install knixl
```

## Prebuilt binaries

Every [release](https://github.com/1stvamp/knixl/releases/latest) has archives for Linux (glibc and musl) and macOS, on x86_64 and aarch64. There's also an installer script:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/1stvamp/knixl/releases/latest/download/knixl-installer.sh | sh
```

Each archive carries a build provenance attestation, so `gh attestation verify <archive> -R 1stvamp/knixl` confirms it was built by the project's release workflow.

## What knixl needs at runtime

- **A Nix formatter.** knixl formats everything it writes, so it needs `nixfmt` (or `nixfmt-rfc-style`) on your `PATH`. Set `KNIXL_FORMATTER` to use a specific binary. The formatter's version is recorded in the lockfile, because a different nixfmt can format the same code differently.
- **Nix.** Generating doesn't need Nix itself, but a few things do: `knixl install` uses it to check that the package you're adding exists (and builds, with `--build`), and validation needs a NixOS option set that Nix builds (see [Validation and the oracle](11-validation.md)). Without Nix, `install` skips its check with a warning, so you can still work.
- **git and network access, now and then.** Resolving a nixpkgs release to a commit uses `git ls-remote` (falling back to the GitHub API), and resolving a package version pin queries the nixhub version index over HTTPS. Both happen only in `knixl install` and `knixl upgrade`; `generate` and `check` work offline.

Check it all works:

```sh
knixl --version
```

Then carry on to [Your first host](03-first-host.md).
