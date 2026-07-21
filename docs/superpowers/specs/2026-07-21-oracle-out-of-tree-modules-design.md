# Oracle: out-of-tree module option sets design (#35)

Date: 2026-07-21
Status: approved, ready for implementation plan
Issue: #35 (project:homelab; unblocks disko #37 and the secrets model #38)
Refines: ADR 0003 (oracle from nixosOptionsDoc), ADR 0007 (per-host baseline); adds ADR 0008.

Build the oracle's option set from a NixOS eval that includes a declared set of out-of-tree
modules (disko, sops-nix / agenix, ...), pinned in the lock alongside the base rev, so emitting
`disko.*` / `sops.*` gets real option-path validation instead of being rejected as unknown (or
hidden in `raw-nix`). knixl builds the augmented `options.json` itself (full automation), which
also closes the currently-manual base build (docs/06).

## Grounding (current state)

- The oracle (`crates/knixl-oracle/src/lib.rs`) loads an `options.json` (nixosOptionsDoc output)
  cached per nixpkgs rev at `$XDG_CACHE_HOME/knixl/options-<rev>.json`. `check` rejects any path
  not in that set (`UnknownOption`) unless it is a submodule prefix. So an out-of-tree path like
  `disko.*` is either rejected (base set cached) or unchecked (no set cached) : never validated.
- Building `options.json` is NOT automated in knixl today (docs/06: manual `nixosOptionsDoc` +
  copy to the cache path). The lookup by rev is automated; the build is not.
- The lock pins the oracle inputs: `oracle nixpkgs-rev=... options-hash=...` (global default) and,
  per host, `baseline release=... nixpkgs-rev=... options-hash=...` (ADR 0007). `OraclePin { nixpkgs_rev, options_hash }` and `HostBaseline { release, nixpkgs_rev, options_hash }` in
  `knixl-lock/src/model.rs`.
- A host declares its baseline release as a `nixpkgs release="<rel>"` child node (`host.rs`
  recognises it; ADR 0007). Hosts without one fall back to the lock's global `oracle nixpkgs-rev`,
  which today has no explicit KDL declaration (it is seeded from running versions).
- There is NO project-level config file today: a project is `hosts/` + `modules/` +
  `knixl.lock.kdl`, discovered by walking up to the first dir with `hosts/` or the lock.
- knixl-nix has the infra to build on: `NixEval` (nix-build / eval helpers, `builds_expr`), a
  baseline resolver (`git ls-remote` for `nixos-<rel>` -> rev, GitHub API fallback), and a pin
  resolver. `knixl_oracle::cache_path(rev)` returns the cache location; `hash(bytes)` (knixl-nix)
  produces the blake3 hashes the lock uses.

## Design

### `knixl.kdl`: the project oracle-inputs file

Introduce one optional project-level file at the root, `knixl.kdl`, holding the project's oracle
inputs. Two declarations, both with the same "project default, host may replace" model:

```
// knixl.kdl (project root)
nixpkgs release="25.05"          // default baseline for hosts that don't declare their own

oracle-modules {                  // default out-of-tree modules for the oracle option set
    module "disko"    flake="github:nix-community/disko"
    module "sops-nix" flake="github:Mic92/sops-nix"
}
```

- `nixpkgs release="<rel>"` gives the previously-implicit global default baseline an explicit
  home. A host's own `nixpkgs release=` (existing, ADR 0007) still overrides it for that host.
  Hosts fall back to this project default, then (if absent) to the seeded running rev.
- `oracle-modules { module "<name>" flake="<ref>" [attr="<nixosModules-attr>"] }` declares the
  default out-of-tree modules. `attr` defaults to `default` (the flake's
  `nixosModules.default`).
- A host may declare its own `oracle-modules` block (in its host KDL) to **replace** the project
  set for that host (replace semantics: the host's list is its full set; the project default is
  ignored for that host). Effective module set for a host = its own block if present, else the
  project default (else empty).

`knixl.kdl` is optional; absent it, behaviour is exactly today's (no extra modules, seeded
default rev). Parsing lives in `knixl-pipeline::gather` (or a small `knixl-kdl` helper),
alongside host parsing.

### Module-source resolution and pinning

Each `module` flake ref resolves to a locked commit, mirroring the baseline resolver:

- A `ModuleResolver` (knixl-nix) resolves a flake ref (e.g. `github:owner/repo`) to a
  `{ url, rev }`: for `github:` refs, `git ls-remote` the default branch head (GitHub API
  fallback), same pattern as `baseline.rs`; `KNIXL_MODULE_RESOLVER` overrides with an external
  `<flake-ref>` -> `<rev>` command. Resolution happens at `install`/`upgrade` time, not at
  `plan`/`generate` (which stay offline), matching how baselines resolve.
- The resolved pins go in the lock. Project modules pinned once under the global oracle; a host
  that overrides records its own set under its `baseline`. New lock lines:
  ```
  oracle nixpkgs-rev="<rev>" options-hash="<hash>"
      oracle-module name="disko"    url="https://github.com/nix-community/disko"    rev="<rev>" attr="default"
      oracle-module name="sops-nix" url="https://github.com/Mic92/sops-nix"         rev="<rev>" attr="default"
  ```
  and the same `oracle-module` child lines nested under a host's `baseline` when it overrides.
  `OraclePin` and `HostBaseline` gain `modules: Vec<OracleModulePin>` where
  `OracleModulePin { name, url, rev, attr }`. Rendering stays byte-stable (source order; absent
  = no lines, so existing locks render unchanged).

### Building the augmented `options.json` (full automation)

knixl-nix ships a `nixosOptionsDoc` expression and runs it via `NixEval`:

- The expression takes the pinned `nixpkgs` rev and the pinned module sources, evaluates the
  NixOS module system with the base modules plus each declared module
  (`(builtins.getFlake "<url>?rev=<rev>").nixosModules.<attr>`, or `builtins.fetchGit` +
  the module's entry path), and runs `nixosOptionsDoc` over the combined options, emitting the
  same `options.json` shape the oracle already parses.
- knixl builds it on demand: when a host's effective oracle set is needed and not cached, run the
  eval, capture `options.json`, and cache it keyed by the effective set. With no modules the
  expression is just the base nixpkgs options, so this path also produces the base `options.json`
  that was manual before (closing docs/06's gap).
- Cache key depends on the effective set: an empty module set keeps today's `options-<rev>.json`
  path (base caches stay valid and shared), while a non-empty set uses
  `options-<effective-hash>.json` where the effective hash covers `(nixpkgs rev + the ordered
  module pins)`. `cache_path` gains the effective-set-keyed variant beside the existing rev-only
  one. The lock's `options-hash` is the blake3 of the built `options.json` content (unchanged
  meaning), so a changed rev or module set changes the built content and thus the hash, keeping
  the check reproducible.
- nix absent / eval failure is best-effort, same as today: `plan`/`generate` proceed without the
  option check (a warning), unless `--strict`. Building happens at `install`/`upgrade` (online),
  and a missing cache at `plan` time falls back to "no check" rather than shelling out to nix
  mid-plan (keeping `plan` offline and pure).

### Wiring (gather / plan)

`gather` already builds a per-host oracle keyed by the host's baseline rev. It changes to:

1. Parse `knixl.kdl` (project default release + oracle-modules) once.
2. Compute each host's effective (baseline rev, module set): host `nixpkgs release=` else project
   default else seeded; host `oracle-modules` else project default else empty.
3. Load the augmented `options.json` cached for that effective set (env `KNIXL_OPTIONS_JSON` still
   wins); absent, no check (best-effort), same as today.

`install`/`upgrade` additionally resolve module revs, build+cache the augmented `options.json`,
and write the module pins + `options-hash` to the lock (revertably, on confirmed apply, mirroring
the baseline pre-pass from #22). Validation itself (`Oracle::check`) is unchanged: once
`disko.*` is a known option, it validates for free.

### ADR 0008

Add ADR 0008: out-of-tree oracle modules and the `knixl.kdl` project file. It records that the
oracle option set is built from `nixpkgs@rev` plus a declared, pinned set of out-of-tree modules;
that `knixl.kdl` declares the project's default baseline release and module set with per-host
replace-override; and that the module pins join the reproducibility boundary
(`output = f(kdl, tool, module versions, formatter, oracle_rev + oracle module revs)`). It
refines ADR 0003 (the oracle now spans out-of-tree modules) and ADR 0007 (the global default
baseline becomes an explicit `knixl.kdl` declaration rather than an implicit seed).

## Determinism / reproducibility

The augmented `options.json` is a pure function of `(nixpkgs rev, ordered module pins)`; both are
in the lock, and the `options-hash` pins the built content. Module pins render in source order.
No `HashMap` on any emit/lock path. A check that passes under one module set and fails under
another changes the `options-hash`, so drift is caught, preserving the ADR 0003 property.

## Testing

- Lock: `OraclePin`/`HostBaseline` round-trip with and without `oracle-module` lines; byte-stable
  render; an existing lock without them renders unchanged.
- `knixl.kdl` parsing: project default release + oracle-modules; a host `oracle-modules` replaces
  the project set; absent file = today's behaviour.
- Module resolver: `git ls-remote` output -> rev (pure `rev_from_*` helpers, as baseline.rs);
  `KNIXL_MODULE_RESOLVER` external override.
- Effective-set computation: host override vs project default vs empty; cache-key hash is stable
  and order-sensitive to the module list.
- Build expression: an integration test (gated on nix, like the golden/formatter tests) that the
  expression over a small pinned rev + one module produces an `options.json` containing that
  module's option paths; skipped when nix is absent.
- End to end: a host emitting a `disko.*` path validates clean against an augmented set and fails
  (`UnknownOption`) against the base set, proving the augmented set is what makes it pass.
- `KNIXL_OPTIONS_JSON` override still wins; nix-absent falls back to no-check (best-effort).

## Decomposition (implementation phases)

Large and nix-heavy; the plan will task it in this order, each independently shippable:

1. `knixl.kdl` + `oracle-modules`/default-release parsing and effective-set computation (pure).
2. Lock schema: `OracleModulePin` on `OraclePin`/`HostBaseline`, render/parse, reconcile pruning.
3. Module-source resolver (knixl-nix) + `install`/`upgrade` resolution and lock writes.
4. The `nixosOptionsDoc` build expression + `NixEval` integration + effective-set-keyed caching
   (incl. the base no-modules build).
5. gather/plan wiring so each host's oracle uses the augmented set; ADR 0008 + docs (03/06).

## Out of scope

- Building the disko (#37) or secrets (#38) modules themselves; #35 only makes their options
  validatable.
- Fetching/validating module *values* beyond option-path/type (the oracle stays best-effort per
  ADR 0003: unknown paths and gross type mismatches, submodule interiors punted).
- Non-flake module sources beyond what the resolver's flake-ref/`fetchGit` model covers (a local
  path module could be a later addition).
- Changing `Oracle::check` semantics.
