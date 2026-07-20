# In-TUI module authoring implementation plan (#11, slice 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the fixed one-field Author screen with a full authoring workflow for a new
declarative module: a structured schema editor (args, props, children, including structured
children with nested arg/prop sub-fields) and a free-text emit block, live-validated against
the real dry type-pass, writing `modules/<name>/knixl-module.kdl`.

**Architecture:** A pure manifest builder and validator in `knixl-modules` (`ModuleDraft` +
`render_manifest` + `validate_manifest`), consumed by a reworked `tui/author.rs` that holds the
editor state (header inputs, a `Vec` of schema entries with nested sub-fields, and a `textarea`
for emit), computes a dynamic focus list, and rebuilds/validates the draft on every change. The
rendered manifest flows through the unchanged `Nav::Scaffold` -> `Outcome::Scaffold` ->
`commit_scaffold` path.

**Tech Stack:** Rust, knixl-modules (pure), knixl-cli TUI (bubbletea-rs + bubbletea-widgets +
lipgloss).

## Global Constraints

- British spelling in prose and comments. No em-dashes or en-dashes: commas, colons,
  parentheses, full stops.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless,
  delve, and the AI-smell set.
- Do NOT run `cargo fmt` (the repo is not rustfmt-normalised; hand-format to the surrounding
  style of each file).
- Never run git/but or commit in a task; the controller commits.
- Determinism: `render_manifest` is byte-stable for a given draft; entry order is the `entries`
  vector order. No `HashMap` iteration in the render path.
- Validation goes through `DeclarativeModule::from_kdl` only (the real dry type-pass); the
  editor must not reimplement schema/emit checks.
- Another agent has unrelated uncommitted work in the tree (`.superpowers/docreview/*`,
  `assets/cli-workflow.gif`) on branch `docs/review-and-usage-media`. Do NOT touch it.
- This is authoritative-KDL authoring, not a Nix-to-KDL round-trip (ADR 0001 untouched).

---

### Task 1: Manifest builder and validator in knixl-modules

**Files:**
- Modify: `crates/knixl-modules/src/template.rs` (add the draft types, `render_manifest`,
  `validate_manifest`; keep `scaffold_manifest` for now so `tui/author.rs` still compiles)
- Test: `crates/knixl-modules/src/template.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (all `pub`, in `knixl_modules::template`):
  ```rust
  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum FieldTy { Str, Bool, Int }

  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum EntryKind { Arg, Prop, Child }

  #[derive(Clone, Copy, PartialEq, Eq, Debug)]
  pub enum SubKind { Arg, Prop }

  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct SubField { pub kind: SubKind, pub name: String, pub ty: FieldTy, pub required: bool }

  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct SchemaEntry {
      pub kind: EntryKind,
      pub name: String,
      pub ty: FieldTy,
      pub required: bool,
      pub repeated: bool,          // Child only
      pub subfields: Vec<SubField>, // Child only; non-empty => structured child
  }

  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct ModuleDraft {
      pub name: String,
      pub node: String,
      pub summary: String,
      pub entries: Vec<SchemaEntry>,
      pub emit: String,
  }

  pub fn render_manifest(draft: &ModuleDraft) -> String;
  pub fn validate_manifest(text: &str) -> Result<(), String>;
  ```
- Consumes: existing `DeclarativeModule::from_kdl`, and the `kdl` crate already used in the file.

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `template.rs`. `FieldTy`/`EntryKind`/etc. are in scope via
`use super::*;`.

```rust
    fn ty_token(t: FieldTy) -> &'static str {
        match t { FieldTy::Str => "string", FieldTy::Bool => "bool", FieldTy::Int => "int" }
    }

    #[test]
    fn render_manifest_is_deterministic() {
        let draft = ModuleDraft {
            name: "cache".into(),
            node: String::new(), // defaults to name
            summary: "a cache".into(),
            entries: vec![SchemaEntry {
                kind: EntryKind::Arg, name: "host".into(), ty: FieldTy::Str,
                required: true, repeated: false, subfields: vec![],
            }],
            emit: "set \"services.cache.enable\" #true".into(),
        };
        assert_eq!(render_manifest(&draft), render_manifest(&draft));
    }

    #[test]
    fn render_manifest_flat_entries_and_node_default() {
        let draft = ModuleDraft {
            name: "svc".into(),
            node: String::new(),
            summary: "does things".into(),
            entries: vec![
                SchemaEntry { kind: EntryKind::Arg, name: "host".into(), ty: FieldTy::Str, required: true, repeated: false, subfields: vec![] },
                SchemaEntry { kind: EntryKind::Prop, name: "port".into(), ty: FieldTy::Int, required: false, repeated: false, subfields: vec![] },
                SchemaEntry { kind: EntryKind::Child, name: "alias".into(), ty: FieldTy::Str, required: false, repeated: true, subfields: vec![] },
            ],
            emit: "set \"services.svc.enable\" #true".into(),
        };
        let m = render_manifest(&draft);
        assert!(m.contains("claims-node \"svc\""), "node defaults to name: {m}");
        assert!(m.contains("arg \"host\" type=\"string\" required=#true"), "{m}");
        assert!(m.contains("prop \"port\" type=\"int\" required=#false"), "{m}");
        assert!(m.contains("child \"alias\" type=\"string\" required=#false repeated=#true"), "{m}");
        assert!(m.contains("set \"services.svc.enable\" #true"), "emit spliced: {m}");
        // The rendered manifest must load and dry-type-check.
        validate_manifest(&m).expect("rendered flat draft is valid");
    }

    #[test]
    fn render_manifest_structured_child() {
        let draft = ModuleDraft {
            name: "web".into(),
            node: "web".into(),
            summary: String::new(),
            entries: vec![
                SchemaEntry { kind: EntryKind::Arg, name: "host".into(), ty: FieldTy::Str, required: true, repeated: false, subfields: vec![] },
                SchemaEntry {
                    kind: EntryKind::Child, name: "acme".into(), ty: FieldTy::Str,
                    required: true, repeated: false,
                    subfields: vec![
                        SubField { kind: SubKind::Prop, name: "email".into(), ty: FieldTy::Str, required: true },
                    ],
                },
            ],
            emit: "set \"services.web.virtualHosts.{host}.enable\" #true".into(),
        };
        let m = render_manifest(&draft);
        // Structured child: block form, no type=/repeated= on the child line.
        assert!(m.contains("child \"acme\" required=#true {"), "structured child block: {m}");
        assert!(m.contains("prop \"email\" type=\"string\" required=#true"), "{m}");
        assert!(!m.contains("child \"acme\" type="), "structured child omits type=: {m}");
        validate_manifest(&m).expect("rendered structured draft is valid");
    }

    #[test]
    fn validate_manifest_rejects_an_undeclared_binding() {
        // emit references {missing}, which the dry type-pass rejects at load.
        let draft = ModuleDraft {
            name: "bad".into(), node: "bad".into(), summary: String::new(),
            entries: vec![SchemaEntry { kind: EntryKind::Arg, name: "host".into(), ty: FieldTy::Str, required: true, repeated: false, subfields: vec![] }],
            emit: "set \"services.bad.{missing}\" #true".into(),
        };
        let err = validate_manifest(&render_manifest(&draft)).unwrap_err();
        assert!(err.contains("missing") || err.contains("unknown binding"), "got: {err}");
    }

    #[test]
    fn validate_manifest_reports_a_kdl_parse_error() {
        assert!(validate_manifest("this is { not valid").is_err());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-modules render_manifest` and `cargo test -p knixl-modules validate_manifest`
Expected: FAIL to compile (the types and functions do not exist yet).

- [ ] **Step 3: Add the types**

Add the enums and structs from the Interfaces block above near `scaffold_manifest` in
`template.rs`. Keep `scaffold_manifest` and `ModuleScaffold` in place (removed in Task 3).

- [ ] **Step 4: Implement `render_manifest`**

Deterministic KDL. Reuse the same escaping `scaffold_manifest` uses
(`v.replace('\\', "\\\\").replace('"', "\\\"")`). Build with a `String` and `push_str`/`writeln!`,
not any hashed collection.

```rust
pub fn render_manifest(draft: &ModuleDraft) -> String {
    let esc = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let ty = |t: FieldTy| match t { FieldTy::Str => "string", FieldTy::Bool => "bool", FieldTy::Int => "int" };
    let node = if draft.node.trim().is_empty() { draft.name.trim() } else { draft.node.trim() };

    let mut s = String::new();
    s.push_str(&format!("module name=\"{}\" version=\"0.1.0\" {{\n", esc(draft.name.trim())));
    s.push_str(&format!("    summary \"{}\"\n", esc(draft.summary.trim())));
    s.push_str(&format!("    claims-node \"{}\"\n\n", esc(node)));
    s.push_str("    schema {\n");
    for e in &draft.entries {
        let kw = match e.kind { EntryKind::Arg => "arg", EntryKind::Prop => "prop", EntryKind::Child => "child" };
        let structured = e.kind == EntryKind::Child && !e.subfields.is_empty();
        if structured {
            s.push_str(&format!("        child \"{}\" required=#{} {{\n", esc(e.name.trim()), e.required));
            for sf in &e.subfields {
                let skw = match sf.kind { SubKind::Arg => "arg", SubKind::Prop => "prop" };
                s.push_str(&format!(
                    "            {skw} \"{}\" type=\"{}\" required=#{} doc=\"\"\n",
                    esc(sf.name.trim()), ty(sf.ty), sf.required,
                ));
            }
            s.push_str("        }\n");
        } else if e.kind == EntryKind::Child {
            s.push_str(&format!(
                "        child \"{}\" type=\"{}\" required=#{} repeated=#{} doc=\"\"\n",
                esc(e.name.trim()), ty(e.ty), e.required, e.repeated,
            ));
        } else {
            s.push_str(&format!(
                "        {kw} \"{}\" type=\"{}\" required=#{} doc=\"\"\n",
                esc(e.name.trim()), ty(e.ty), e.required,
            ));
        }
    }
    s.push_str("    }\n\n");
    s.push_str("    emit {\n");
    for line in draft.emit.lines() {
        if line.trim().is_empty() { s.push('\n'); } else { s.push_str(&format!("        {line}\n")); }
    }
    s.push_str("    }\n}\n");
    s
}
```

- [ ] **Step 5: Implement `validate_manifest`**

```rust
pub fn validate_manifest(text: &str) -> Result<(), String> {
    let doc = text.parse::<kdl::KdlDocument>().map_err(|e| e.to_string())?;
    DeclarativeModule::from_kdl(&doc, std::path::Path::new("draft"))
        .map(|_| ())
        .map_err(|e| e.to_string())
}
```

(If `LowerError` does not implement `Display` cleanly, use `format!("{e}")`; confirm against the
existing `from_kdl` error type in the file.)

- [ ] **Step 6: Run to verify pass**

Run: `cargo test -p knixl-modules` and `cargo build --workspace --tests`
Expected: PASS (new tests green; `scaffold_manifest` and `tui/author.rs` still compile).

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p knixl-modules --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: No commit.**

---

### Task 2: Rework the Author screen into the schema + emit editor

**Files:**
- Modify: `crates/knixl-cli/src/tui/author.rs` (full rework of `AuthorModel`, its reducer, view,
  and tests)
- Test: `crates/knixl-cli/src/tui/author.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: Task 1 `ModuleDraft`, `SchemaEntry`, `SubField`, `EntryKind`, `SubKind`, `FieldTy`,
  `render_manifest`, `validate_manifest` (from `knixl_modules::template`); the existing
  `Nav::Scaffold { name, manifest }` (unchanged); `bubbletea_widgets::{textinput, textarea}`;
  `bubbletea_widgets::Component` (brings `textarea.focus()/.blur()` into scope);
  `super::{theme, widgets, Nav, Step}`.
- Produces: a reworked `AuthorModel` with the same public surface the parent uses
  (`AuthorModel::enter(size) -> AuthorModel`, `update(&mut self, msg, size) -> Step`,
  `view(&self, size) -> String`). No change to `tui/mod.rs`.

**Model shape:**

```rust
pub struct AuthorModel {
    name: textinput::Model,
    node: textinput::Model,
    summary: textinput::Model,
    entries: Vec<EntryState>,
    emit: textarea::Model,
    focus: usize,          // index into the computed focus list
    status: Result<(), String>, // cached validation of the current draft
}

struct EntryState {
    kind: EntryKind,
    name: textinput::Model,
    ty: FieldTy,
    required: bool,
    repeated: bool,
    subfields: Vec<SubFieldState>,
}

struct SubFieldState { kind: SubKind, name: textinput::Model, ty: FieldTy, required: bool }
```

A focus point is an enum computed on demand (never stored as a big vector on the struct):

```rust
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Focus {
    Name, Node, Summary,
    EntryKind(usize), EntryName(usize), EntryType(usize), EntryRequired(usize), EntryRepeated(usize),
    SubKind(usize, usize), SubName(usize, usize), SubType(usize, usize), SubRequired(usize, usize),
    AddSub(usize), DeleteEntry(usize),
    AddEntry, Emit, Create, Cancel,
}
```

- [ ] **Step 1: Write the failing tests**

Replace the existing `tests` module with these (they define the behaviour of the reworked
screen). Key helper mirrors the current file.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> AuthorModel { AuthorModel::enter((100, 40)) }
    fn key(c: KeyCode) -> Msg { Box::new(KeyMsg { key: c, modifiers: KeyModifiers::NONE }) as Msg }
    fn press(m: &mut AuthorModel, c: KeyCode) { m.update(key(c), (100, 40)); }

    #[test]
    fn starts_with_a_seed_entry_and_emit() {
        let m = model();
        assert_eq!(m.entries.len(), 1, "one seed entry");
        assert!(!m.emit.value().trim().is_empty(), "seed emit line present");
    }

    #[test]
    fn focus_moves_through_the_dynamic_list_and_wraps() {
        let mut m = model();
        assert_eq!(m.focus_at(), Focus::Name);
        // Tab from the last control wraps to the first.
        let last = m.focus_list().len() - 1;
        m.focus = last;
        press(&mut m, KeyCode::Tab);
        assert_eq!(m.focus, 0);
        assert_eq!(m.focus_at(), Focus::Name);
    }

    #[test]
    fn add_and_delete_entry() {
        let mut m = model();
        let before = m.entries.len();
        m.add_entry();
        assert_eq!(m.entries.len(), before + 1);
        m.delete_entry(before); // remove the one just added
        assert_eq!(m.entries.len(), before);
    }

    #[test]
    fn add_subfield_only_on_a_child_and_delete() {
        let mut m = model();
        m.entries[0].kind = EntryKind::Child;
        m.add_subfield(0);
        assert_eq!(m.entries[0].subfields.len(), 1);
        m.delete_subfield(0, 0);
        assert!(m.entries[0].subfields.is_empty());
    }

    #[test]
    fn cycles_and_toggles() {
        let mut m = model();
        m.entries[0].ty = FieldTy::Str;
        m.cycle_entry_type(0, true);
        assert_eq!(m.entries[0].ty, FieldTy::Bool);
        m.cycle_entry_type(0, false);
        assert_eq!(m.entries[0].ty, FieldTy::Str);
        let r = m.entries[0].required;
        m.toggle_entry_required(0);
        assert_eq!(m.entries[0].required, !r);
        m.entries[0].kind = EntryKind::Arg;
        m.cycle_entry_kind(0, true);
        assert_eq!(m.entries[0].kind, EntryKind::Prop);
    }

    #[test]
    fn draft_reflects_the_editor_state() {
        let mut m = model();
        m.name.set_value("cache");
        m.entries[0].kind = EntryKind::Arg;
        m.entries[0].name.set_value("host");
        m.entries[0].ty = FieldTy::Str;
        let d = m.draft();
        assert_eq!(d.name, "cache");
        assert_eq!(d.entries[0].name, "host");
    }

    #[test]
    fn create_gated_on_name_and_validity() {
        let mut m = model();
        m.name.set_value(""); // no name
        m.recompute_status();
        assert!(!m.can_create(), "empty name blocks create");
        m.name.set_value("cache");
        m.entries[0].name.set_value("host");
        m.recompute_status();
        assert!(m.can_create(), "valid named draft can create: {:?}", m.status);
    }

    #[test]
    fn create_emits_scaffold_with_a_valid_manifest() {
        let mut m = model();
        m.name.set_value("cache");
        m.entries[0].name.set_value("host");
        m.recompute_status();
        // Move focus to Create and press Enter.
        let idx = m.focus_list().iter().position(|f| *f == Focus::Create).unwrap();
        m.focus = idx;
        let step = m.update(key(KeyCode::Enter), (100, 40));
        match step.nav {
            Nav::Scaffold { name, manifest } => {
                assert_eq!(name, "cache");
                knixl_modules::template::validate_manifest(&manifest).expect("emitted manifest valid");
            }
            _ => panic!("expected Nav::Scaffold"),
        }
    }

    #[test]
    fn esc_backs_out() {
        let mut m = model();
        let step = m.update(key(KeyCode::Esc), (100, 40));
        assert!(matches!(step.nav, Nav::Back));
    }

    #[test]
    fn view_shows_sections_and_resize_hint() {
        assert!(model().view((100, 40)).contains("new module"));
        assert!(model().view((100, 40)).contains("schema"));
        assert!(model().view((100, 40)).contains("emit"));
        assert!(model().view((20, 8)).contains("resize"));
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-cli author`
Expected: FAIL to compile (the reworked model, methods, and `Focus` do not exist yet).

- [ ] **Step 3: Build the model, focus list, and pure mutations**

Implement `AuthorModel` with:
- `enter(size)`: seed `name`/`node`/`summary` textinputs (placeholders as today: node
  "defaults to name"); seed `entries` with one `EntryState { kind: Arg, name: "" input, ty:
  Str, required: false, repeated: false, subfields: [] }`; seed `emit` textarea via
  `set_value("set \"services.<node>.enable\" #true")` using the name/node placeholder
  (`services.new-module.enable` is fine as a starter, since node is blank at start); `focus: 0`;
  then `recompute_status()`.
- `focus_list(&self) -> Vec<Focus>`: build the vector per the spec algorithm (Name, Node,
  Summary; per entry its cells with `EntryRepeated` only when `kind == Child`, then per subfield
  its cells and `AddSub` only when `kind == Child`, then `DeleteEntry`; then `AddEntry`, `Emit`,
  `Create`, `Cancel`).
- `focus_at(&self) -> Focus`: `self.focus_list()[self.focus.min(len-1)]`.
- `focus_next`/`focus_prev`: `self.focus = (self.focus +/- 1) mod focus_list().len()`, then
  `refocus()`.
- `refocus(&mut self)`: focus the textinput/textarea matching `focus_at()` and blur the rest
  (only one widget shows a cursor). The header inputs, each entry/sub name input, and the emit
  textarea are the focusable widgets.
- Pure mutations (each `pub`/private but unit-tested): `add_entry`, `delete_entry(i)`,
  `add_subfield(i)`, `delete_subfield(i, j)`, `cycle_entry_kind(i, fwd)`,
  `cycle_entry_type(i, fwd)`, `toggle_entry_required(i)`, `toggle_entry_repeated(i)`, and the
  sub-field equivalents. After any structural mutation, clamp `self.focus` to
  `focus_list().len() - 1` and call `recompute_status()`.
- `draft(&self) -> ModuleDraft`: read the widget values into `ModuleDraft`/`SchemaEntry`/
  `SubField`. For a `Child` with non-empty `subfields`, the draft carries them (render treats it
  as structured).
- `recompute_status(&mut self)`: `self.status = validate_manifest(&render_manifest(&self.draft()))`.
- `can_create(&self) -> bool`: `!self.name.value().trim().is_empty() && self.status.is_ok()`.

FieldTy cycle order: `Str -> Bool -> Int -> Str`. EntryKind cycle: `Arg -> Prop -> Child -> Arg`.
SubKind cycle: `Arg -> Prop -> Arg`.

- [ ] **Step 4: Build the reducer (`update`)**

Mirror the current dispatch shape:
- `WindowSizeMsg`: size the emit textarea to the available width/height, `Step::stay()`.
- Ctrl-c or Esc: `Nav::Back`.
- Tab / Down: `focus_next()`; BackTab / Up: `focus_prev()`. EXCEPTION: when `focus_at() ==
  Focus::Emit`, Up/Down are forwarded to the textarea (cursor movement); only Tab/BackTab move
  focus out of the emit editor.
- Otherwise dispatch on `focus_at()`:
  - Kind/Type cells: Left cycles back, Right/Space cycles forward.
  - Required/Repeated cells: Space/Enter toggle.
  - `AddEntry`/`AddSub(i)`: Enter calls the mutation.
  - `DeleteEntry(i)`/(sub delete reachable via a key on the sub row, e.g. Enter on `SubKind`
    is the kind cycle, so put delete-subfield on the `AddSub`/a dedicated `Delete` focus point;
    simplest: a `DeleteEntry(i)` control per entry as in the focus list, and delete-subfield via
    a `Delete`-keyed action — keep it a focusable control if you add one, else document that
    sub-fields are removed by deleting and re-adding). Keep the focus list and the tests in
    sync: the tests call `delete_subfield` directly, so it must exist as a method even if its
    only key binding is via a control you choose.
  - Name cells (`Name`/`Node`/`Summary`/`EntryName`/`SubName`): forward the key to the bound
    textinput (`let cmd = input.update(msg); return Step { nav: Nav::Stay, cmd };`), then
    `recompute_status()`.
  - `Emit`: forward the key to the textarea, then `recompute_status()`.
  - `Create`: Enter, when `can_create()`, returns `Nav::Scaffold { name:
    self.name.value().trim().into(), manifest: render_manifest(&self.draft()) }`.
  - `Cancel`: Enter returns `Nav::Back`.
- After any key that mutates state (cycles, toggles, add/delete, text edits, emit edits), call
  `recompute_status()` before returning so the status line and `can_create()` stay live.

- [ ] **Step 5: Build the view**

Render, top to bottom, matching the file's lipgloss style (`theme::chip`, `theme::dim`,
`theme::accent`, `theme::selected`, `theme::toggle`, `widgets::footer`, `marker(focused)`):
- The `" new module "` chip.
- Header rows: name, node, summary (label + `input.view()`), each with a focus marker.
- A `schema` sub-heading, then one row per entry: `kind` (cycled, `< kind >`), name input,
  `type` (cycled), `required` toggle, and `repeated` toggle for children; indented sub-field
  rows under a structured child; an `+ add sub-field` control under a child; a `- delete`
  control per entry. An `+ add entry` control after the list.
- An `emit` sub-heading then `self.emit.view()` inside a bordered panel.
- A status line: `theme::dim().render("valid")` when `status.is_ok()`, else the error string in
  an error style (reuse `theme::amber()` or add a red style in `theme` if none exists; do not
  invent unrelated styles).
- `create` and `cancel` buttons (reuse the `button` helper), create dimmed when
  `!can_create()`.
- A `widgets::footer` with the key hints (tab move, arrows cycle/edit, space toggle, enter
  create, esc back).
- Keep the tiny-terminal guard at the top (`if size.0 < NN || size.1 < NN { return
  theme::dim().render("terminal too small ... resize ...") }`), widened as needed for the taller
  layout; the test only checks the word "resize" for `(20, 8)`.

- [ ] **Step 6: Run to verify pass**

Run: `cargo test -p knixl-cli author` then `cargo test -p knixl-cli` and `cargo build --workspace --tests`
Expected: PASS.

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p knixl-cli --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: No commit.**

---

### Task 3: Remove the superseded scaffold builder

**Files:**
- Modify: `crates/knixl-modules/src/template.rs` (remove `scaffold_manifest` and
  `ModuleScaffold`, and their test `scaffold_manifest_loads_and_type_checks`)

**Interfaces:**
- Consumes: nothing new. Precondition: `tui/author.rs` (Task 2) no longer references
  `scaffold_manifest`/`ModuleScaffold`.

- [ ] **Step 1: Confirm no remaining references**

Run: `grep -rn "scaffold_manifest\|ModuleScaffold" crates/`
Expected: matches only inside `template.rs` (the definition and its test). If anything else
references them, STOP: Task 2 is incomplete.

- [ ] **Step 2: Remove them**

Delete the `scaffold_manifest` function, the `ModuleScaffold` struct, and the
`scaffold_manifest_loads_and_type_checks` test. Leave `render_manifest`/`validate_manifest` and
their tests.

- [ ] **Step 3: Run**

Run: `cargo test -p knixl-modules -p knixl-cli`, `cargo build --workspace --tests`, and
`cargo clippy --all-targets -- -D warnings`
Expected: all PASS, no dead-code or unused-import warnings.

- [ ] **Step 4: No commit.**

---

## Self-Review

- Spec coverage: Task 1 builds the pure `ModuleDraft`/`render_manifest`/`validate_manifest`
  (structured children included) with the determinism, flat, structured, and validation tests
  the spec lists. Task 2 delivers the sectioned editor: dynamic focus list, add/delete entry and
  sub-field, kind/type cycles, required/repeated toggles, the emit textarea, live validation,
  create-gating, and the `Nav::Scaffold` emit, with reducer tests. Task 3 removes the superseded
  scaffold path. Out-of-scope items (edit-existing, structured emit editor, migrations/doc/
  version editing) are excluded.
- Placeholders: none; Task 1 shows full code, Task 2 gives the exact model, focus enum, mutation
  set, reducer rules, and complete tests (the view is specified by its rendering rules and
  contains-assertions, matching how the current screen is tested).
- Type consistency: the draft types defined in Task 1 (`ModuleDraft`, `SchemaEntry`, `SubField`,
  `EntryKind`, `SubKind`, `FieldTy`) are consumed unchanged by Task 2's `draft()`/`recompute_status`
  and by the tests' `validate_manifest` call. `render_manifest`/`validate_manifest` signatures
  match across tasks. `Nav::Scaffold { name, manifest }` is used exactly as the current screen
  and `commit_scaffold` expect, so no `tui/mod.rs` or `main.rs` change is required.
- Ordering: `scaffold_manifest` is kept through Tasks 1-2 (so the workspace stays green) and
  removed only in Task 3, after Task 2 stops using it.
