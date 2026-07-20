# Emit grammar: list of attribute sets design (#34)

Date: 2026-07-20
Status: approved, ready for implementation plan
Issue: #34 (project:homelab; enabler for incus-host #36 and disko #37)

Add a declarative template statement that folds a repeated child into a Nix list of attribute
sets (`[ { ... } { ... } ]`), the one target shape the grammar cannot currently express, so a
module whose option is a list-of-attrset no longer has to be a Rust built-in for that reason.

## Grounding (current state)

- The emit grammar (`crates/knixl-modules/src/template.rs`, docs/04) has four statement forms
  after #16: `set`, `when-flag` (generation-time gate), `when-config` (runtime `lib.mkIf`),
  `for-each` (one dynamic-keyed path per item), plus the value annotations `(collect)` (a flat
  scalar list) and `(indent-str)`.
- None yields a list of attribute sets. `for-each` produces attrset-of-attrsets by dynamic key
  (e.g. `services.nginx.virtualHosts."ex.com".locations."/api" = { ... }`), not a keyless list;
  `collect` folds only a repeated child's first arg into a flat scalar list.
- The IR already supports the target shape: `NixExpr::List(Vec<NixExpr>)` of
  `NixExpr::AttrSet(BTreeMap<AttrKey, NixExpr>)`, with nested `AttrSet` values and `AttrKey::Quoted`
  keys (for keys like `"ipv4.address"`). The emitter renders lists, nested attrsets, and
  `Apply`/`Select` (for `lib.mkIf`) correctly, and `emit_atom` already parenthesises list
  elements by precedence (the #15 fix). So this is purely a grammar gap, not an IR gap.
- `run` (the interpreter) is `run(&self, stmts, b, loops, cond, out: &mut Vec<Unit>)`; `for-each`
  resolves a repeated child via `resolve_list(source, b)` and pushes a `LoopScopes` entry per
  item. The dry type-pass (`check_stmts` / `schema_shape`) validates lookups at load.
- Determinism is load-bearing (the lock hashes post-format text): no `HashMap` on the emit path,
  list order must be a pure function of the input.

## Design

### The `list` statement

```
list "<target-path>" from "<repeated-child>" {
    set "<attr>" <value>
    ...
}
```

`list "virtualisation.incus.preseed.networks" from "network" { ... }` folds the repeated
`network` child into `NixExpr::List([AttrSet, ...])` assigned to the target path, one attrset
element per `network`, in KDL source order. The repeated child's name is the loop binding
(`from "network"` binds `{network.…}` inside the body), so no separate loop-var token is needed
(this differs from `for-each "var" in "source"`, which needs an explicit var to shadow; `list`
binds the source name directly, matching the approved shape).

AST: `Stmt::List { path: PathTemplate, source: String, body: Vec<Stmt> }`.

### Element construction (reuses the existing machinery)

Each element is built by running the body through the existing statement interpreter into a
per-element accumulator, then folding the result into one attrset:

- Body statements are `set`, `when-flag`, and `when-config` (the approved scope; nested `list`
  and `for-each` inside an element are out of scope).
- Each inner `set` uses the existing `PathTemplate` + `ValueTemplate`, so an attr path may be
  nested and quoted (`config."ipv4.address"`) and values may be scalars, interpolated strings,
  `(indent-str)`, or `(collect)` lists, all with `{loop-var.field}` interpolation.
- `when-flag` includes or drops an inner `set` at generation time (the common case: fewer attrs,
  plain values). `when-config` attaches a runtime condition to that inner `set`; when folding, a
  conditioned inner assignment's value is wrapped as `lib.mkIf (<cond>) <value>` (emitted as an
  `Apply`), which is valid as an attr value where the target option accepts it. That is the
  author's responsibility, the same raw-condition trust boundary `when-config` already carries.

Folding: the per-element `Vec<Unit>` (each a relative attr-path `Assignment`) is merged into a
nested `NixExpr::AttrSet` by walking each `AttrPath`'s segments to build/descend `AttrSet`
branches and placing the value (or `lib.mkIf`-wrapped value) at the leaf. Two inner sets writing
the same path, or a path that is both a leaf and a branch, is a `LowerError` (a real authoring
mistake, caught at generate).

### Interpretation

```
Stmt::List { path, source, body } => {
    let mut elems = Vec::new();
    for item in resolve_list(source, b)? {          // source order => stable
        loops.push(source, item);
        let mut elem_units = Vec::new();
        self.run(body, b, loops, None, &mut elem_units)?;  // inner when-config sets per-attr cond
        loops.pop();
        elems.push(fold_units_into_attrset(elem_units)?);
    }
    let assignment = Assignment {
        path: path.interpret(b, loops)?,
        value: NixExpr::List(elems),
        priority: None,
        condition: cond,   // an outer when-config gates the whole list assignment
        doc: None,
    };
    out.push(Unit { bucket: Bucket::Default, assignment, module: String::new() });
}
```

An absent repeated child yields an empty list (matching `resolve_list`'s absent-child behaviour
and `collect`'s empty-list behaviour). The outer `cond` (a `when-config` around the `list`
statement) gates the whole list assignment; inner `when-config` gates individual attrs.

Determinism: element order is the child's source order (a `Vec`); within-element key order is
`BTreeMap`-sorted (deterministic by construction). No `HashMap` on the path.

### Dry type-pass

Add a `Stmt::List` arm to `check_stmts`: the `from` source must be a repeated child (reuse the
`for-each` check, erroring "not a repeated child" otherwise), push the loop var (source name)
with the element shape, then check the body (inner `set`/`when-flag`/`when-config` lookups as
scalars, as today). Check the target path's `{lookup}` parts as scalars. So a bad source or a
non-scalar lookup fails at load, not at generate, consistent with the rest of the grammar.

### Parsing

`parse_stmt` gains a `"list"` arm mirroring `for-each`: collect the positional string args
(`"<path>" from "<source>"`, the bare `from` is noise, first = path, last = source), body via
`parse_stmts(n.children())`. A missing path or source is a load error.

### IR / emit

No change. The statement produces one `Assignment` whose value is `NixExpr::List` of
`NixExpr::AttrSet`; emit already renders these.

## Testing

Unit tests in `template.rs` (mirroring how `for-each`/`collect` are tested), using a
manifest-string fixture with a repeated structured child:
- A repeated structured child folds into a `List` of `AttrSet`s with the right keys and values,
  including a nested/quoted key (`config."ipv4.address"`).
- Element order follows KDL source order (two children -> two elements, order preserved).
- Determinism: generate twice, assert byte-identical emitted text.
- An emitted-text assertion that the value renders as `<path> = [ { ... } { ... } ];`.
- `when-flag #false` on an inner `set` drops that attr from the element; `#true` keeps it.
- An absent repeated child yields `[ ]`.
- Dry-check rejects `list ... from "<non-repeated-child>"` ("not a repeated child") and a
  non-scalar `{lookup}` in an inner set.
- A path conflict between two inner sets (same path) is a `LowerError`.

## Out of scope

- Nested `list` or `for-each` inside a `list` element (only `set`/`when-flag`/`when-config`).
- Building the incus-host (#36) or disko (#37) modules themselves; #34 only provides the
  capability. A full golden example lands with the first consumer (#36).
- Any change to `for-each`/`collect`/the IR/the lock.
