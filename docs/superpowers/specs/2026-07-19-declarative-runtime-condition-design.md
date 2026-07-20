# Declarative runtime condition design (#16)

Date: 2026-07-19
Status: approved, ready for implementation plan
Issue: #16

Let a declarative `knixl-module.kdl` express a runtime `lib.mkIf` condition off `config.*`,
so the capability that currently forces a module to be a built-in (backups' `when=`) is
available to declarative modules too.

## Grounding (current state)

- The IR already carries runtime conditions: `Assignment.condition: Option<NixExpr>` emits
  `<path> = lib.mkIf (<cond>) <value>;` (`knixl-ir/src/emit.rs:87`). The `lib` formal is in
  every module header already, so emit needs no change.
- Only built-ins set it today. `backups` reads a raw Nix `when=` string and stores it as
  `condition: Some(NixExpr::Raw(..))` (`knixl-modules/src/builtin/backups.rs:51`). Its module
  doc says it is a built-in *solely* because a declarative module cannot emit a runtime
  condition.
- Declarative modules have three template statements (`set`, `when-flag`, `for-each`).
  `when-flag` gates at *generation* time: it includes or drops its body
  (`template.rs:159`), so it cannot express a `config.*` condition that is only known when
  NixOS evaluates the generated module.
- docs/03:53 and docs/04:5 state, as a design boundary (not an ADR), that a declarative
  module "cannot emit a runtime `lib.mkIf` off `config.*`". This issue deliberately lifts that
  boundary; both docs are updated as part of the work. No ADR governs this, and ADR 0002
  (emit source, not values) is upheld: `lib.mkIf` is source text.

## Design

### A fourth statement: `when-config`

```
emit {
    when-config "config.services.postgresql.enable" {
        set "services.restic.backups.db.repository" "{repo}"
        set "services.restic.backups.db.initialize" #true
    }
}
```

`when-config "<cond>" { <body> }` wraps every assignment produced by its body with the
runtime condition, i.e. each becomes `<path> = lib.mkIf (<cond>) <value>;`. It parallels
`when-flag` in shape, but where `when-flag` decides *whether* to emit at generation time,
`when-config` always emits and defers the decision to NixOS evaluation.

The condition string is raw Nix (off `config.*`), with the same `{lookup}` interpolation the
rest of the grammar uses: `config.services.{service}.enable` substitutes a scalar input.
The interpolated result is stored verbatim as `NixExpr::Raw` (knixl does not parse or validate
the Nix expression itself, matching backups' `when=`).

### AST and parsing (template.rs)

- Add `Stmt::WhenConfig { cond: Vec<StrPart>, body: Vec<Stmt> }`. `cond` reuses the existing
  `StrPart` (literal + `{lookup}`) representation, parsed with `parse_str_parts`.
- `parse_stmt` gains a `"when-config"` arm: first positional arg is the condition string,
  `body` via `parse_stmts(n.children())`. A missing condition arg is a load error, matching
  `when-flag`'s missing-flag error.

### Interpretation (template.rs `run`)

`run` threads the active condition down the recursion as `Option<String>` (the already
interpolated Nix text):

- On entering `WhenConfig`, interpret `cond` to a string against the bindings and the loop
  scopes currently in scope (so a condition may reference an *enclosing* `for-each` var).
- Combine with any outer condition by conjunction: `outer` present → `({outer}) && ({inner})`,
  else just `inner`. Pass the combined text into the body's `run`.
- When a `Set` fires and a condition is active, set
  `assignment.condition = Some(NixExpr::Raw(RawNix { src: cond, span: None }))`.
- `when-flag` and `for-each` pass the active condition through unchanged (a `when-flag` that
  drops its body drops the conditioned sets too; a `for-each` inside `when-config` conditions
  every generated element with the same condition).

Nesting is defined, not rejected: `when-config A { when-config B { set .. } }` emits
`lib.mkIf ((A) && (B)) ..`. Conjunction is deterministic and matches logical nesting.

### Dry type-pass (template.rs `check_stmts`)

Add a `WhenConfig` arm: for every `{lookup}` part in the condition, `expect_scalar` (same rule
as `set` paths and values). The condition text itself is not type-checked (it is opaque Nix).
Recurse into the body. This keeps a bad `{lookup}` in a condition a load-time error, not a
generation-time one, consistent with the rest of the grammar.

### Emit

No change. `Assignment.condition` already emits `lib.mkIf (<cond>) <value>` and composes with
`priority` in the fixed `mkIf cond (mkForce value)` order (`emit.rs:85`).

## Determinism

The condition text is a pure function of the inputs: interpolation is deterministic and body
order is KDL source order. No `HashMap` on the path. The output hash stays a function of the
input, which the lock depends on.

## Testing

Unit tests in `template.rs` (mirroring how `when-flag`/`for-each`/`collect` are tested there,
via manifest-string fixtures lowered through `DeclarativeModule`):

- `when-config` sets `condition = Raw(..)` on each wrapped `set`, with `{lookup}` interpolated
  into the condition text.
- A `set` outside any `when-config` has `condition == None`.
- Nested `when-config` AND-combines into `(A) && (B)`.
- `when-config` inside `for-each` interpolates the loop var into the condition.
- `when-flag #false` around a `when-config` drops the body entirely (generation-time gate
  still wins; no conditioned sets emitted).
- Dry-check rejects a non-scalar `{lookup}` in a condition at load time.

An end-to-end assertion that the emitted text contains `lib.mkIf (...)` for a conditioned set
(a small emit test over the lowered assignment), proving the IR path is exercised.

## Out of scope

- Converting `backups` (or any built-in) to declarative. The design makes it *possible*; the
  conversion is a separate decision and not done here.
- Validating the Nix condition expression against the oracle (raw Nix off `config.*` is not
  validated today; unchanged).
- Any new priority/bucket capability for declarative modules (docs/03 boundary otherwise
  stands: still `Bucket::Default`, no computed priorities).
