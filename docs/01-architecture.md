# 01: Architecture

## Pipeline

```
KDL inputs
  -> parse (knixl-kdl, the kdl crate + miette spans)
  -> per-node schema validation (knixl-modules: NodeSchema)
  -> dispatch to modules by node name (Registry)
  -> lower to bucketed Assignments (Module::lower)
  -> oracle check every emitted option path (knixl-oracle)
  -> group buckets into files, wire imports, hoist lets
  -> emit Nix source (knixl-ir: Emit)
  -> format with pinned nixfmt (knixl-nix)
  -> hash (blake3), reconcile against lock (knixl-lock)
  -> write generated/*.nix + knixl.lock.kdl
```

When the project declares `system {}`, the pipeline additionally generates `generated/flake.nix`, an optional locked artefact defining per-host NixOS configurations, each pinned to that host's baseline nixpkgs rev. It is reconciled and hashed like the host modules.

The whole thing is a pure function from (KDL, tool version, module versions, formatter version, oracle rev) to output bytes. `Plan::compute` runs everything up to "write" and produces a diff; the commands decide whether to write.

The "hoist lets" step is `knixl-ir::hoist`: within a file, a compound value (attrset, list, or indented string) that appears two or more times is bound once at the top as `let _knixl0 = ...; in { ... }` and referenced at each use. It is a pure IR-to-IR pass, deterministic, and a no-op on files with no repetition, so it never changes output unless there is genuine duplication. The `_knixlN` names seen in generated files come from here. The rule is defined in `docs/superpowers/specs/2026-07-14-let-hoisting-design.md`. `examples/hosts/shared.kdl`, together with the declarative module `modules/security-headers/knixl-module.kdl`, demonstrates it end to end: one `security-headers` block emitted at two vhosts produces identical assignments that hoist into a single shared binding.

## Crate layout and dependency direction

Strictly one direction. No crate imports `knixl`.

- `knixl-ir` : IR types (`NixExpr`, `NixModule`, `Assignment`), the `Emit` trait, escaping/float/attr-key helpers. Depends on nothing but `miette` and `semver`.
- `knixl-kdl` : input parsing over the `kdl` crate, span-carrying diagnostics.
- `knixl-oracle` : `nixosOptionsDoc` extraction and best-effort type checking. Depends on `serde_json`.
- `knixl-modules` : the `Module` trait, `Registry`, built-in modules, and the declarative `EmitTemplate` interpreter. Depends on `knixl-ir` + `knixl-kdl` + `knixl-oracle`.
- `knixl-lock` : lockfile model and the reconcile state machine. Depends on `knixl-nix` (for hashing).
- `knixl-nix` : formatter invocation (pinned nixfmt) and blake3 hashing.
- `knixl-pipeline` : the single generation entry point (gather, dispatch, lower, emit, format, install/strategy helpers). Depends on `knixl-ir` + `knixl-kdl` + `knixl-modules` + `knixl-nix` + `knixl-lock` + `knixl-oracle`.
- `knixl` : arg parsing, orchestration, exit codes. Depends on everything.

Keeping the library layers free of the CLI is deliberate: a language server or a GitHub Action can reuse `Plan::compute` and the emitter without dragging in clap or process handling.

## The IR is a constrained subset of Nix, on purpose

knixl only ever emits module bodies: attribute assignments into option paths, imports, the occasional `let`, plus a verbatim escape. It does not represent all of Nix. Trying to is the dhall-nix trap (see docs/00).

Two deliberate IR omissions: no dynamic/interpolated attr keys in the value AST (keeps output static and hashable), and `condition` + `priority` compose in one fixed order (`mkIf cond (mkForce x)`) rather than an arbitrary wrapper stack. A general stack is over-engineering for v1.

See `crates/knixl-ir/src/` for the full types. The shape:

- `NixExpr` : the value language (scalars, `List`, `AttrSet` over a `BTreeMap` so key order is deterministic by construction, `Apply`, `Lambda`, `Let`, `Select`, `Raw` escape).
- `NixModule` : not a `NixExpr`. It has a fixed shape (formals, imports, lets, body, provenance) always emitted the same way.
- `Assignment` : one option assignment, with optional `priority` (mkForce/mkDefault/mkOverride), optional `condition` (mkIf), and a doc comment.

## Determinism rules (load-bearing)

- No `HashMap` iteration in emit paths. Use `BTreeMap` or index-preserving structures.
- `AttrSet` keys are sorted by construction (`BTreeMap<AttrKey, _>`).
- Lists preserve KDL source order, so repeated children and `for-each` output are stable.
- `fmt_nix_float` always emits a decimal point (Rust's default drops it on whole numbers, which Nix reads as int) and rejects non-finite (Nix has no inf/nan).
- The formatter and oracle rev are pinned in the lock. Output is only reproducible relative to those pins.
