# Open-sourcing knixl: implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make knixl publishable and installable in the open: dual licence, whole-workspace
crates.io publishing (CLI crate renamed to `knixl`), a tag-driven release workflow (binaries +
crates), a Nix flake install path, repo metadata and community files, and CI.

**Architecture:** Metadata and licence first, then the crate rename, then CI, then the flake,
then release tooling (cargo-dist for binaries + cargo-workspaces for crates), then GitHub repo
settings and community files, then a release dry-run. One shared workspace version drives every
crate and the flake.

**Tech Stack:** Rust workspace (8 crates), GitHub Actions, cargo-dist, cargo-workspaces, Nix
flake (`rustPlatform.buildRustPackage`).

## Global Constraints

- Prose (docs, comments, READMEs, community files, commit messages): British spelling; no
  em-dashes or en-dashes (colons, parentheses, commas, full stops); no banned vocabulary
  (passionate, leverage, robust, seamless, delve, comprehensive, streamline, unlock, realm,
  landscape, tapestry, testament, elevate, empower, "it's not just X, it's Y").
- **From Task 1 onward the tree is rustfmt-normalised.** Run `cargo fmt --all` after Rust edits
  and keep `cargo fmt --all --check` clean. (This reverses the project's earlier "do not run
  cargo fmt" rule, which existed only because the tree was not normalised.)
- Shared version: `workspace.package.version` (currently `0.3.1`) is the single source of truth
  for all crates and the flake. Do not add per-crate versions.
- Licence SPDX everywhere: `MIT OR Apache-2.0`. Copyright holder: `Wesley Mason`.
- Never commit or run git/but in a task; the controller commits.
- Do not run `cargo publish`, `gh repo edit`, `dist build`, or push tags in any task. Publishing
  and live-repo edits happen only in the final, explicitly-gated release step with confirmation.

---

### Task 1: Normalise formatting

**Files:** every Rust file (mechanical), plus a `rustfmt.toml` if defaults are not wanted.

- [ ] **Step 1: Baseline build is green**

Run: `cargo build --workspace --tests`
Expected: PASS (records the pre-format state).

- [ ] **Step 2: Normalise**

Run: `cargo fmt --all`

- [ ] **Step 3: Verify nothing broke**

Run: `cargo fmt --all --check` (expect clean), then `cargo build --workspace --tests` and
`cargo test --workspace` (expect PASS). If the format pass reveals a genuine compile issue,
stop and report; it should be whitespace-only.

- [ ] **Step 4: No commit.** (Controller commits this as a single mechanical "chore: normalise
  formatting" commit, kept separate from every other task.)

---

### Task 2: Dual licence

**Files:**
- Create: `LICENSE-MIT`, `LICENSE-APACHE`
- Modify: `Cargo.toml` (`workspace.package.license`), `README.md` (licence section)

- [ ] **Step 1: Add licence texts**

`LICENSE-MIT`: the standard MIT text, first line `Copyright (c) 2026 Wesley Mason`.
`LICENSE-APACHE`: the verbatim Apache License 2.0 text (the standard boilerplate, unmodified).

- [ ] **Step 2: Set the SPDX**

In `Cargo.toml` `[workspace.package]` change `license = "Apache-2.0"` to
`license = "MIT OR Apache-2.0"`.

- [ ] **Step 3: README licence section**

Replace the current "Intended: Apache-2.0 ... Not yet applied" with the conventional dual note:

```markdown
## Licence

Licensed under either of Apache License, Version 2.0 (LICENSE-APACHE) or the MIT licence
(LICENSE-MIT) at your option. Unless you state otherwise, any contribution you submit for
inclusion is dual licensed as above, with no additional terms.
```

- [ ] **Step 4: Verify**

Run: `cargo metadata --format-version 1 --no-deps | grep -o '"license":"[^"]*"' | sort -u`
Expected: every crate shows `"license":"MIT OR Apache-2.0"`.

- [ ] **Step 5: No commit.**

---

### Task 3: Rename the CLI crate to `knixl`

**Files:**
- Rename: directory `crates/knixl-cli` → `crates/knixl`
- Modify: `Cargo.toml` (workspace `members`), `crates/knixl/Cargo.toml` (`name`, drop redundant
  `[[bin]]`), any doc/comment references to the `knixl-cli` crate name

- [ ] **Step 1: Move the directory and update the member path**

`git mv crates/knixl-cli crates/knixl` (use `git mv` so history follows; the controller owns
git, so if `git mv` is disallowed here, a plain move plus telling the controller is fine). In
the root `Cargo.toml`, change the member `"crates/knixl-cli"` to `"crates/knixl"`.

- [ ] **Step 2: Rename the package**

In `crates/knixl/Cargo.toml` set `name = "knixl"`. The crate name now yields the `knixl`
binary, so remove the redundant block:

```toml
[[bin]]
name = "knixl"
path = "src/main.rs"
```

(Only remove it if the source entry point is the default `src/main.rs`. If `path` differs, keep
a `[[bin]]` with the correct `path` but the name is still `knixl`.)

- [ ] **Step 3: Fix references**

Grep for the old crate name and update prose/comments (not the library crates, which stay
`knixl-*`): `grep -rn "knixl-cli" --include=*.rs --include=*.toml --include=*.md .`
Update any that name the CLI *crate*. Leave unrelated matches alone.

- [ ] **Step 4: Verify**

Run: `cargo build --workspace` then `./target/debug/knixl --help`
Expected: builds; help prints the command list. `cargo metadata --no-deps --format-version 1 |
grep -o '"name":"knixl"'` shows the package exists.

- [ ] **Step 5: No commit.**

---

### Task 4: Crate metadata and versioned internal deps

**Files:** `Cargo.toml` (workspace.package), all 8 `crates/*/Cargo.toml`

- [ ] **Step 1: Shared metadata in `workspace.package`**

Add to `[workspace.package]`:

```toml
authors = ["Wesley Mason <wes@1stvamp.org>"]
homepage = "https://github.com/1stvamp/knixl"
```

(`repository` and `license` already present. Confirm the email or replace with the preferred
public contact before publish.)

- [ ] **Step 2: Per-crate description, keywords, categories**

Add `description`, `keywords`, `categories`, and `homepage.workspace = true` /
`authors.workspace = true` / `repository.workspace = true` to each crate's `[package]`. Use these:

| crate | description | keywords | categories |
|-------|-------------|----------|------------|
| knixl-ir | knixl IR: a constrained subset of Nix (module bodies) with a deterministic emitter. | nix, kdl, codegen | development-tools |
| knixl-kdl | KDL input-parsing helpers for knixl, over the kdl crate. | kdl, nix, parsing | development-tools, parsing |
| knixl-oracle | Validate emitted NixOS option paths against the real option set. | nix, nixos, validation | development-tools |
| knixl-nix | nixfmt invocation and content hashing for knixl. | nix, nixfmt, hashing | development-tools |
| knixl-lock | knixl's lockfile model and reconcile/drift state machine. | nix, lockfile, reproducibility | development-tools |
| knixl-modules | The knixl Module trait, built-in modules, and the declarative KDL module loader. | nix, kdl, modules | development-tools |
| knixl-pipeline | knixl's generation pipeline: KDL in, formatted Nix files out. | nix, kdl, codegen | development-tools |
| knixl | Compile opinionated KDL into maintainable, committed NixOS module source. | nix, nixos, kdl, configuration | command-line-utilities, development-tools |

The `knixl` crate also gets `readme = "../../README.md"`.

- [ ] **Step 3: Version the internal path deps**

In every crate that depends on a sibling, add `version = "0.3.1"` beside the path, e.g. in
`crates/knixl-oracle/Cargo.toml`:

```toml
knixl-ir = { path = "../knixl-ir", version = "0.3.1" }
```

Do this for all internal deps across knixl-oracle, knixl-lock, knixl-modules, knixl-pipeline,
knixl (the graph: oracle→ir; lock→nix; modules→ir,kdl,oracle; pipeline→ir,kdl,lock,modules,nix,
oracle; knixl→all).

- [ ] **Step 4: Verify metadata and a leaf dry-run**

Run: `cargo build --workspace` (PASS). Then a leaf-crate package check that needs no unpublished
deps: `cargo package -p knixl-ir --allow-dirty --no-verify` (expect it to assemble a `.crate`;
this confirms the metadata is publish-valid). Also
`cargo metadata --no-deps --format-version 1 | grep -c '"description":null'` should be `0`.

- [ ] **Step 5: No commit.**

---

### Task 5: CI workflow

**Files:** Create `.github/workflows/ci.yml`

- [ ] **Step 1: Write the workflow**

```yaml
name: CI
on:
  push:
    branches: [main]
  pull_request:
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.87.0
        with:
          components: rustfmt, clippy
      - uses: Swatinem/rust-cache@v2
      - uses: DeterminateSystems/nix-installer-action@main
      - name: Provide nixfmt
        run: |
          nix profile install nixpkgs#nixfmt-rfc-style
          echo "KNIXL_FORMATTER=$(command -v nixfmt)" >> "$GITHUB_ENV"
      - run: cargo fmt --all --check
      - run: cargo clippy --all-targets -- -D warnings
      - run: cargo build --workspace --tests
      - run: cargo test --workspace
```

- [ ] **Step 2: Validate locally**

The workflow cannot run GitHub Actions locally, so validate two ways: (a) if `actionlint` is
available, run `actionlint .github/workflows/ci.yml` (expect no errors); otherwise
`python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/ci.yml'))"` for syntax.
(b) Re-run the workflow's own commands locally to prove they pass on this tree:
`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`,
`KNIXL_FORMATTER=$(command -v nixfmt) cargo test --workspace`.

- [ ] **Step 3: No commit.**

---

### Task 6: Nix flake

**Files:** Create `flake.nix` (and let `nix` write `flake.lock`)

- [ ] **Step 1: Write the flake**

```nix
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
```

(Note: `description` is not in `workspace.package` today, so the `or` fallback covers it. If Task
4 adds a workspace-level description, the `or` still works.)

- [ ] **Step 2: Build and check**

Run: `nix build .#default` then `./result/bin/knixl --help` (expect the help output, with
`nixfmt` available on its wrapped PATH). Then `nix flake check` (expect no errors).

- [ ] **Step 3: No commit.** (Controller commits `flake.nix` and the generated `flake.lock`.)

---

### Task 7: Release tooling (cargo-dist + crates publish)

**Files:** `Cargo.toml` (`[workspace.metadata.dist]`), `.github/workflows/release.yml` (dist
generates it), a crates-publish job

- [ ] **Step 1: Initialise cargo-dist**

Run `dist init` (the `dist`/cargo-dist tool). Choose: GitHub CI, a shell installer, and the
targets `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-apple-darwin`,
`aarch64-apple-darwin`. This writes `[workspace.metadata.dist]` and
`.github/workflows/release.yml`. Confirm the `[workspace.metadata.dist]` block lists exactly
those six targets and that `install-path` and checksum settings are the defaults.

- [ ] **Step 2: Add the crates.io publish job**

Append a job to `.github/workflows/release.yml` (or a sibling `publish-crates.yml` triggered on
the same `v*` tags) that publishes in dependency order:

```yaml
  publish-crates:
    needs: [plan]           # gate on dist's plan/verify job; adjust to the generated job name
    runs-on: ubuntu-latest
    if: startsWith(github.ref, 'refs/tags/v')
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.87.0
      - run: cargo install cargo-workspaces
      - run: cargo ws publish --from-git --yes --no-git-commit --token "$CARGO_REGISTRY_TOKEN"
        env:
          CARGO_REGISTRY_TOKEN: ${{ secrets.CARGO_REGISTRY_TOKEN }}
```

(`cargo ws publish` walks the workspace in dependency order and waits for the index between
crates. Confirm the exact flags against the installed cargo-workspaces version.)

- [ ] **Step 3: Validate without releasing**

Run: `dist plan` (expect it to enumerate the six targets and the artifacts with no error).
Validate the added YAML with `actionlint` or a YAML parse. Do NOT run `dist build` for real
targets or publish anything.

- [ ] **Step 4: No commit.**

---

### Task 8: Repo metadata and community files

**Files:** Create `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`; modify `README.md`
(badges). GitHub repo settings are applied at release time, not here.

- [ ] **Step 1: Community files**

- `CODE_OF_CONDUCT.md`: the Contributor Covenant v2.1 text, contact set to the public email.
- `SECURITY.md`: how to report (email the maintainer privately, expected response window), which
  versions are supported.
- `CONTRIBUTING.md`: prerequisites (Rust 1.87 via rustup, Nix, `nixfmt-rfc-style`); the mise
  tasks (`mise run build`/`test`/`lint`/`fmt`); that the tree is rustfmt-normalised so
  `cargo fmt --all` must stay clean; the golden tests are the acceptance tests; PR expectations;
  the dual-licence contribution note.

- [ ] **Step 2: README badges**

Add, under the title, badges for CI (`actions/workflows/ci.yml/badge.svg`), crates.io version
(`img.shields.io/crates/v/knixl`), and licence (`img.shields.io/crates/l/knixl`).

- [ ] **Step 3: Record the GitHub repo settings to apply at release**

In the release doc (Task 9), record the exact commands to run once, at go-public time (they edit
the live repo, so they are not run in this task):

```
gh repo edit 1stvamp/knixl \
  --description "Compile opinionated KDL into maintainable, committed NixOS module source." \
  --homepage "https://github.com/1stvamp/knixl" \
  --add-topic nix --add-topic nixos --add-topic kdl --add-topic rust \
  --add-topic code-generation --add-topic configuration
```

- [ ] **Step 4: Verify**

Files exist and contain no banned vocabulary or dashes (`grep -nP '[\x{2013}\x{2014}]'` clean).
README renders (optional local render). No live-repo commands run.

- [ ] **Step 5: No commit.**

---

### Task 9: Release process doc and dry-run

**Files:** Create `RELEASING.md`

- [ ] **Step 1: Write the release runbook**

`RELEASING.md`: the one-time setup (`CARGO_REGISTRY_TOKEN` secret; verify crate names are free
on crates.io; the `gh repo edit` block from Task 8; making the repo public), and the per-release
steps (bump `workspace.package.version`, `cargo test --workspace`, commit, `git tag vX.Y.Z`,
push the tag, watch the release workflow, confirm crates.io + the GitHub Release + `nix run
github:1stvamp/knixl` all resolve the new version).

- [ ] **Step 2: Dry-run what can be dry-run**

Run (no publishing, no tags): `dist plan`; `cargo ws publish --dry-run` if the installed version
supports it (else `cargo package --workspace --no-verify` to prove all crates assemble);
`nix build .#default`. Record the outcomes in the runbook.

- [ ] **Step 3: Crate-name availability check**

For each of `knixl`, `knixl-ir`, `knixl-kdl`, `knixl-oracle`, `knixl-nix`, `knixl-lock`,
`knixl-modules`, `knixl-pipeline`, check crates.io (`curl -s
https://crates.io/api/v1/crates/<name> | grep -o '"detail":"Not Found"'` means free). Record
which are free; if any is taken, stop and raise it before the first publish.

- [ ] **Step 4: No commit.**

---

## Self-Review

- Spec coverage: Task 1 normalise (spec's open item, resolved to normalise-and-gate); Task 2
  licence; Tasks 3-4 crate rename + crates.io metadata/versioned deps; Task 5 CI; Task 6 flake;
  Task 7 release workflow (binaries via cargo-dist + crates via cargo-workspaces); Task 8 repo
  metadata + community files; Task 9 release runbook + dry-run + name check. Snap and nixpkgs
  upstream are out of scope per the spec.
- Placeholders: none; each config/file has concrete content or an exact command. Values needing
  a human confirm (public contact email, crate-name availability) are called out as explicit
  checks, not left vague.
- Ordering: normalise before CI (so the fmt gate passes), rename before metadata (so metadata
  names the `knixl` crate), everything before the release dry-run. No task publishes, tags, or
  edits the live repo; those are gated to the runbook and done with confirmation.
- Version consistency: the single `workspace.package.version` drives crates, cargo-dist, and the
  flake (which reads it from Cargo.toml), so a release is one version bump.
