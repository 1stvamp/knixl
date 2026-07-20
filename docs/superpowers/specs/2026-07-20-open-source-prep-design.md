# Open-sourcing knixl: design

Date: 2026-07-20
Status: draft for review

Prepare knixl for a public release: a licence, crates.io publishing of the whole workspace,
a tag-driven release workflow (binaries + crates), a Nix flake install path, repo metadata
and community files, and the CI the repo does not yet have. Snap was considered and dropped.

## Decisions (settled)

- **Licence:** dual `MIT OR Apache-2.0`. Copyright holder: Wesley Mason (also known as Wes
  Mason, 1stvamp).
- **crates.io:** publish the whole workspace, so `cargo install knixl` works and the libraries
  stay reusable (the LSP and GitHub-Action goals in CLAUDE.md).
- **CLI crate renamed** from `knixl-cli` to `knixl` (it is the main deliverable, and
  `cargo install knixl` reads better than `cargo install knixl-cli`).
- **Nix:** a flake in the repo now (`nix run`/`nix profile install` off any tag). A nixpkgs
  upstream submission is a documented follow-up, not built here.
- **Snap:** dropped. knixl shells out to host `nix`/`nixfmt`/`git`, which strict snap
  confinement blocks; classic confinement would work but needs manual Snapcraft-store
  approval. Not worth it for a Nix-centric audience. Recorded here so the decision is explicit.
- **Release targets:** Linux x86_64/aarch64 (gnu and musl static), macOS x86_64/aarch64.
- **CI:** added (none exists today).

## Crate rename

`knixl-cli` becomes `knixl`. The directory `crates/knixl-cli` becomes `crates/knixl`; the
`[[bin]] name = "knixl"` line becomes redundant (crate name already yields the `knixl` binary)
and is removed. Nothing depends on the CLI crate, so the blast radius is the workspace member
path, the crate name, and any doc references. The seven library crates keep their `knixl-*`
names.

## Licence

- `workspace.package.license = "MIT OR Apache-2.0"`.
- Add `LICENSE-MIT` (MIT text, `Copyright (c) 2026 Wesley Mason`) and `LICENSE-APACHE` (the
  Apache-2.0 text). Standard Rust dual-licence pair.
- Update the README licence section (currently "Intended: Apache-2.0 ... Not yet applied") to
  state the dual licence, with the conventional "licensed under either of ... at your option"
  wording and a contribution note.

## crates.io publishing

Per-crate metadata (the seven libs plus `knixl`):

- `description` (one line each), `keywords` (up to five), `categories`, `homepage`, `documentation`
  left to docs.rs. The `knixl` crate gets `readme = "../../README.md"` (or a crate-local README).
- Shared fields (`homepage`, `authors`, `repository`, `license`) hoisted into
  `workspace.package` where they are not already.
- Convert every internal path dependency to carry a version, e.g.
  `knixl-ir = { path = "../knixl-ir", version = "0.3.1" }`, so the published crates resolve
  their deps from the registry. (Path is used locally, version on the registry.)

Publish order (topological, verified from the Cargo.tomls):

```
knixl-ir → knixl-kdl → knixl-nix → knixl-oracle → knixl-lock → knixl-modules → knixl-pipeline → knixl
```

Use **`cargo-workspaces`** (`cargo ws publish`) for the publish step: it walks the workspace in
dependency order and waits for the index between crates, so we do not hand-script `cargo publish`
ordering and sleeps.

Before the first publish: verify `knixl` and every `knixl-*` name is free on crates.io. If any
is taken, decide a fallback prefix before tagging.

## Release workflow (tag-driven)

A single annotated tag `v<version>` drives the release. The version lives once in
`workspace.package.version`; releasing is: bump it, tag, push.

- **Binaries and GitHub Release:** `cargo-dist` (the `dist` tool). `dist init` writes
  `.github/workflows/release.yml` and a `[workspace.metadata.dist]` block. It cross-builds the
  six targets, produces tarballs + checksums, a shell installer script, and the GitHub Release.
  aarch64 builds run on native runners or via cross; musl via the `rust-musl` toolchain.
- **crates.io publish:** a job gated on the tag running `cargo ws publish` with a
  `CARGO_REGISTRY_TOKEN` repository secret. It can share `release.yml` (an added job) or sit in a
  sibling workflow triggered on the same tag. Both the binary build and the publish gate on CI
  being green.

## Nix flake

`flake.nix` at the repo root:

- Inputs: `nixpkgs` and a small `systems`/`flake-utils` helper for the four supported systems
  (x86_64/aarch64 Linux and Darwin).
- `packages.default` built with `rustPlatform.buildRustPackage`, `src = self`,
  `cargoLock.lockFile = ./Cargo.lock`, and `version` read from `Cargo.toml`
  (`(builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version`). Using the
  lockfile means there is no `cargoHash` to recompute, so a version bump needs no flake edit.
- `postInstall` wraps the binary (`wrapProgram $out/bin/knixl --prefix PATH : ...`) so
  `nixfmt-rfc-style` is on its PATH out of the box. `nix` itself is left to the environment (a
  NixOS/nix-darwin user already has it; wrapping it would pin a nix version into the closure).
- `overlays.default` (`final: prev: { knixl = ...; }`) and a `devShell` (the pinned Rust
  toolchain, `nixfmt-rfc-style`, `cargo-workspaces`, `cargo-dist`).
- **No NixOS service module.** knixl is a CLI, not a service, so a `services.knixl` module would
  be empty ceremony. NixOS/nix-darwin install is documented instead: add `overlays.default` and
  put `pkgs.knixl` in `environment.systemPackages` (or `home.packages`).

Install paths this enables: `nix run github:1stvamp/knixl`, `nix profile install
github:1stvamp/knixl`, and the overlay for system configs. All work off any pushed tag with no
per-release flake maintenance. nixpkgs upstream (a `pkgs/by-name/kn/knixl` submission, then the
nixpkgs-update bot for version bumps) is a follow-up, out of scope here.

## Repo metadata and community files

- **Cargo metadata:** as under crates.io above (descriptions, keywords, categories, homepage,
  authors).
- **GitHub repo:** set the description and topics via `gh repo edit` (topics e.g. `nix`,
  `nixos`, `kdl`, `rust`, `code-generation`, `configuration`).
- **README badges:** CI status, crates.io version, and licence.
- **Community health files:** `CONTRIBUTING.md` (build/test via mise or cargo, the no-`cargo fmt`
  caveat if it still holds, PR conventions), `SECURITY.md` (how to report, contact), and
  `CODE_OF_CONDUCT.md` (Contributor Covenant).

## CI (new)

`.github/workflows/ci.yml` on push and pull request:

- Install a Nix (for the golden and oracle-touching tests) and put `nixfmt-rfc-style` on PATH
  (set `KNIXL_FORMATTER`), then run `cargo build --workspace`, `cargo clippy --all-targets -D
  warnings`, and `cargo test --workspace`.
- The release jobs depend on this being green.

**Open item, needs a decision at review time:** the working tree is not currently
rustfmt-normalised (noted earlier this project). A `cargo fmt --check` gate would fail on day
one. Options: (a) leave fmt out of CI for now, (b) do a one-time `cargo fmt --all` normalisation
commit and then gate on it. Recommendation: (b), as a clearly separated first commit, so
contributors get the normal Rust fmt gate. Flagged rather than assumed because it is a large
mechanical diff the project deliberately deferred before.

## Out of scope

- nixpkgs upstream submission (documented follow-up).
- Snap (dropped, rationale above).
- A NixOS service module (knixl is a CLI).
- Homebrew tap or other installers beyond cargo-dist's shell installer (can be added later; the
  dist config makes it a one-line addition).

## Sequencing

Metadata and licence first (no behaviour, unblocks everything), then the crate rename, then CI,
then the flake, then the release tooling (cargo-dist + crates publish), then the GitHub repo
settings and community files, then a dry-run of the whole release before the first real tag.
