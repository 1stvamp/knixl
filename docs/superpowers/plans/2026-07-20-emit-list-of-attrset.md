# Emit grammar: list of attribute sets implementation plan (#34)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a declarative `list "<path>" from "<child>" { ... }` statement that folds a repeated
child into a Nix list of attribute sets, so a list-of-attrset option no longer forces a built-in.

**Architecture:** A new `Stmt::List` in the template interpreter. It runs its body through the
existing `set`/`when-flag`/`when-config` machinery into a per-element `Vec<Unit>`, folds those
relative attr-path assignments into one nested `NixExpr::AttrSet`, and emits a single `Assignment`
whose value is `NixExpr::List` of those attrsets. The IR and emitter are unchanged.

**Tech Stack:** Rust, knixl-modules (template interpreter), knixl-ir (types, unchanged).

## Global Constraints

- British spelling in prose and comments. No em-dashes or en-dashes.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless,
  delve, and the AI-smell set.
- Do NOT run `cargo fmt` (repo is not rustfmt-normalised; hand-format to the file's style).
- Never run git/but or commit in a task; the controller commits. Do NOT run `git stash`/`git
  status`/any git command (another agent may have parallel uncommitted work).
- Determinism: element order is the child's source order (a `Vec`); within-element key order is
  `BTreeMap`-sorted. No `HashMap` on the emit path.
- Element body is `set` / `when-flag` / `when-config` only (nested `list`/`for-each` are out of
  scope). Inner `set` reuses `PathTemplate`/`ValueTemplate` (nested and quoted keys, interpolation).
- `NixExpr`, `AttrKey`, `AttrPath`, `RawNix`, `Assignment`, `Emit`, `Writer` come from `knixl_ir`.

---

### Task 1: The `list` statement in the template interpreter

**Files:**
- Modify: `crates/knixl-modules/src/template.rs` (`Stmt::List` variant; `run` arm +
  `fold_units_into_attrset`; `parse_stmt` arm; `check_stmts` arm; tests)

**Interfaces:**
- Produces: `Stmt::List { path: PathTemplate, source: String, body: Vec<Stmt> }`; a single
  `Assignment { value: NixExpr::List(vec![NixExpr::AttrSet(..), ..]) }` per `list` statement.
- Consumes: existing `resolve_list`, `run`, `PathTemplate::interpret`, `Assignment`, `Unit`,
  `Bucket`, `expect_scalar`, `lookup_shape`, `check_str_lookups`, `SegmentTemplate`, `RawNix`.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module. Reuse the existing helpers `node`, `lower`, `find`, `path_str` (used
by the other template tests). The fixture is a repeated structured `network` child.

```rust
    fn networks_module() -> DeclarativeModule {
        let manifest = "module name=\"net\" version=\"0.1.0\" {\n    claims-node \"net\"\n    schema {\n        child \"network\" repeated=#true {\n            prop \"name\" type=\"string\" required=#true\n            prop \"kind\" type=\"string\" required=#true\n            prop \"ipv4\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        list \"virtualisation.incus.preseed.networks\" from \"network\" {\n            set \"name\" \"{network.name}\"\n            set \"type\" \"{network.kind}\"\n            set \"config.\\\"ipv4.address\\\"\" \"{network.ipv4}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        DeclarativeModule::from_kdl(&doc, std::path::Path::new("net")).expect("loads")
    }

    #[test]
    fn list_folds_a_repeated_child_into_a_list_of_attrsets() {
        let m = networks_module();
        let n = node("net {\n    network name=\"incusbr0\" kind=\"bridge\" ipv4=\"auto\"\n    network name=\"br1\" kind=\"macvlan\" ipv4=\"none\"\n}");
        let out = lower(&m, &n);
        match find(&out, "virtualisation.incus.preseed.networks") {
            Some(NixExpr::List(items)) => {
                assert_eq!(items.len(), 2, "one element per network");
                match &items[0] {
                    NixExpr::AttrSet(map) => {
                        assert!(matches!(map.get(&AttrKey::Ident("name".into())), Some(NixExpr::Str(s)) if s == "incusbr0"));
                        assert!(matches!(map.get(&AttrKey::Ident("type".into())), Some(NixExpr::Str(s)) if s == "bridge"));
                        // nested + quoted key: config."ipv4.address" = "auto"
                        match map.get(&AttrKey::Ident("config".into())) {
                            Some(NixExpr::AttrSet(cfg)) => assert!(
                                matches!(cfg.get(&AttrKey::Quoted("ipv4.address".into())), Some(NixExpr::Str(s)) if s == "auto")
                            ),
                            other => panic!("config = {other:?}"),
                        }
                    }
                    other => panic!("element 0 = {other:?}"),
                }
            }
            other => panic!("networks = {other:?}"),
        }
    }

    #[test]
    fn list_preserves_source_order() {
        let m = networks_module();
        let n = node("net {\n    network name=\"a\" kind=\"bridge\" ipv4=\"1\"\n    network name=\"b\" kind=\"bridge\" ipv4=\"2\"\n}");
        let out = lower(&m, &n);
        let names: Vec<String> = match find(&out, "virtualisation.incus.preseed.networks") {
            Some(NixExpr::List(items)) => items.iter().map(|e| match e {
                NixExpr::AttrSet(m) => match m.get(&AttrKey::Ident("name".into())) { Some(NixExpr::Str(s)) => s.clone(), _ => String::new() },
                _ => String::new(),
            }).collect(),
            other => panic!("networks = {other:?}"),
        };
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn list_emits_a_list_of_attrsets_and_is_deterministic() {
        use knixl_ir::{Emit, Writer};
        let m = networks_module();
        let n = node("net {\n    network name=\"x\" kind=\"bridge\" ipv4=\"auto\"\n}");
        let render = || {
            let out = lower(&m, &n);
            let a = out.units.iter().map(|u| &u.assignment).find(|a| path_str(a) == "virtualisation.incus.preseed.networks").unwrap().clone();
            let mut w = Writer::new();
            a.emit(&mut w);
            w.into_string()
        };
        let text = render();
        assert!(text.contains("= ["), "renders a list: {text}");
        assert!(text.contains("name = \"x\""), "renders the element attr: {text}");
        assert!(text.contains("\"ipv4.address\""), "renders the quoted nested key: {text}");
        assert_eq!(text, render(), "byte-identical on a second render");
    }

    #[test]
    fn list_with_an_absent_child_is_empty() {
        let m = networks_module();
        let out = lower(&m, &node("net"));
        match find(&out, "virtualisation.incus.preseed.networks") {
            Some(NixExpr::List(items)) => assert!(items.is_empty()),
            other => panic!("networks = {other:?}"),
        }
    }

    #[test]
    fn list_when_flag_drops_an_inner_attr() {
        let manifest = "module name=\"net\" version=\"0.1.0\" {\n    claims-node \"net\"\n    schema {\n        child \"network\" repeated=#true {\n            prop \"name\" type=\"string\" required=#true\n            prop \"managed\" type=\"bool\"\n        }\n    }\n    emit {\n        list \"a.networks\" from \"network\" {\n            set \"name\" \"{network.name}\"\n            when-flag \"network.managed\" {\n                set \"managed\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("net")).expect("loads");
        let out = lower(&m, &node("net {\n    network name=\"on\" managed=#true\n    network name=\"off\"\n}"));
        match find(&out, "a.networks") {
            Some(NixExpr::List(items)) => {
                let has = |i: usize, k: &str| matches!(&items[i], NixExpr::AttrSet(m) if m.contains_key(&AttrKey::Ident(k.into())));
                assert!(has(0, "managed"), "managed=true keeps the attr");
                assert!(!has(1, "managed"), "managed absent drops the attr");
            }
            other => panic!("networks = {other:?}"),
        }
    }

    #[test]
    fn dry_check_rejects_list_over_a_non_repeated_child() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"net\" type=\"string\"\n    }\n    emit {\n        list \"a.b\" from \"net\" {\n            set \"name\" \"{net}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad")).err().unwrap();
        assert!(format!("{err}").contains("not a repeated child"), "got: {err}");
    }

    #[test]
    fn list_rejects_a_duplicate_inner_attr_path() {
        let manifest = "module name=\"dup\" version=\"0.1.0\" {\n    claims-node \"dup\"\n    schema {\n        child \"x\" repeated=#true {\n            prop \"a\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        list \"p.q\" from \"x\" {\n            set \"name\" \"{x.a}\"\n            set \"name\" \"{x.a}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("dup")).expect("loads (dup is a generate-time error)");
        let reg = crate::Registry::new();
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(crate::Scope { host: "h".into() }, &reg, &mut diags, vec![]);
        let err = m.lower(&node("dup {\n    x a=\"1\"\n}"), &mut ctx).err().unwrap();
        assert!(format!("{err}").contains("duplicate") || format!("{err}").contains("both a value and a set"), "got: {err}");
    }
```

Run: `cargo test -p knixl-modules list_ dry_check_rejects_list`
Expected: FAIL to compile (`Stmt::List` and the arms do not exist).

- [ ] **Step 2: Add the `Stmt::List` variant**

In the `Stmt` enum add (keep the existing four):

```rust
    List {
        path: PathTemplate,
        source: String,
        body: Vec<Stmt>,
    }, // fold a repeated child into a list of attribute sets
```

- [ ] **Step 3: Add the fold helper**

Add near `resolve_list`:

```rust
enum AttrNode {
    Leaf(NixExpr),
    Branch(std::collections::BTreeMap<AttrKey, AttrNode>),
}

fn attr_key_str(k: &AttrKey) -> String {
    match k { AttrKey::Ident(s) | AttrKey::Quoted(s) => s.clone() }
}

fn insert_attr_path(
    map: &mut std::collections::BTreeMap<AttrKey, AttrNode>,
    path: &[AttrKey],
    val: NixExpr,
) -> Result<(), LowerError> {
    let (first, rest) = path
        .split_first()
        .ok_or_else(|| LowerError::Other("empty attr path in a list element".into()))?;
    if rest.is_empty() {
        if map.contains_key(first) {
            return Err(LowerError::Other(format!(
                "duplicate attr `{}` in a list element",
                attr_key_str(first)
            )));
        }
        map.insert(first.clone(), AttrNode::Leaf(val));
    } else {
        let entry = map
            .entry(first.clone())
            .or_insert_with(|| AttrNode::Branch(std::collections::BTreeMap::new()));
        match entry {
            AttrNode::Branch(inner) => insert_attr_path(inner, rest, val)?,
            AttrNode::Leaf(_) => {
                return Err(LowerError::Other(format!(
                    "attr `{}` is both a value and a set in a list element",
                    attr_key_str(first)
                )))
            }
        }
    }
    Ok(())
}

fn attr_node_to_expr(map: std::collections::BTreeMap<AttrKey, AttrNode>) -> NixExpr {
    NixExpr::AttrSet(
        map.into_iter()
            .map(|(k, n)| {
                let v = match n {
                    AttrNode::Leaf(v) => v,
                    AttrNode::Branch(m) => attr_node_to_expr(m),
                };
                (k, v)
            })
            .collect(),
    )
}

/// Fold one list element's relative-path assignments into a nested attribute set. A conditioned
/// assignment (from an inner `when-config`) has its value wrapped in `lib.mkIf (<cond>) <value>`.
fn fold_units_into_attrset(units: Vec<Unit>) -> Result<NixExpr, LowerError> {
    let mut root = std::collections::BTreeMap::new();
    for u in units {
        let a = u.assignment;
        let val = match a.condition {
            Some(cond) => NixExpr::Apply(
                Box::new(NixExpr::Select(
                    Box::new(NixExpr::Ref("lib".into())),
                    vec!["mkIf".into()],
                )),
                vec![cond, a.value],
            ),
            None => a.value,
        };
        insert_attr_path(&mut root, &a.path.0, val)?;
    }
    Ok(attr_node_to_expr(root))
}
```

- [ ] **Step 4: Add the `run` arm**

In `run`, add alongside the other arms (after `ForEach`):

```rust
                Stmt::List { path, source, body } => {
                    let mut elems = Vec::new();
                    for item in resolve_list(source, b)? {
                        loops.push(source, item); // the child name is the loop binding
                        let mut elem_units = Vec::new();
                        let res = self.run(body, b, loops, None, &mut elem_units);
                        loops.pop();
                        res?;
                        elems.push(fold_units_into_attrset(elem_units)?);
                    }
                    let assignment = Assignment {
                        path: path.interpret(b, loops)?,
                        value: NixExpr::List(elems),
                        priority: None,
                        condition: cond.map(|c| NixExpr::Raw(RawNix { src: c.to_string(), span: None })),
                        doc: None,
                    };
                    out.push(Unit { bucket: Bucket::Default, assignment, module: String::new() });
                }
```

- [ ] **Step 5: Add the `parse_stmt` arm**

Add before the catch-all `other =>`:

```rust
        "list" => {
            // `list "<path>" from "<source>"`: the bare `from` is noise; take first + last.
            let args: Vec<String> = n
                .entries()
                .iter()
                .filter(|e| e.name().is_none())
                .filter_map(|e| e.value().as_string().map(str::to_string))
                .collect();
            let path = args
                .first()
                .cloned()
                .ok_or_else(|| LowerError::Other("`list` missing path".into()))?;
            let source = args
                .last()
                .cloned()
                .ok_or_else(|| LowerError::Other("`list` missing source".into()))?;
            Ok(Stmt::List { path: parse_path(&path), source, body: parse_stmts(n.children())? })
        }
```

- [ ] **Step 6: Add the `check_stmts` arm**

```rust
            Stmt::List { path, source, body } => {
                for seg in &path.0 {
                    match seg {
                        SegmentTemplate::Interp(lk) => expect_scalar(&lk.0, shapes, loops, errors),
                        SegmentTemplate::QuotedLit(t) => check_str_lookups(t, shapes, loops, errors),
                        SegmentTemplate::Ident(_) => {}
                    }
                }
                match lookup_shape(std::slice::from_ref(source), shapes, loops) {
                    Ok(Shape::List(inner)) => {
                        loops.push((source.as_str(), inner));
                        check_stmts(body, shapes, loops, errors);
                        loops.pop();
                    }
                    Ok(_) => errors.push(format!("list source `{source}` is not a repeated child")),
                    Err(e) => errors.push(e),
                }
            }
```

- [ ] **Step 7: Run to green**

Run: `cargo test -p knixl-modules` and `cargo build --workspace --tests`
Expected: PASS (the new tests plus the existing template tests).

- [ ] **Step 8: Clippy**

Run: `cargo clippy -p knixl-modules --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: No commit.**

---

### Task 2: Document the `list` statement

**Files:**
- Modify: `docs/04-template-grammar.md` (add the fifth statement form)
- Modify: `docs/03-module-system.md` (declarative-capabilities list; drop list-of-attrset as a
  built-in-only signal)

**Interfaces:**
- Consumes: Task 1's `list` behaviour.
- Produces: docs only.

- [ ] **Step 1: docs/04**

Change the heading "## Four statement forms" to "## Five statement forms". Add, after the
`for-each` bullet:

```markdown
- `list "<path>" from "<repeated-child>" { set "<attr>" <value> ... }` : fold a repeated child
  into a list of attribute sets (`[ { ... } { ... } ]`) at `<path>`, one element per child in KDL
  source order. The child name is the loop binding (`from "network"` binds `{network.…}`). Each
  element is built from inner `set` statements (relative attr paths, so nested and quoted keys
  like `config."ipv4.address"` work), optionally gated by `when-flag` (generation-time) or
  `when-config` (which wraps that attr's value in `lib.mkIf`). Two inner sets writing the same
  path is an error.
```

- [ ] **Step 2: docs/03**

In the "A declarative module can only" list, add a bullet after the `for-each`/`collect` one:

```markdown
- fold a repeated child into a list of attribute sets (`list ... from`).
```

If docs/03 states anywhere that a list-of-attrset target is a built-in-only signal, remove or
correct that (this issue lifts it). Keep the remaining boundary (computed priorities from
cross-module conflicts, and buckets other than `Bucket::Default`, are still built-in-only).

- [ ] **Step 3: Prose check**

Re-read both edits for British spelling, no em/en-dashes, no banned vocabulary, and confirm
docs/03 and docs/04 agree on the statement-form count (five) and naming.

- [ ] **Step 4: No commit.**

---

## Self-Review

- Spec coverage: Task 1 delivers the `list` statement (AST, parse, interpret + fold, dry-check)
  with every test the spec lists (fold to List(AttrSet) incl. nested/quoted keys, source order,
  determinism + emit-text, absent child, `when-flag` drop, non-repeated source rejection,
  duplicate-path rejection). Task 2 documents it in docs/03 and docs/04. Out-of-scope items
  (nested list/for-each inside an element, the incus/disko modules) are excluded.
- Placeholders: none; every code step shows the exact code and tests carry their assertions.
- Type consistency: `Stmt::List { path: PathTemplate, source: String, body: Vec<Stmt> }` is
  defined in Step 2 and consumed identically in `run` (Step 4), `parse_stmt` (Step 5), and
  `check_stmts` (Step 6). `fold_units_into_attrset(Vec<Unit>) -> Result<NixExpr, LowerError>`
  (Step 3) is called in Step 4. `NixExpr::{List, AttrSet, Apply, Select, Ref, Raw}` and
  `AttrKey::{Ident, Quoted}` match the IR enums.
