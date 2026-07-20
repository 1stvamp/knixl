# Declarative runtime condition implementation plan (#16)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a fourth declarative template statement, `when-config "<cond>" { ... }`, that
wraps its body's assignments in a runtime `lib.mkIf (<cond>) <value>`, with `{lookup}`
interpolation and load-time dry-checking.

**Architecture:** A new `Stmt::WhenConfig { cond: Vec<StrPart>, body }` in
`knixl-modules/src/template.rs`. Parsing adds a `when-config` arm; interpretation threads the
active condition (already-interpolated Nix text) down the `run` recursion, AND-combining nested
conditions and stamping `Assignment.condition = Some(NixExpr::Raw(..))` on each `set`;
`check_stmts` gains a `WhenConfig` arm that scalar-checks the condition's `{lookup}` parts. The
IR emit path is unchanged (it already renders `lib.mkIf`).

**Tech Stack:** Rust, knixl-modules (template interpreter), knixl-ir (IR + emit).

## Global Constraints

- British spelling in prose and comments. No em-dashes or en-dashes: use commas, parentheses,
  colons, full stops.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless,
  delve, and the AI-smell set.
- Do NOT run `cargo fmt` (the repo is not rustfmt-normalised; hand-format to surrounding style).
- Never run git/but or commit in a task; the controller commits.
- Determinism: the condition text is a pure function of inputs (interpolation is deterministic,
  body order is KDL source order). No `HashMap` on the emit path.
- The condition is opaque raw Nix (off `config.*`); knixl does not parse or validate the Nix
  expression itself, matching `backups`' `when=`.
- `NixExpr`/`RawNix`/`AttrKey`/`AttrPath`/`Assignment`/`Emit`/`Writer` come from `knixl_ir`.

---

### Task 1: `when-config` statement in the template interpreter

**Files:**
- Modify: `crates/knixl-modules/src/template.rs`
  - import (line 5): add `RawNix` to the `use knixl_ir::{...}` list
  - `Stmt` enum (lines 11-15): add `WhenConfig` variant
  - `EmitTemplate::run` (lines 146-172): thread `cond: Option<&str>` and add the `WhenConfig` arm
  - `EmitTemplate::interpret` (lines 136-141): pass `None` as the initial condition
  - `parse_stmt` (lines 423-455): add the `"when-config"` arm
  - `check_stmts` (lines 591-642): add the `WhenConfig` arm
- Test: `crates/knixl-modules/src/template.rs` (the existing `#[cfg(test)] mod tests`)

**Interfaces:**
- Produces: `Stmt::WhenConfig { cond: Vec<StrPart>, body: Vec<Stmt> }`; a `set` reached under
  one or more `when-config` blocks emits `Assignment { condition: Some(NixExpr::Raw(RawNix {
  src, span: None })), .. }` where `src` is the interpolated (and, when nested, `(A) && (B)`
  AND-combined) condition text.
- Consumes: existing `StrPart`, `parse_str_parts`, `interp_parts`, `expect_scalar`,
  `LoopScopes`, `Bindings`.

- [ ] **Step 1: Write the failing tests**

Add these tests to the `tests` module at the bottom of `template.rs`. They reuse the existing
`node`, `lower`, `find`, and `path_str` helpers already defined in that module. `lower` builds
a `LowerOutput`; `find` returns the `&NixExpr` value for a path. To read the `condition`, add a
small local helper `find_assignment` alongside them in the first test (shown inline below).

```rust
    #[test]
    fn when_config_stamps_an_interpolated_runtime_condition() {
        use knixl_ir::{Emit, Writer};
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n        arg \"svc\" type=\"string\" required=#true\n    }\n    emit {\n        when-config \"config.services.{svc}.enable\" {\n            set \"services.foo.{host}.enable\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module = DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\" \"postgresql\""));

        let a = out
            .units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| path_str(a) == "services.foo.\"web\".enable")
            .expect("assignment present");

        match &a.condition {
            Some(NixExpr::Raw(r)) => assert_eq!(r.src, "config.services.postgresql.enable"),
            other => panic!("condition = {other:?}"),
        }

        // End-to-end: the IR emit path renders lib.mkIf for this assignment.
        let mut w = Writer::new();
        a.emit(&mut w);
        assert!(
            w.into_string().contains("lib.mkIf (config.services.postgresql.enable)"),
            "expected a lib.mkIf wrapper in the emitted text"
        );
    }

    #[test]
    fn a_set_outside_when_config_has_no_condition() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n    }\n    emit {\n        set \"services.foo.{host}.enable\" #true\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module = DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\""));
        assert!(out.units.iter().all(|u| u.assignment.condition.is_none()));
    }

    #[test]
    fn nested_when_config_and_combines() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n    }\n    emit {\n        when-config \"config.a.enable\" {\n            when-config \"config.b.enable\" {\n                set \"services.foo.{host}.enable\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module = DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\""));
        let a = out.units.iter().map(|u| &u.assignment).next().expect("one assignment");
        match &a.condition {
            Some(NixExpr::Raw(r)) => assert_eq!(r.src, "(config.a.enable) && (config.b.enable)"),
            other => panic!("condition = {other:?}"),
        }
    }

    #[test]
    fn when_config_inside_for_each_interpolates_the_loop_var() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        for-each \"it\" in \"item\" {\n            when-config \"config.services.{it}.enable\" {\n                set \"p.{it}\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module = DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond {\n    item \"nginx\"\n    item \"sshd\"\n}"));

        let cond_for = |name: &str| {
            out.units
                .iter()
                .map(|u| &u.assignment)
                .find(|a| path_str(a) == format!("p.\"{name}\""))
                .and_then(|a| match &a.condition {
                    Some(NixExpr::Raw(r)) => Some(r.src.clone()),
                    _ => None,
                })
        };
        assert_eq!(cond_for("nginx").as_deref(), Some("config.services.nginx.enable"));
        assert_eq!(cond_for("sshd").as_deref(), Some("config.services.sshd.enable"));
    }

    #[test]
    fn when_flag_false_drops_a_when_config_body() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        child \"on\" type=\"bool\"\n    }\n    emit {\n        when-flag \"on\" {\n            when-config \"config.a.enable\" {\n                set \"p\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module = DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond")); // `on` absent => flag false
        assert!(out.units.is_empty(), "generation-time gate should drop the body");
    }

    #[test]
    fn dry_check_rejects_a_non_scalar_lookup_in_a_condition() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"acme\" {\n            prop \"email\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        when-config \"config.{acme}.enable\" {\n            set \"p\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad")).err().unwrap();
        assert!(format!("{err}").contains("not a scalar"), "got: {err}");
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p knixl-modules when_config` and `cargo test -p knixl-modules dry_check_rejects_a_non_scalar_lookup_in_a_condition`
Expected: FAIL. Before the enum variant exists the module will not compile (the `when-config`
parse arm returns the "unknown emit statement" error, so the load-based tests would fail even
if it did compile). That is the expected red.

- [ ] **Step 3: Add the `WhenConfig` AST variant and the `RawNix` import**

In the `use knixl_ir::{...}` at line 5, add `RawNix`:

```rust
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr, RawNix};
```

In the `Stmt` enum (lines 11-15) add the variant (keep the existing three):

```rust
pub enum Stmt {
    Set { path: PathTemplate, value: ValueTemplate },
    WhenFlag { flag: String, body: Vec<Stmt> },               // generation-time gate
    WhenConfig { cond: Vec<StrPart>, body: Vec<Stmt> },        // runtime lib.mkIf off config.*
    ForEach { var: String, source: String, body: Vec<Stmt> }, // binds <var> per item
}
```

- [ ] **Step 4: Thread the active condition through `run` and stamp it on each `set`**

Replace `EmitTemplate::interpret` (lines 136-141) and `EmitTemplate::run` (lines 146-172) with:

```rust
    pub fn interpret(&self, b: &Bindings) -> Result<LowerOutput, LowerError> {
        let mut units = Vec::new();
        let mut loops = LoopScopes::new();
        self.run(&self.stmts, b, &mut loops, None, &mut units)?;
        Ok(LowerOutput::units(units))
    }

    // self only recurses today; kept as a method so template state stays reachable
    // once bind/interpret are fleshed out.
    #[allow(clippy::only_used_in_recursion)]
    fn run(
        &self,
        stmts: &[Stmt],
        b: &Bindings,
        loops: &mut LoopScopes,
        cond: Option<&str>,
        out: &mut Vec<Unit>,
    ) -> Result<(), LowerError> {
        for st in stmts {
            match st {
                Stmt::Set { path, value } => {
                    let a = Assignment {
                        path: path.interpret(b, loops)?,
                        value: value.interpret(b, loops)?, // Collect => NixExpr::List
                        priority: None,
                        condition: cond.map(|c| NixExpr::Raw(RawNix { src: c.to_string(), span: None })),
                        doc: None,
                    };
                    out.push(Unit { bucket: Bucket::Default, assignment: a, module: String::new() });
                }
                Stmt::WhenFlag { flag, body } => {
                    if resolve_flag(flag, b, loops)? { self.run(body, b, loops, cond, out)?; }
                }
                Stmt::WhenConfig { cond: parts, body } => {
                    let inner = interp_parts(parts, b, loops)?;
                    // Nested conditions conjoin: `lib.mkIf ((A) && (B)) ..`.
                    let combined = match cond {
                        Some(outer) => format!("({outer}) && ({inner})"),
                        None => inner,
                    };
                    self.run(body, b, loops, Some(&combined), out)?;
                }
                Stmt::ForEach { var, source, body } => {
                    for item in resolve_list(source, b)? { // source order => stable
                        loops.push(var, item);
                        self.run(body, b, loops, cond, out)?;
                        loops.pop();
                    }
                }
            }
        }
        Ok(())
    }
```

- [ ] **Step 5: Add the `when-config` parse arm**

In `parse_stmt` (lines 423-455), add an arm before the catch-all `other =>`:

```rust
        "when-config" => {
            let cond = arg_str(n, 0)
                .ok_or_else(|| LowerError::Other("`when-config` missing condition".into()))?;
            Ok(Stmt::WhenConfig { cond: parse_str_parts(&cond), body: parse_stmts(n.children())? })
        }
```

- [ ] **Step 6: Add the `WhenConfig` dry-check arm**

In `check_stmts` (lines 591-642), add an arm alongside the others. Scalar-check every
`{lookup}` in the condition, then recurse into the body:

```rust
            Stmt::WhenConfig { cond, body } => {
                for part in cond {
                    if let StrPart::Interp(lk) = part {
                        expect_scalar(&lk.0, shapes, loops, errors);
                    }
                }
                check_stmts(body, shapes, loops, errors);
            }
```

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p knixl-modules` and `cargo build --workspace --tests`
Expected: PASS (the six new tests plus the existing template tests).

- [ ] **Step 8: Clippy**

Run: `cargo clippy -p knixl-modules --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 9: No commit.**

---

### Task 2: Update the grammar and boundary docs

**Files:**
- Modify: `docs/04-template-grammar.md` (the "Three statement forms" section, lines 5-11)
- Modify: `docs/03-module-system.md` (the declarative-limitations list, lines 47-53)

**Interfaces:**
- Consumes: Task 1's `when-config` behaviour (block form, `{lookup}` interpolation, AND
  nesting, raw-Nix condition).
- Produces: docs only; no code.

- [ ] **Step 1: Update docs/04 to four statement forms**

Change the heading "## Three statement forms, and no more" to "## Four statement forms". Add,
after the `when-flag` bullet (line 10):

```markdown
- `when-config "<cond>" { ... }` : runtime gate. Always emits its body, wrapping each
  assignment in `lib.mkIf (<cond>) <value>`. The condition is raw Nix off `config.*` with
  `{lookup}` interpolation (dry-checked at load like a `set` path); the Nix expression itself
  is opaque and unvalidated. Nested `when-config` conjoin: `(A) && (B)`.
```

Update the parenthetical on line 7 (`(substitute, repeat-into-list, gate-on-flag)`) to also
name the runtime gate, e.g. `(substitute, repeat-into-list, gate-on-flag, gate-at-runtime)`.

- [ ] **Step 2: Update docs/03 to lift the boundary**

In the "A declarative module can only" list (lines 47-53), add a bullet after the `when-flag`
one:

```markdown
- gate a block on a runtime `config.*` condition (`when-config`, emitted as `lib.mkIf`).
```

Then edit the closing paragraph (line 53) so it no longer claims declarative modules cannot
emit a runtime `lib.mkIf`. It should still list what remains built-in-only: computing
priorities from cross-module conflicts and writing buckets other than `Bucket::Default`. Note
that a runtime condition alone no longer forces a module to be a built-in (so `backups` could
in principle be declarative; converting it is out of scope here).

- [ ] **Step 3: Prose check**

Re-read both edits for British spelling, no em/en-dashes, and no banned vocabulary. Confirm
docs/03 and docs/04 agree on the count and wording of the statement forms.

- [ ] **Step 4: No commit.**

---

## Self-Review

- Spec coverage: Task 1 delivers the `when-config` statement (AST, parse, interpret with
  condition threading + AND nesting, dry-check) and every test the spec lists, including the
  end-to-end `lib.mkIf` emit assertion. Task 2 updates docs/03 and docs/04, the two docs the
  spec calls out. Out-of-scope items (converting backups, validating the Nix expression, new
  priority/bucket power) are excluded.
- Placeholders: none; every code step shows the exact code and every test carries its
  assertions.
- Type consistency: `Stmt::WhenConfig { cond: Vec<StrPart>, body: Vec<Stmt> }` is defined in
  Step 3 and consumed identically in `run` (Step 4), `parse_stmt` (Step 5), and `check_stmts`
  (Step 6). `run`'s new `cond: Option<&str>` parameter is added at the definition and every
  call site (interpret + the three recursive calls) in Step 4. `NixExpr::Raw(RawNix { .. })`
  matches the `backups` construction and the `condition: Option<NixExpr>` field type.
