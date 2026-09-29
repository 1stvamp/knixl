# ADR 0014: Flake inputs in the system flake

Status: accepted

Refines: ADR 0007 (per-host baseline nixpkgs rev) and ADR 0008 (out-of-tree oracle modules).
Supersedes in part: ADR 0009 (system assembly flake). Relates to: ADR 0005 (pinning), ADR 0006
(automatic pin strategy selection), ADR 0012 (installer media), ADR 0013 (guest-image targets).

## Context

With `system {}` declared (ADR 0009), knixl writes `generated/flake.nix`, pinning nixpkgs per
host by `builtins.fetchGit` and building each configuration through `nixos/lib/eval-config.nix`.
That flake has no inputs. A system that also depends on flake-provided NixOS modules (disko,
sops-nix, impermanence and so on) still needs a hand-written flake wrapped around the generated
modules to bring those inputs in, and that hand-written flake sits outside `output = f(kdl, ...)`
in the same way the whole assembly did before ADR 0009.

A user migrating an existing flake-based system onto knixl would like to delete that
hand-written flake and have knixl generate all of it: the inputs, the NixOS modules they provide,
and the `nixosConfigurations` that use them. For the migration to be safe, the generated flake
has to produce a byte-identical system toplevel to the flake it replaces, so the switch can be
checked with a store-path comparison before anything is deployed.

Two things in the current design get in the way. ADR 0009 made the flake pure and input-free on
purpose (knixl owns the lock, and a `flake.lock` would be a second one), so there is nowhere to
declare an input. And an eval-config-built system is expected to differ from a
`lib.nixosSystem`-built one even when every module is the same: as far as we understand it, the
flake's `nixosSystem` sets `system.nixos.revision`, `system.nixos.versionSuffix` and
`nixpkgs.flake.source` (which feeds `nix.registry` and `nix.nixPath`), and eval-config does not.

ADR 0008 already lets a project declare flake-sourced modules, but only for validation: the
oracle resolves them and builds an augmented `options.json`, and the generated flake never
imports them. ADR 0007 deferred pinning a baseline to an exact commit, which is what a migration
needs (a running system was built from a specific commit, rarely the current tip of its channel).

## Decision

When `system {}` declares `input` nodes, knixl generates a flake with those inputs, pinned
exactly by `knixl.lock.kdl`, and builds hosts and images with `nixpkgs.lib.nixosSystem`. Without
them, emission is unchanged.

- **Flake inputs are declared in `system {}`**: `input "<name>" url="<flake ref>" [rev="<commit>"]
  [flake=#false] { follows nixpkgs="nixpkgs" }`. Each `follows` child maps an input of this input (the key) to
  one of the project's own inputs (the value), emitted as
  `inputs.<name>.inputs.<key>.follows = "<value>"`. `flake=#false` emits `flake = false` for a
  non-flake source. `nixpkgs` is itself an input and is required once any input is declared. Its
  `url` carries no ref (its rev is the baseline, below), and it takes the place of the existing
  `system {}` child `nixpkgs-url`, so declaring both is refused.
- **`knixl.lock.kdl` stays the single source of truth**: `upgrade` resolves every input other
  than `nixpkgs` to a full 40-character rev. A declared `rev=` is recorded as written (a migration
  needs the exact commits a running system was built from, which are rarely the current tips);
  anything else goes through the resolver oracle modules already use (ADR 0008:
  `KNIXL_MODULE_RESOLVER` when set, otherwise the built-in `git ls-remote`, which handles
  `github:` refs and pins HEAD, so a `github:` url carrying a branch or tag is refused). A rev
  that is not 40 hex characters is refused. `install` doesn't resolve inputs (it never needs
  them); an unresolved input refuses generation with a message pointing at `upgrade`. The pin is recorded in
  the same shape as an `oracle-module` line, as a proposed top-level
  `flake-input name="<name>" url="<url>" rev="<rev>"` line (the lock's `input` node name is
  already taken by the KDL input file hashes). The generated flake writes that rev into each
  input's URL (e.g. `github:owner/repo/<rev>`), so the `flake.lock` nix writes is determined by
  `knixl.lock.kdl`: `nix flake update` has nothing to move, and transitive inputs come from each
  upstream's own `flake.lock` at that fixed rev. `generated/flake.lock` sits beside
  `generated/flake.nix` and is nix-owned: it must be committed, it is not drift-hashed or listed
  as a knixl output, and `--prune` leaves it alone.
- **`knixl check` reads `flake.lock`**: it fails when the locked `nixpkgs` rev differs from the
  `knixl.lock.kdl` baseline, or when `flake.lock` is missing. That catches a `flake.lock`
  re-locked or edited outside knixl (a stray `nix flake update` with an override, say), and one
  left stale after an `upgrade` moved the baseline.
- **The nixpkgs input is the baseline**: its rev is the baseline rev (ADR 0007), so oracle
  validation, pin feasibility (ADR 0006) and the build all see the same nixpkgs by construction.
  There is one `nixpkgs` input per flake, so in input mode every host must share one baseline
  rev, and a project whose hosts differ is refused (exit 5). With `system {}` every host already
  declares its own baseline (ADR 0009), so the shared host rev is the input's rev; image targets
  use it too, and only a project with no hosts falls back to the lock's `oracle` rev. ADR 0007's deferral is lifted: `nixpkgs release="<rel>" rev="<full rev>"` pins a
  baseline to an exact commit, such as the one a running system was built from. `install` and
  `upgrade` record the declared rev as `nixpkgs-rev` without resolving the release branch, and
  `upgrade` never moves it (only a KDL edit does). `release=` stays required beside `rev=`,
  since the lock's `baseline` line records both and keeps its current shape. The oracle's
  `options.json` for that rev comes from the existing augmented build path (ADR 0008), run with
  the declared module set, which may be empty.
- **Hosts and images are built with `nixpkgs.lib.nixosSystem`** in input mode, replacing
  `fetchGit` plus `eval-config.nix`. ADR 0009's correction still holds for a plainly imported
  nixpkgs; `lib.nixosSystem` is a flake lib extension, and it is available here because nixpkgs
  is a flake input. We expect this to make the generated toplevel match a flake-built one,
  because of what `nixosSystem` sets (Context, above); the byte-identical migration test has to
  confirm that before this ADR is accepted. Hosts and installers pass `system` (their platform is
  declared); guest images do not, since the guest-image module sets the platform, which matches
  common flake usage. The regression test in `crates/knixl-pipeline/src/flake.rs` that forbids
  `lib.nixosSystem` keeps guarding the input-free path.
- **An oracle module may name an input**: `module "disko" input="disko" attr="disko"` in an
  `oracle-modules` block names a declared input in place of `flake=`. One declaration then drives
  both oracle validation (resolved from the input's pin, so the `oracle-module` lock line records
  the input's rev, with its url in the `https://github.com/<owner>/<repo>` form the options
  build fetches, and the options cache key is unchanged) and
  `modules = [ inputs.disko.nixosModules.disko ]` in the generated flake. `attr` defaults to
  `"default"`, as it does today. Naming an undeclared input is an error. Input modules apply to
  hosts only, never to installer or guest-image targets. A host's own `oracle-modules` block
  still replaces the project default in full (ADR 0008), and the replacement set is the one that
  host imports. A `flake=` entry stays validation-only.
- **sops-nix wiring**: the existing top-level `secrets` node gains `input=` and children:
  `secrets backend="sops-nix" input="<name>" { default-file "<path>"; ssh-key-paths "<path>" }`.
  For each host, knixl adds `inputs.<name>.nixosModules.sops` to the host's modules, sets
  `sops.defaultSopsFile` and `sops.age.sshKeyPaths`, and emits `sops.secrets."<name>" = { }` for
  every `(secret)` the host references, in sorted order. It is an error if the named input is
  not declared, and `input=` with the `agenix` backend is refused for now. Without `input=`,
  `secrets` behaves as it does today (it only chooses what a `(secret)` reference resolves to).
- **An optional `formatter.<system>` output**: a proposed `formatter "<attr>"` child of
  `system {}` emits `formatter.<system> = nixpkgs.legacyPackages.<system>.<attr>` for each system
  the flake's hosts target, so `nix fmt` works on the project. It is separate from the formatter
  knixl pins in its lock for its own output.
- **Opt-in and backwards compatible**: a `system {}` with no `input` nodes keeps today's
  `fetchGit` and `eval-config.nix` emission byte for byte, so existing projects and the
  `examples/` goldens don't change. We think that is the right call. Moving every `system {}`
  project to inputs would rewrite every generated flake and add a `flake.lock` to projects that
  never asked for one, and for a system with no flake-provided modules the input-free flake is
  still the simpler of the two. The cost is two emission paths in `render_system_flake`, each
  needing its own golden coverage.

## Consequences

- A generated system can consume other flakes the way a hand-written flake does: `follows` lets
  an input share the project's nixpkgs, and transitive inputs are locked by each upstream's own
  `flake.lock` at the pinned rev, so knixl doesn't have to model them.
- `flake.lock` is a second lock file, but a derived one: every top-level entry is fixed by
  `knixl.lock.kdl`, and nix only fills in what follows from those revs (transitive inputs,
  `narHash`, `lastModified`). It has to be committed, and `knixl check` reports it when it is
  missing.
- `knixl check` gains a lint that reads a nix-owned file. It stays offline and writes nothing,
  so `Plan::compute` stays pure. A disagreement points at re-locking with `nix flake lock`, or at
  `upgrade` when `knixl.lock.kdl` is the side that is behind.
- The determinism of `generated/flake.nix` is unchanged: inputs are emitted in name order and
  `follows` in key order, and the file is formatted by the pinned formatter, hashed, and
  reconciled `Stale`/`Drifted`/`Orphaned` like any other output. The resolved input revs join
  `f(...)` beside the baselines.
- A baseline change is still `upgrade`-gated. It moves the lock's rev, which changes the
  `nixpkgs` input URL (a `Stale` flake), and nix re-locks from that. Other inputs move the same
  way, and only on `upgrade`.
- One `nixpkgs` input per flake means a fleet split across releases can't use input mode yet.
  Those projects stay on the input-free path until more than one nixpkgs input is designed.
- The `nixosSystem` point is confirmed. A real flake-built host migrated onto input mode
  (nixpkgs pinned with `rev=`, three input modules, sops-nix wiring, local imports) evaluates to
  the same system toplevel store path as the running system, and a guest image built beside it
  to the same drv. Module-list order made no difference there, though a system that merges
  list options across modules in an order-sensitive way could still differ. A `default-file`
  outside `generated/` is reachable: the flake is a git flake with `?dir=generated`, so nix
  copies the whole repository and `../secrets/<host>.yaml` resolves.
- Local module imports (a host `import "<path>"` node) are related but designed separately.
- Still deferred: merging per-host input modules with the project set (ADR 0008's
  replace-not-merge rule stands), passing `specialArgs` or `inputs` to modules, inputs that
  provide anything other than NixOS modules (overlays, packages), fetchers for sources that are
  not flake input refs, more than one nixpkgs input (differing per-host baselines in input mode),
  and `input=` for the agenix backend.
