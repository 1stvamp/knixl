# NEXT-STEPS

Ordered backlog. Each item is sized to be a session or less. Do them roughly in order: later ones assume earlier ones.

## Phase 0: make it real

1. **Compile the workspace.** Fill helper bodies, wire cross-crate types, stub `lower()` bodies to return empty. Goal: `cargo build` green, `cargo clippy` clean.
2. **Golden test harness.** Load `examples/hosts/*.kdl`, run the (stubbed) pipeline, compare to `examples/expected/`. It will fail; that is the target to drive toward.

## Phase 1: the emit path

3. **`knixl-ir` emit, fully.** `Emit` for every `NixExpr`, plus `escape_nix_str`, `emit_indent_str` (the `''${` escape), `fmt_nix_float` (canonical, always a decimal point, reject non-finite), `emit_attr_path` / `emit_key` (bare vs quoted classification). Unit-test each.
4. **Determinism.** Prove generate-twice is byte-identical. Add a test that permutes internal collection order (feature-gated) and asserts output unchanged.
5. **Formatter integration (`knixl-nix`).** Shell out to a pinned `nixfmt-rfc-style`. Record its version. Only the post-format text is hashed. Make the formatter path pinned and overridable for tests.

## Phase 2: reconcile and the lock

6. **`knixl-lock` model.** Parse and emit `knixl.lock.kdl`. blake3 hashing in `knixl-nix`. Round-trip test against `examples/knixl.lock.kdl` (recompute placeholder hashes once emit is real).
7. **`Plan::compute`.** The three-hash `FileState` derivation (lock vs disk vs expected), orphan discovery by scanning for the knixl header, `VersionSkew` from lock-vs-running. Pure, no writes.
8. **`knixl check` and `knixl plan`.** Wire the verdict/exit-code mapping from docs/05. `check` is the CI gate. This is the first genuinely useful command.

## Phase 3: modules

9. **`Module` trait + `Registry`.** Duplicate-node claim is a hard error. Built-in `host` (container, delegates via `lower_children`).
10. **Built-in `postgres`.** Exercises conditional `mkForce` (the case that cannot be declarative). Drive `examples/expected/db.nix` green.
11. **`EmitTemplate` + `DeclarativeModule`.** The substitution grammar from docs/04: `set`, `when-flag`, `for-each`, `collect`, path/value interpolation, the bindings tree. Load `modules/web-service/knixl-module.kdl`, drive `examples/expected/web.nix` green.
12. **Module-load dry type-pass.** Catch `{acme.email}` resolving to a non-scalar at load, not generate.

## Phase 4: safety and polish

13. **`knixl generate`.** Apply policy: silent Stale/Missing, refuse Drifted without `--accept-drift`, refuse skew (point at `upgrade`). Commit the lock only on a clean apply.
14. **`knixl upgrade`.** Migration notes keyed by `(module, version delta)`, diff, apply on `--yes`, bump all versions in the lock together.
15. **Cross-module value-conflict lint.** Plan-time: same `AttrPath` from two modules in one file, warn unless priorities disambiguate.
16. **`knixl-oracle`.** `nixosOptionsDoc` extraction, rev pinned in lock, best-effort `NixType` parse, path-existence + gross-type checks wired into generation. Cache `options.json` keyed by rev.
17. **`knixl doc <node>`.** Render the typed reference from `schema()`.

## Phase 5: open design work (needs a decision before coding)

- `let` hoisting pass (dedup repeated sub-expressions). Low priority.
- `lib.mkIf` from declarative modules (currently Rust-only). Only if a real module needs it.
- Third-party module distribution: how modules are discovered beyond `modules/` (a search path? a registry?). Out of scope for v1, but decide before the module format is stable and public.

## Definition of done for v1

`knixl check` gates CI, `generate` and `upgrade` enforce the drift/skew policy, both example hosts reproduce byte-for-byte from KDL, and a third party can add a straight-line module by dropping a `knixl-module.kdl` in `modules/` without recompiling.
