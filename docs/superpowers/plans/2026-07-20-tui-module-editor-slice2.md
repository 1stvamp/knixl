# In-TUI module editing implementation plan (#11, slice 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the TUI edit an existing declarative module through the slice-1 structured editor,
writing it back via in-place KDL mutation so version, migrations, `doc=` strings, comments, and
formatting the editor does not model survive byte-for-byte.

**Architecture:** A pure `load_editable`/`reconcile` pair in `knixl-modules` (reconcile clones
the original parsed `KdlDocument` and mutates only the editor-owned parts). The Author screen
becomes mode-aware (`New` uses `render_manifest`; `Edit` uses `reconcile` and holds the original
document). Browse gains an edit action that opens Edit mode; a new `Outcome::SaveModule`
overwrites the manifest.

**Tech Stack:** Rust, knixl-modules (pure, kdl 6.7.1), knixl (CLI + bubbletea-rs TUI).

## Global Constraints

- British spelling in prose and comments. No em-dashes or en-dashes.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless,
  delve, and the AI-smell set.
- Do NOT run `cargo fmt` (repo is not rustfmt-normalised; hand-format to each file's style).
- Never run git/but or commit in a task; the controller commits. Do NOT run `git stash`/`git
  status`/any git command (another agent may have parallel uncommitted work).
- The CLI crate is `knixl` (path `crates/knixl/`), renamed from `knixl-cli`.
- New mode must stay byte-for-byte unchanged: `render_manifest` ignores the new `origin` field.
- Fidelity is the point: `reconcile` must preserve the module `version`, the `migrations` block,
  existing `doc=` strings on kept fields, comments, and any node the editor does not model.
- Validation goes only through `validate_manifest` (the real dry type-pass).
- `kdl::KdlDocument`/`KdlNode` are `Clone`; they are not `Send`, so the Author model owns the
  document on the main thread and no async closure touches it.

---

### Task 1: `load_editable` and `reconcile` in knixl-modules

**Files:**
- Modify: `crates/knixl-modules/src/template.rs` (add `origin` to `SchemaEntry`/`SubField`,
  factor `render_entry`, add `Editable`, `load_editable`, `reconcile`, tests)
- Modify: `crates/knixl/src/tui/author.rs` (mechanical: set `origin: None` in `draft()`'s
  `SchemaEntry`/`SubField` literals so the workspace stays green; no behaviour change)

**Interfaces:**
- Produces (all `pub`, in `knixl_modules::template`):
  ```rust
  // added field on the slice-1 structs (render_manifest ignores it):
  //   SchemaEntry { .., pub origin: Option<usize> }
  //   SubField    { .., pub origin: Option<usize> }

  pub struct Editable {
      pub doc: kdl::KdlDocument,   // full parsed original, for reconcile
      pub name: String,
      pub node: String,
      pub summary: String,
      pub entries: Vec<SchemaEntry>, // origins set from the source node indices
      pub emit: String,              // emit block inner text
  }
  pub fn load_editable(text: &str) -> Result<Editable, String>;
  pub fn reconcile(original: &kdl::KdlDocument, draft: &ModuleDraft) -> Result<String, String>;
  ```

- [ ] **Step 1: Add `origin` and keep the workspace compiling**

Add `pub origin: Option<usize>` to `SchemaEntry` and `SubField`. `render_manifest` does not read
it, so its output is unchanged. Update every existing construction site to set `origin: None`:
- the `SchemaEntry`/`SubField` literals in `crates/knixl-modules/src/template.rs` tests,
- the `SchemaEntry`/`SubField` literals in `crates/knixl/src/tui/author.rs` `draft()` (add
  `origin: None,` to both).

Run `cargo build --workspace --tests`. Expected: PASS (pure mechanical addition).

- [ ] **Step 2: Factor `render_entry` (DRY for reconcile's fresh-node path)**

Extract the per-entry rendering already inside `render_manifest` into a helper that returns one
schema node's text including its leading indentation and trailing newline, then have
`render_manifest` call it:

```rust
/// Render a single schema entry as one KDL node, indented for the `schema { }` block. Shared by
/// render_manifest and reconcile (which parses the text into a fresh node).
fn render_entry(e: &SchemaEntry) -> String {
    let esc = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let ty = |t: FieldTy| match t { FieldTy::Str => "string", FieldTy::Bool => "bool", FieldTy::Int => "int" };
    let mut s = String::new();
    let structured = e.kind == EntryKind::Child && !e.subfields.is_empty();
    if structured {
        s.push_str(&format!("        child \"{}\" required=#{} repeated=#{} {{\n", esc(e.name.trim()), e.required, e.repeated));
        for sf in &e.subfields {
            let skw = match sf.kind { SubKind::Arg => "arg", SubKind::Prop => "prop" };
            s.push_str(&format!("            {skw} \"{}\" type=\"{}\" required=#{} doc=\"\"\n", esc(sf.name.trim()), ty(sf.ty), sf.required));
        }
        s.push_str("        }\n");
    } else if e.kind == EntryKind::Child {
        s.push_str(&format!("        child \"{}\" type=\"{}\" required=#{} repeated=#{} doc=\"\"\n", esc(e.name.trim()), ty(e.ty), e.required, e.repeated));
    } else {
        let kw = match e.kind { EntryKind::Arg => "arg", EntryKind::Prop => "prop", EntryKind::Child => "child" };
        s.push_str(&format!("        {kw} \"{}\" type=\"{}\" required=#{} doc=\"\"\n", esc(e.name.trim()), ty(e.ty), e.required));
    }
    s
}
```

Replace the inline entry-rendering loop body in `render_manifest` with `s.push_str(&render_entry(e))`.
Run `cargo test -p knixl-modules render_manifest`. Expected: PASS (output identical).

- [ ] **Step 3: Failing tests for `load_editable` and `reconcile`**

Add to the `tests` module. They use the real `web-service` manifest as the fidelity fixture; it
has `doc=` strings, a `migrations` block, and a repeated structured `location` child.

```rust
    fn web_service_manifest() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../modules/web-service/knixl-module.kdl"
        )).expect("read web-service manifest")
    }

    #[test]
    fn load_editable_reads_header_entries_and_emit() {
        let ed = load_editable(&web_service_manifest()).expect("loads");
        assert_eq!(ed.node, "web-service");
        assert!(ed.entries.iter().any(|e| e.name == "host"), "has the host entry");
        assert!(ed.entries.iter().any(|e| e.kind == EntryKind::Child && e.name == "location" && e.repeated),
            "location is a repeated child");
        assert!(ed.entries.iter().all(|e| e.origin.is_some()), "every loaded entry has an origin");
        assert!(!ed.emit.trim().is_empty(), "emit text captured");
    }

    #[test]
    fn reconcile_with_no_edits_preserves_version_migrations_and_docs() {
        let src = web_service_manifest();
        let ed = load_editable(&src).expect("loads");
        let draft = ModuleDraft {
            name: ed.name.clone(), node: ed.node.clone(), summary: ed.summary.clone(),
            entries: ed.entries.clone(), emit: ed.emit.clone(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        validate_manifest(&out).expect("reconciled manifest is valid");
        // Content the editor does not model must survive.
        assert!(out.contains("migrations"), "migrations block preserved: {out}");
        assert!(out.contains("serverAliases is generated"), "a migration note preserved");
        assert!(out.contains("doc=\"Additional server name.\""), "a doc string preserved");
        // version is not 0.1.0 (render_manifest's default) : the original version survived.
        let orig_ver = src.split("version=\"").nth(1).unwrap().split('"').next().unwrap();
        assert!(out.contains(&format!("version=\"{orig_ver}\"")), "version {orig_ver} preserved: {out}");
    }

    #[test]
    fn reconcile_toggling_required_updates_only_that_node() {
        let ed = load_editable(&web_service_manifest()).expect("loads");
        let mut entries = ed.entries.clone();
        let host = entries.iter_mut().find(|e| e.name == "host").expect("host entry");
        let was = host.required;
        host.required = !was;
        let draft = ModuleDraft {
            name: ed.name.clone(), node: ed.node.clone(), summary: ed.summary.clone(),
            entries, emit: ed.emit.clone(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        validate_manifest(&out).expect("valid");
        // doc strings still present (node was updated in place, not rebuilt fresh).
        assert!(out.contains("doc=\"Additional server name.\""), "unrelated doc preserved: {out}");
    }

    #[test]
    fn reconcile_adds_a_new_entry_and_drops_a_removed_one() {
        let ed = load_editable(&web_service_manifest()).expect("loads");
        let mut entries = ed.entries.clone();
        // add a fresh arg (origin None) and remove whichever entry is first.
        entries.push(SchemaEntry {
            kind: EntryKind::Arg, name: "extra".into(), ty: FieldTy::Str,
            required: false, repeated: false, subfields: vec![], origin: None,
        });
        let removed_name = entries.remove(0).name;
        let draft = ModuleDraft {
            name: ed.name.clone(), node: ed.node.clone(), summary: ed.summary.clone(),
            entries, emit: ed.emit.clone(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        assert!(out.contains("arg \"extra\""), "new entry rendered: {out}");
        assert!(!out.contains(&format!("\"{removed_name}\"")) || removed_name.is_empty(),
            "removed entry gone: {out}");
        validate_manifest(&out).expect("valid");
    }

    #[test]
    fn reconcile_replaces_the_emit_block() {
        let ed = load_editable(&web_service_manifest()).expect("loads");
        let draft = ModuleDraft {
            name: ed.name.clone(), node: ed.node.clone(), summary: ed.summary.clone(),
            entries: ed.entries.clone(),
            emit: "set \"services.nginx.enable\" #true".into(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        assert!(out.contains("services.nginx.enable"), "new emit present: {out}");
        assert!(out.contains("migrations"), "migrations still preserved: {out}");
        validate_manifest(&out).expect("valid");
    }
```

Run: `KNIXL_FORMATTER` is not needed (no nix). `cargo test -p knixl-modules load_editable reconcile`.
Expected: FAIL to compile (functions absent).

- [ ] **Step 4: Implement `load_editable`**

Parse the document, read the module node's `name=`, the `summary`/`claims-node` child first
args, the `schema` children into `SchemaEntry`s (each with `origin: Some(index)`; sub-fields with
`origin: Some(sub_index)`), and the `emit` block inner text via `children().map(|d|
d.to_string())`. A `type=` value maps to `FieldTy` (`"bool"`->Bool, `"int"`->Int, else Str).
Return `Editable { doc, name, node, summary, entries, emit }`. Map any structural problem
(missing `module`, empty body) to an `Err(String)`.

- [ ] **Step 5: Implement `reconcile`**

```rust
pub fn reconcile(original: &kdl::KdlDocument, draft: &ModuleDraft) -> Result<String, String> {
    let mut doc = original.clone();
    let module = doc.nodes_mut().iter_mut().find(|n| n.name().value() == "module")
        .ok_or_else(|| "missing `module` node".to_string())?;
    if let Some(v) = module.get_mut("name") { *v = draft.name.trim().into(); }
    let node_name = if draft.node.trim().is_empty() { draft.name.trim() } else { draft.node.trim() };
    let body = module.children_mut().as_mut().ok_or_else(|| "empty module".to_string())?;
    set_child_first_arg(body, "summary", draft.summary.trim());
    set_child_first_arg(body, "claims-node", node_name);

    if let Some(schema) = body.nodes_mut().iter_mut().find(|n| n.name().value() == "schema") {
        let orig: Vec<kdl::KdlNode> = schema.children().map(|d| d.nodes().to_vec()).unwrap_or_default();
        let mut nodes = Vec::with_capacity(draft.entries.len());
        for e in &draft.entries {
            let node = match e.origin {
                Some(i) if i < orig.len() && node_kw(&orig[i]) == entry_kw(e) => {
                    let mut n = orig[i].clone();          // keeps trivia, comments, doc=
                    update_schema_node(&mut n, e);        // mutate only editor-owned parts
                    n
                }
                _ => parse_one_node(&render_entry(e))?,   // fresh node (doc="")
            };
            nodes.push(node);
        }
        let mut child_doc = kdl::KdlDocument::new();
        *child_doc.nodes_mut() = nodes;
        schema.set_children(child_doc);
    }

    if let Some(emit) = body.nodes_mut().iter_mut().find(|n| n.name().value() == "emit") {
        let parsed = format!("{}\n", draft.emit).parse::<kdl::KdlDocument>().map_err(|e| e.to_string())?;
        emit.set_children(parsed);
    }
    Ok(doc.to_string())
}
```

Helpers to add:
- `set_child_first_arg(body, kw, val)`: find the `kw` child node, set its first positional
  entry's value; if absent, do nothing (these nodes exist in any real manifest).
- `node_kw(node) -> &str` / `entry_kw(entry) -> &str`: the `arg`/`prop`/`child` keyword.
- `parse_one_node(text) -> Result<KdlNode, String>`: parse `text` as a `KdlDocument` and take
  its first node (`nodes_mut().remove(0)`); error if empty. Because `render_entry` includes the
  8-space indent, the parsed node carries matching leading trivia.
- `update_schema_node(node, e)`: mutate in place, preserving `doc=` and trivia:
  - set the first positional arg value to `e.name.trim()`;
  - set or insert `required=#<e.required>`;
  - for `arg`/`prop`/scalar `child`: set or insert `type="<ty>"`; for a structured child (has
    subfields): remove any `type=` (a Node-typed child omits it);
  - for `child`: set or insert `repeated=#<e.repeated>`; for `arg`/`prop`: remove any `repeated=`;
  - for a structured child: recurse the same reconcile over its sub-children against
    `e.subfields` (build a fresh inner doc: matched sub-nodes by `origin` updated in place, fresh
    sub-nodes from a `render_subfield` helper, others dropped), then `node.set_children(...)`.
  Use `node.get_mut(key)` to set an existing prop value and `node.insert(key, value)` /
  `KdlEntry::new_prop` to add a missing one; `node.remove(key)` to drop one.

Note on trivia: matched nodes keep their original indentation and comments; fresh nodes get the
indent from `render_entry`'s text. Exact whitespace is best-effort. The gate is that
`validate_manifest` accepts the output and the preserved content (version, migrations, docs,
comments) survives, which the tests assert. Do NOT call `autoformat`/`autoformat_no_comments`
(they strip comments and formatting).

- [ ] **Step 6: Run to green**

Run: `cargo test -p knixl-modules` and `cargo build --workspace --tests`
Expected: PASS.

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p knixl-modules --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: No commit.**

---

### Task 2: Mode-aware Author screen (New vs Edit)

**Files:**
- Modify: `crates/knixl/src/tui/author.rs` (add `Mode`, `origin` on `EntryState`/`SubFieldState`,
  `edit()` constructor, mode-aware current-text and primary control, `Nav::SaveModule` emit)
- Modify: `crates/knixl/src/tui/mod.rs` (add `Nav::SaveModule { path, text }`)
- Test: `crates/knixl/src/tui/author.rs`

**Interfaces:**
- Consumes: Task 1 `load_editable`, `reconcile`, `Editable`, and the `origin` fields.
- Produces: `AuthorModel::edit(size, path, text) -> Result<AuthorModel, String>`; in Edit mode
  the screen emits `Nav::SaveModule { path: PathBuf, text: String }`; New mode still emits
  `Nav::Scaffold`.

- [ ] **Step 1: Add the save path end to end (Nav, Outcome, apply arm, commit)**

To keep the workspace green, the `Outcome` variant and its `main.rs` handler must land together
(adding `Outcome::SaveModule` alone makes `main.rs`'s `match outcome` non-exhaustive). Add, all
in this step:
- `tui/mod.rs`: `Nav::SaveModule { path: std::path::PathBuf, text: String }`;
  `Outcome::SaveModule { path: std::path::PathBuf, text: String }`; an `App::apply` arm for
  `Nav::SaveModule` that sets `self.outcome = Outcome::SaveModule { path, text }` and returns
  `command::quit()` (mirroring the `Nav::Scaffold` arm).
- `main.rs`: `commit_save_module` and the `Outcome::SaveModule` handler:
  ```rust
  /// Overwrite an existing module manifest with edited text (Edit mode). Unlike commit_scaffold
  /// this expects the file to exist and replaces it.
  fn commit_save_module(path: &std::path::Path, text: &str) -> Code {
      if let Err(e) = std::fs::write(path, text) {
          eprintln!("knixl: {}: {e}", path.display());
          return Code::Internal;
      }
      println!("updated {}", path.display());
      Code::Clean
  }
  ```
  Add the `tui::Outcome::SaveModule { path, text } => commit_save_module(&path, &text),` arm next
  to `Outcome::Scaffold`. Add a unit test that `commit_save_module` overwrites an existing temp
  file with the given text.

The Browse entry point that produces `Nav::EditModule` is Task 3; this task drives Edit mode
through `AuthorModel::edit` directly in its tests.

- [ ] **Step 2: Failing tests**

```rust
    #[test]
    fn edit_loads_a_manifest_into_the_editor() {
        let src = "module name=\"demo\" version=\"2.0.0\" {\n    summary \"s\"\n    claims-node \"demo\"\n    schema {\n        arg \"host\" type=\"string\" required=#true doc=\"the host\"\n    }\n    emit {\n        set \"services.demo.enable\" #true\n    }\n}\n";
        let m = AuthorModel::edit((100, 40), std::path::PathBuf::from("modules/demo/knixl-module.kdl"), src).expect("edit loads");
        assert_eq!(m.name.value(), "demo");
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].name.value(), "host");
        assert!(m.emit.value().contains("services.demo.enable"));
    }

    #[test]
    fn edit_mode_saves_via_savemodule_with_a_reconciled_manifest() {
        let src = "module name=\"demo\" version=\"2.0.0\" {\n    summary \"s\"\n    claims-node \"demo\"\n    schema {\n        arg \"host\" type=\"string\" required=#true doc=\"the host\"\n    }\n    emit {\n        set \"services.demo.enable\" #true\n    }\n}\n";
        let mut m = AuthorModel::edit((100, 40), std::path::PathBuf::from("modules/demo/knixl-module.kdl"), src).expect("edit");
        m.recompute_status();
        let idx = m.focus_list().iter().position(|f| *f == Focus::Create).unwrap();
        m.focus = idx;
        let step = m.update(key(KeyCode::Enter), (100, 40));
        match step.nav {
            Nav::SaveModule { path, text } => {
                assert!(path.ends_with("knixl-module.kdl"));
                assert!(text.contains("version=\"2.0.0\""), "version preserved: {text}");
                assert!(text.contains("doc=\"the host\""), "doc preserved: {text}");
                knixl_modules::template::validate_manifest(&text).expect("valid");
            }
            other => panic!("expected SaveModule, got something else"),
        }
    }

    #[test]
    fn new_mode_still_scaffolds() {
        let mut m = AuthorModel::enter((100, 40));
        m.name.set_value("fresh");
        m.entries[0].name.set_value("host");
        m.recompute_status();
        let idx = m.focus_list().iter().position(|f| *f == Focus::Create).unwrap();
        m.focus = idx;
        match m.update(key(KeyCode::Enter), (100, 40)).nav {
            Nav::Scaffold { .. } => {}
            _ => panic!("New mode must still scaffold"),
        }
    }
```

Run `cargo test -p knixl-cli author` ... note the crate is `knixl`: `cargo test -p knixl author`.
Expected: FAIL to compile (`edit`, `Mode`, `Nav::SaveModule` absent).

- [ ] **Step 3: Add `Mode` and `origin`, build `edit()`**

- Add `enum Mode { New, Edit { path: std::path::PathBuf, original: kdl::KdlDocument } }` and a
  `mode: Mode` field on `AuthorModel` (default `New` in `enter`).
- Add `origin: Option<usize>` to `EntryState` and `SubFieldState`; set `None` in `enter`'s seed
  and in `add_entry`/`add_subfield`.
- `AuthorModel::edit(size, path, text)`: call `load_editable(text)`; build the header textinputs
  from `ed.name/node/summary`; build `entries: Vec<EntryState>` from `ed.entries` (each field a
  `textinput` seeded with the value; carry `origin`; sub-fields likewise); seed the emit textarea
  with `ed.emit`; set `mode: Mode::Edit { path, original: ed.doc }`; `focus: 0`; then
  `recompute_status()`. Map a load error to `Err(String)` (Task 3's caller shows it).

- [ ] **Step 4: Mode-aware current text and `draft()` origin**

- `draft()` now reads `e.origin` / `s.origin` (instead of the `origin: None` added in Task 1
  Step 1) so Edit-mode drafts carry origins.
- Introduce `fn current_text(&self) -> Result<String, String>`:
  - `Mode::New`: `Ok(render_manifest(&self.draft()))`.
  - `Mode::Edit { original, .. }`: `reconcile(original, &self.draft())`.
  `recompute_status` uses `current_text` (then `validate_manifest`).

- [ ] **Step 5: Mode-aware primary control**

- The primary control label is "create" in New mode and "save" in Edit mode (view text).
- On Enter at `Focus::Create` when `can_create()` (rename the gate to read "name non-empty and
  status ok"; keep the method name to minimise churn):
  - `Mode::New`: emit `Nav::Scaffold { name, manifest: current_text()? }` (unchanged behaviour;
    `current_text` in New mode is `render_manifest`).
  - `Mode::Edit { path, .. }`: emit `Nav::SaveModule { path: path.clone(), text: current_text()? }`.

- [ ] **Step 6: Run to green**

Run: `cargo test -p knixl author` then `cargo test -p knixl` and `cargo build --workspace --tests`
Expected: PASS.

- [ ] **Step 7: Clippy**

Run: `cargo clippy -p knixl --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 8: No commit.**

---

### Task 3: Browse -> Edit wiring and the save path

**Files:**
- Modify: `crates/knixl/src/tui/mod.rs` (`BrowseModule.manifest`, `Nav::EditModule`, the
  `App::apply` arm for `EditModule`)
- Modify: `crates/knixl/src/tui/browse.rs` (an `edit` action for declarative modules ->
  `Nav::EditModule`)
- Modify: `crates/knixl/src/main.rs` (`browse_modules` fills `manifest`)
- Test: `crates/knixl/src/tui/browse.rs`

**Interfaces:**
- Consumes: Task 2 `AuthorModel::edit`.
- Produces: `BrowseModule { .., pub manifest: Option<std::path::PathBuf> }`;
  `Nav::EditModule { manifest: std::path::PathBuf }`.

- [ ] **Step 1: `BrowseModule.manifest` and `browse_modules`**

Add `pub manifest: Option<std::path::PathBuf>` to `BrowseModule` (`tui/mod.rs`). In
`main.rs::browse_modules`, set it to `Some(modules/<dir>/knixl-module.kdl)` for declarative
modules and `None` for built-ins. `build_registry` discovers by directory, but `browse_modules`
uses the registry which does not expose the path; extend it: after building the registry, for
each declarative module resolve its manifest path by scanning `root/modules/*/knixl-module.kdl`
and matching the module's `node`/dir, OR (simpler and robust) change the enumerator to walk
`root/modules/*/knixl-module.kdl` directly for declarative entries and pair by claimed node.
Prefer walking the directory so the path is authoritative. Built-ins get `None`.

- [ ] **Step 2: Failing test (Browse emits EditModule only for declarative)**

```rust
    #[test]
    fn edit_action_only_for_declarative_modules() {
        // Build a BrowseModel with one built-in (manifest None) and one declarative (Some path).
        // Drive the edit key on each selection; assert Nav::EditModule only for the declarative.
        // (Follow the existing browse tests' construction pattern.)
    }
```

Fill it in using `browse.rs`'s existing test helpers (construct the model with two
`BrowseModule`s, select each, press the edit key). Expected: FAIL (no edit action yet).

- [ ] **Step 3: Browse edit action**

Add `Nav::EditModule { manifest: std::path::PathBuf }` to `Nav` (`tui/mod.rs`). In `browse.rs`,
on the module-list view, bind an `edit` key (e.g. `KeyCode::Char('e')`): if
`selected_module().manifest` is `Some(path)`, return `Nav::EditModule { manifest: path }`; if
`None` (built-in), stay (optionally a brief status). Add `("e", "edit")` to the screen's footer.

- [ ] **Step 4: `App::apply` for EditModule**

In `tui/mod.rs`, add the `Nav::EditModule { manifest }` arm: read the file
(`std::fs::read_to_string`); on success call `AuthorModel::edit(self.size, manifest, &text)` and,
if it returns `Ok`, set `self.screen = Screen::Author(..)`; on a read error or an `edit` error,
stay on Browse (a no-op or a brief status is acceptable; do not open a broken editor). The
`Nav::SaveModule` arm already exists from Task 2.

- [ ] **Step 5: Full check**

Run: `cargo test -p knixl -p knixl-modules`, `cargo build --workspace --tests`, and
`cargo clippy --all-targets -- -D warnings`.
Also run the golden pipeline test to confirm nothing else moved:
`KNIXL_FORMATTER=/home/wes/.nix-profile/bin/nixfmt cargo test -p knixl-pipeline --test golden` (unchanged).
Expected: all PASS.

- [ ] **Step 7: No commit.**

---

## Self-Review

- Spec coverage: Task 1 is the fidelity core (`load_editable` + `reconcile`, preserving version,
  migrations, doc strings, comments; New mode unchanged because `render_manifest` ignores
  `origin`), tested against the real `web-service` manifest. Task 2 makes the Author screen
  mode-aware (load via `edit`, save via `Nav::SaveModule` using `reconcile`), New mode still
  scaffolds. Task 3 wires Browse -> Edit and the overwriting save path. Out-of-scope items
  (editing version/migrations/doc in the UI, editing built-ins) are excluded.
- Placeholders: Task 1 carries the reconcile algorithm and tests in full; Task 3's Browse test is
  a stub-with-instructions because it must follow `browse.rs`'s existing test construction, which
  the implementer reads in situ; every other step shows concrete code.
- Type consistency: `origin: Option<usize>` is added on `SchemaEntry`/`SubField` in Task 1 and
  read by `draft()` in Task 2; `reconcile(&KdlDocument, &ModuleDraft)` signature is stable across
  tasks; `Nav::SaveModule`/`Outcome::SaveModule { path, text }` and `Nav::EditModule { manifest }`
  are introduced and consumed with the same shapes; `BrowseModule.manifest: Option<PathBuf>` is
  set in `browse_modules` and read in `browse.rs`.
- Ordering: `render_manifest` stays behaviour-identical (New mode) throughout; the workspace is
  kept green at each task boundary (Task 1 updates all `origin` construction sites; Task 2 adds
  both `Nav` and `Outcome` variants so `apply` is exhaustive).
