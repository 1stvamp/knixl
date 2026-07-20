# Releasing knixl

This is the runbook for cutting a knixl release: crates.io publish plus the
GitHub binary release via `dist`. Read it top to bottom before the first
release; after that, the "Per release" section is all you need.

## One-time setup

Do these once, before the first tagged release. None of them are reversible
without contacting GitHub/crates.io support, so double-check before running.

### 1. `CARGO_REGISTRY_TOKEN` secret

Done: the secret is set on the repo. For reference, or to rotate it:

`publish-crates.yml` publishes with `cargo ws publish --token
"$CARGO_REGISTRY_TOKEN"`, reading the token from a repo secret. Generate a
crates.io API token (crates.io account settings, scope: publish-new and
publish-update are enough, no need for full account access) and add it:

```sh
gh secret set CARGO_REGISTRY_TOKEN --repo 1stvamp/knixl
```

(pastes the token from stdin/prompt; do not put it on the command line where
it lands in shell history).

### 2. Crate-name availability

Checked against the live crates.io API on 2026-07-20 (`curl -s -A
"<contact>" https://crates.io/api/v1/crates/<name>`; a `does not exist` error
detail means free, a `"crate": {...}` body means taken):

| Crate            | Result |
|-------------------|--------|
| `knixl`           | free   |
| `knixl-ir`        | free   |
| `knixl-kdl`       | free   |
| `knixl-oracle`    | free   |
| `knixl-nix`       | free   |
| `knixl-lock`      | free   |
| `knixl-modules`   | free   |
| `knixl-pipeline`  | free   |

All eight are free as of this check. No blocker. Re-run this check
immediately before the first publish, not just at doc-writing time, since
availability can change: someone else could register one of these names in
the meantime. If any comes back taken, stop; do not publish any of the eight
until the naming clash is resolved (rename the crate before it is a public
1.0, since it is a much smaller change now than after other projects start
depending on the name).

Note: crates.io's "not found" response text has changed since the tool that
originally specified this check was written. It no longer returns
`"detail":"Not Found"`; it now returns `{"errors":[{"detail":"crate `<name>`
does not exist"}]}`. Match on `does not exist`, not the old string. Also send
a descriptive `User-Agent` (`-A`); crates.io's API returns a data-access-policy
error and refuses the request without one.

### 3. `gh repo edit` (repo metadata and topics)

Run once, at go-public time, after the repo is confirmed public (order
matters less than doing both before announcing, but public-first avoids
topics/description showing on a 404):

```sh
gh repo edit 1stvamp/knixl \
  --description "Compile opinionated KDL into maintainable, committed NixOS module source." \
  --homepage "https://github.com/1stvamp/knixl" \
  --add-topic nix --add-topic nixos --add-topic kdl --add-topic rust \
  --add-topic code-generation --add-topic configuration
```

Not run by this doc or by any task in this series. Record only; run by hand
when actually going public.

### 4. Make the repo public

`gh repo edit 1stvamp/knixl --visibility public`, or via the GitHub UI
(Settings > General > Danger Zone > Change visibility). Confirm before
running: this is not easily reversible in spirit (forks and clones already
made stay out there even if you flip it back to private).

The maintainer contact used throughout (`SECURITY.md`, `CODE_OF_CONDUCT.md`,
crate `authors`) is `wes@1stvamp.org`, confirmed correct. It will receive
security reports and conduct escalations once the repo is public.

## Per release

One version bump drives everything: `workspace.package.version` in the root
`Cargo.toml`. Crates, `dist`, and the flake (which reads the version out of
`Cargo.toml` at eval time) all follow it, so there is exactly one number to
change.

1. Bump `workspace.package.version` in `Cargo.toml`.
2. `cargo test --workspace`. Also worth running `cargo fmt --all --check` and
   `cargo clippy --all-targets -- -D warnings` (what CI checks on every PR),
   since a release tag should not be the first place these run.
3. Commit: `git commit -am "chore(release): vX.Y.Z"` (or however the version
   bump was staged).
4. Tag: `git tag vX.Y.Z`.
5. Push both: `git push && git push --tags`.
6. Watch the two workflows this triggers on the `v*` tag:
   - `release.yml` (`dist`-generated): builds the six target archives, the
     shell installer, and creates the GitHub Release.
   - `publish-crates.yml`: publishes all eight crates to crates.io in
     dependency order via `cargo ws publish --from-git`.

   `gh run watch` against the latest run for each workflow, or watch in the
   Actions tab.
7. Verify the release actually landed:
   - crates.io: `curl -s -A "<contact>" https://crates.io/api/v1/crates/knixl
     | grep '"newest_version"'` shows `X.Y.Z`.
   - GitHub Release: `gh release view vX.Y.Z --repo 1stvamp/knixl`.
   - The flake resolves the new tag: `nix run github:1stvamp/knixl -- --version`
     prints `knixl X.Y.Z`.

## Dry-runs already done (this doc, before first publish)

These were run against the committed tree with no publishing, no tags, no
`dist build`. Recorded here so the first real release is not the first time
any of this has been exercised.

### `dist plan`

Exit 0. Enumerates the six configured targets (`aarch64-apple-darwin`,
`aarch64-unknown-linux-gnu`, `aarch64-unknown-linux-musl`,
`x86_64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`x86_64-unknown-linux-musl`), one `knixl` binary archive per target plus a
source tarball and shell installer, all under `announcing v0.3.1`. Matches
Task 7's earlier `dist plan` run; unchanged since.

### Crate packaging

`cargo package --workspace --no-verify` fails partway through, and this is
expected, not a bug: `cargo package` regenerates each crate's packaged
manifest against the live crates.io index, and for a crate with a workspace
path dependency (`{ path = "...", version = "0.3.1" }`) it needs that sibling
to already be resolvable from the registry, not just from the local path. It
is not there yet, because nothing in this workspace has been published.

Packaged individually to see exactly which crates that affects:

| Crate            | `cargo package -p <crate> --no-verify` |
|-------------------|------------------------------------------|
| `knixl-ir`        | succeeds (no workspace deps)             |
| `knixl-kdl`       | succeeds (no workspace deps)              |
| `knixl-nix`       | succeeds (no workspace deps)              |
| `knixl-oracle`    | fails: needs `knixl-ir` on the registry  |
| `knixl-lock`      | fails: needs `knixl-nix` on the registry |
| `knixl-modules`   | fails: needs `knixl-ir` on the registry  |
| `knixl-pipeline`  | fails: needs `knixl-ir` on the registry  |
| `knixl`           | fails: needs `knixl-ir` on the registry  |

The three leaf crates (no `knixl-*` dependency) package cleanly and prove
their manifests, file lists, and included assets are publishable as-is. The
other five cannot be fully packaging-verified until their dependencies are
actually live on crates.io, which only happens during the real publish
sequence. This is exactly why `cargo-workspaces` publishes leaf-first, in
dependency order, with a wait for registry propagation between crates, rather
than all at once: it is solving the same ordering problem this dry-run hit.
`cargo build --workspace` and `cargo test --workspace` (via path deps, no
registry lookup involved) already prove the interdependent crates compile
and pass tests; what is unverified until the real publish is only the
packaging/manifest step for the five non-leaf crates.

`cargo ws publish --dry-run` could not be run locally: `cargo install
cargo-workspaces` needs rustc 1.88+ to compile (its 0.4.2 dependency tree:
`home`, `time`, `kstring`), and this repo's `rust-toolchain.toml` pins 1.87.0.
The publish job does not need the build floor, so `publish-crates.yml` installs
`cargo-workspaces` under `dtolnay/rust-toolchain@stable` (publishing against a
newer toolchain is fine, since `rust-version` is a minimum, not a ceiling). The
interdependent crates are already proven to compile and pass tests via
`cargo build`/`cargo test --workspace` (path deps, no registry); the only step
unverified until the real publish is the packaging of the five non-leaf crates,
which `cargo ws publish` handles leaf-first in CI.

### Flake

Not a full `nix build` (already verified in Task 6; a cold build is slow and
would not tell us anything new). Ran the two cheap checks instead, against
the committed tree (no `path:` workaround needed this time, since `flake.nix`
is committed):

- `nix build --dry-run .#default`: exit 0, one derivation to build
  (`knixl-0.3.1.drv`), no evaluation errors. One pre-existing upstream
  warning (`nixfmt-rfc-style is now the same as pkgs.nixfmt`), unrelated to
  this flake.
- `nix flake show`: exit 0. Lists `devShells.<system>.default`,
  `overlays.default`, `packages.<system>.default`, `packages.<system>.knixl`
  for `x86_64-linux` (other systems shown as "omitted", the normal
  single-machine-evaluation behaviour, not a failure).

Both confirm the flake evaluates cleanly straight from the committed tree.

## Files this touches per release

- `Cargo.toml` (`workspace.package.version`) : the only hand-edit.
- `Cargo.lock` : updates automatically on the next `cargo build`/`cargo test`
  after the bump; commit it alongside.
- Everything else (`flake.lock`, the six `dist` archives, the GitHub Release,
  the eight crates.io entries) is generated by the tag push, not hand-edited.
