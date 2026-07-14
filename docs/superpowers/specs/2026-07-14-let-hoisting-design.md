# let-hoisting design

Date: 2026-07-14
Status: approved, ready for implementation plan

## Problem

`docs/01-architecture.md` lists "hoist lets" as a generator pass, and `NixModule`
already carries a `lets: Vec<Binding>` field, but nothing populates or emits it:
`NixModule::emit` skips the field entirely. So repeated subexpressions are written
out in full at every use site. This spec defines the pass that fills `lets` and the
emit change that renders it.

`docs/03-module-system.md` fixes the ownership: "let hoisting is a generator pass,
not a module concern." Modules stay ignorant of it.

## Scope

Deduplicate repeated subexpressions within a single generated file by binding each
repeated value once in a top-level `let` and referencing it. Nothing else.

Out of scope for v1 (call these out so the plan does not drift):

- Recursive hoisting inside a binding's own value.
- Sharing across files (each output file hoists independently).
- Author-named or author-marked constants (that would push a generator concern into
  modules, against docs/03).
- Hoisting `raw` passthrough, `Apply`, or `Lambda`.

## Trigger and eligibility

A value is hoisted when both hold:

1. It appears verbatim two or more times within one file.
2. It is compound: a non-empty `AttrSet`, a non-empty `List`, or an `IndentStr`.

Scalars (`Bool`, `Int`, `Float`, `Null`, `Str`), `Ref`, `Select`, `Path`, `Apply`,
`Lambda`, `Let`, and `Raw` are never hoisted. Scalars and refs are noise; raw and
apply/lambda can capture scope the pass cannot reason about, so they are left alone.

"Verbatim" means the emitted Nix text is byte-identical. Equality is defined by what
actually lands in the file (via the existing `Emit`/`Writer`), which is what the lock
hashes, so the equality test and the reproducibility boundary agree by construction.

## Naming

A fixed prefix plus a counter assigned in first-use order: `_knixl0`, `_knixl1`, and
so on. The prefix keeps generated names clear of `config`/`lib`/`pkgs` and of any
option content. Names carry no semantic meaning by design; they are stable only as a
pure function of the input (see determinism below).

## Where it runs

A new pure module `knixl-ir::hoist` exposes one function:

```rust
pub fn hoist(body: &mut Vec<Assignment>) -> Vec<Binding>
```

It rewrites each assignment's `value` in place (replacing hoisted nodes with `Ref`s)
and returns the bindings, in name order. `knixl-pipeline::generate_one` calls it per
file after the conflict-lint step and before building the `NixModule`, then passes the
result as `NixModule.lets`.

The oracle already checks option paths and values earlier in `generate_one`, on the
original (pre-hoist) values, so it still type-checks real values rather than `Ref`s.
The pass runs after that, so the two do not interfere.

## Algorithm

Pure and deterministic, a function of the input assignments alone:

1. Walk every assignment `value`, full depth-first descent, building a map from
   emitted text to occurrence count over eligible nodes only.
2. Candidates are the texts with count two or more.
3. Replacement walk over assignments in `body` order, pre-order within each value:
   at each node, if its emitted text is a candidate, look up or assign its name
   (next counter value on first encounter), replace the node with `Ref(name)`, and
   stop descending into it. This gives maximal hoisting: no nested `_knixl`
   references, and a binding's own value keeps its internals literal.
4. Return the bindings in name order, which equals first-use order.

### Why names are assigned in the replacement walk, not the counting walk

A candidate that only ever appears inside another hoisted block must not get a
binding: once the outer block is hoisted, the inner value occurs exactly once (inside
the outer binding). Assigning names during the maximal replacement walk means such an
inner value is never reached, so it gets no name and there is no dangling binding.

### Known v1 limitation

If a value appears both nested inside a hoisted block and standalone elsewhere, only
the standalone (maximal) site is hoisted; the nested copy stays literal inside the
outer binding. The two copies are then textually equal but not shared. This is
acceptable for v1 and is documented rather than fixed, because fixing it means
recursive hoisting inside bindings, which is out of scope.

## Emit

`NixModule::emit` gains one branch. When `lets` is non-empty, emit

```
let
  _knixl0 = <value>;
  ...
in {
  <imports, body, raw as today>
}
```

after the formals; when `lets` is empty, the current `{ ... }` output is unchanged.
Bindings emit in name order. nixfmt canonicalises whitespace, so only syntactic
validity matters for the byte golden, not the emitter's own indentation.

### Validity

The `let` sits inside the module lambda (`{ config, lib, pkgs, ... }:`), so those
arguments are in scope for the bindings. Hoisted values only ever reference those or
literals, so a hoisted binding is always closed and the result is valid Nix.

## Reproducibility

Output changes only when a file contains a genuine repeat. The existing `web.nix`,
`db.nix`, and `db-backup.nix` have none, so their goldens and lock hashes are
byte-unchanged. A new example demonstrates the feature end to end:

- `examples/hosts/shared.kdl`: an input that produces the same compound value at two
  option paths.
- `examples/expected/shared.nix`: the byte golden, generated under the pinned nixfmt.
- The lock gains the matching output entry.

## Testing

`knixl-ir` unit tests:

- No repeat: `hoist` returns no bindings and leaves values untouched.
- One repeated attrset: one binding, both sites become the same `Ref`.
- Maximal hoist: a repeat nested inside a repeated parent is not separately bound; no
  dangling bindings.
- Determinism: two runs over the same input produce identical bindings and names.
- Name order: `_knixl0` is the first-encountered candidate in body/pre-order.

Pipeline and CLI:

- Byte golden for `shared.nix` under the real formatter (VM), skipped when no
  formatter is present, matching the existing golden guard.
- The determinism golden covers the new host too.

## Files touched

- `crates/knixl-ir/src/hoist.rs` (new): the pass and its unit tests.
- `crates/knixl-ir/src/lib.rs`: register the module, re-export `hoist`.
- `crates/knixl-ir/src/emit.rs`: the `let ... in { ... }` branch in `NixModule::emit`.
- `crates/knixl-pipeline/src/lib.rs`: call `hoist` per file, set `NixModule.lets`.
- `examples/hosts/shared.kdl`, `examples/expected/shared.nix`, `examples/knixl.lock.kdl`.
- `docs/01-architecture.md` or `docs/03-module-system.md`: a short note that the pass
  is now implemented, if the prose needs it.
