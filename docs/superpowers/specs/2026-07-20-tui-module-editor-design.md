# In-TUI module authoring design (#11, slice 1)

Date: 2026-07-20
Status: approved, ready for implementation plan
Issue: #11 (this is slice 1 of a decomposition; slice 2 is editing existing modules)

Replace the fixed one-field Author screen with a full in-TUI workflow to author a new
declarative module: a structured schema editor (args, props, children, including structured
children with nested arg/prop sub-fields) and a free-text emit editor, with live validation
against the real dry type-pass, writing `modules/<name>/knixl-module.kdl`.

## Grounding (current state)

- The Author screen (`crates/knixl-cli/src/tui/author.rs`) is a fixed form: name, node,
  summary, and exactly one schema field (name, type, required). It renders a starter manifest
  via `knixl_modules::template::scaffold_manifest` and writes it through `Nav::Scaffold` ->
  `Outcome::Scaffold` -> `commit_scaffold` (main.rs:1167), which writes
  `modules/<name>/knixl-module.kdl` and refuses to overwrite an existing one.
- Screens are pure `update(msg, size) -> Step` / `view(size) -> String` reducers over a
  `bubbletea-rs` state machine (`tui/mod.rs`); the decision logic is unit-tested, the runtime
  glue is not. Text fields use `bubbletea_widgets::textinput`.
- Validation ground truth already exists: `DeclarativeModule::from_kdl(&doc, path)` runs the
  full parse plus the dry type-pass (`template.rs`) and returns a located error.
- The emit grammar is five forms after #16: `set`, `when-flag`, `when-config`, `for-each`,
  plus the `collect` and `indent-str` value annotations.
- `bubbletea-widgets` 0.1.12 ships a `textarea` widget (multi-line: `value`, `set_value`,
  `set_width`/`set_height`, `update`, `view`, `focused`, focus/blur via its focus trait; its
  `update` ignores input unless focused). It compiles under the crate's current
  `default-features = false` (only clipboard support is feature-gated), so no Cargo change.

This writes authoritative KDL from a structured editor. It is not a Nix-to-KDL round-trip and
does not touch ADR 0001.

## Design

### Library layer (knixl-modules) : pure, unit-tested

A pure manifest builder and validator, reusable outside the TUI and testable without it.

- `ModuleDraft { name: String, node: String, summary: String, entries: Vec<SchemaEntry>,
  emit: String }`.
- `SchemaEntry { kind: EntryKind, name: String, ty: FieldTy, required: bool, repeated: bool,
  subfields: Vec<SubField> }` where `EntryKind = Arg | Prop | Child` and
  `FieldTy = Str | Bool | Int` (rendered as `type="string"`, `"bool"`, `"int"` to match
  `ty_from`). `repeated` and `subfields` are only meaningful for `Child`; a
  `Child` with a non-empty `subfields` renders as a structured `Node` child (matching
  `parse_child`, which treats a child-with-block as `ValueTy::Node` and ignores its `type`).
- `SubField { kind: SubKind, name: String, ty: FieldTy, required: bool }` where
  `SubKind = Arg | Prop`.
- `render_manifest(&ModuleDraft) -> String`: deterministic KDL text, byte-stable for a given
  draft. Shape:
  ```
  module name="<name>" version="0.1.0" {
      summary "<summary>"
      claims-node "<node-or-name>"

      schema {
          arg  "<name>" type="<ty>" required=#<bool> doc=""
          prop "<name>" type="<ty>" required=#<bool> doc=""
          child "<name>" type="<ty>" required=#<bool> repeated=#<bool> doc=""
          child "<name>" required=#<bool> repeated=#<bool> {   # structured child
              arg  "<sub>" type="<ty>" required=#<bool> doc=""
              prop "<sub>" type="<ty>" required=#<bool> doc=""
          }
      }

      emit {
          <emit text, spliced verbatim, indented one level>
      }
  }
  ```
  Node defaults to name when blank. Version is fixed at `0.1.0` for a new module. Strings are
  escaped (backslash and double-quote, as `scaffold_manifest` does today). Entry order is the
  `entries` vector order (author-controlled, stable). A scalar child (no subfields) renders the
  single-line `child` form with `type=`/`repeated=`; a structured child renders the block form
  carrying `required=`/`repeated=` on the line (a child-with-block is `Node`-typed, so only
  `type=` is omitted; `parse_child` reads `repeated=` on the block form, which the flagship
  `web-service` `location` child relies on for its `for-each`).
- `validate_manifest(text: &str) -> Result<(), String>`: parse `text` as `KdlDocument` (map a
  parse error to its display string), then `DeclarativeModule::from_kdl(&doc,
  Path::new("draft"))`, mapping `Err` to its display string; `Ok(())` on success. This is the
  single validation entry point the TUI calls; it reuses the real dry type-pass, so the editor
  cannot drift from what the loader accepts.
- `scaffold_manifest` and `ModuleScaffold` are superseded by `render_manifest` and removed;
  their test moves to `render_manifest` coverage.

### TUI layer (tui/author.rs) : reducer + view

Rework `AuthorModel` into a sectioned authoring screen. All decision logic stays pure and
unit-tested; only the textarea/textinput editing and the async-free `Cmd` plumbing are glue.

Sections, top to bottom:
1. Header text inputs: name, node (placeholder "defaults to name"), summary.
2. Schema editor: the `entries` list with per-entry cells and, for structured children, nested
   sub-field rows.
3. Emit editor: a `textarea` holding the emit block text.
4. Status line: the live validation result.
5. Create and Cancel controls.

Focus model: extend the existing focus-enum/`FOCUS_ORDER` pattern to a dynamically computed
focus list, because the number of focus points varies with the entries and sub-fields. The
list is rebuilt from the current draft each time focus moves:

```
[ Name, Node, Summary,
  for each entry:
    EntryKind(i), EntryName(i), EntryType(i), EntryRequired(i),
    EntryRepeated(i)         # only when kind == Child
    for each subfield j of a structured child i:
      SubKind(i,j), SubName(i,j), SubType(i,j), SubRequired(i,j)
    AddSubfield(i)           # only when kind == Child
    DeleteEntry(i),
  AddEntry,
  Emit,
  Create, Cancel ]
```

- Tab / Down moves to the next focus point, BackTab / Up to the previous (wrapping), matching
  today's screen. When the `Emit` textarea is focused, arrows and Enter edit the text (the
  textarea consumes them); Tab leaves it (the textarea does not use Tab), so focus can always
  escape the editor.
- On a cell:
  - Kind cells (EntryKind, SubKind): Left/Right (and Space) cycle the kind.
  - Type cells: Left/Right (and Space) cycle `Str | Bool | Int`.
  - Required / Repeated cells: Space / Enter toggle.
  - Name cells: a `textinput` bound to that entry/sub-field, always editable while focused.
  - Add/Delete/AddSubfield cells: Enter performs the mutation (append an entry with a default
    kind, append a sub-field to the child, remove the entry/sub-field). After a delete, focus
    clamps to a valid point.
- No modal letter-commands: letters go to the focused text field, so there is no ambiguity
  between typing and commands, keeping the file's current input model.

Live validation: whenever an update mutates the draft (a keystroke in a text field, a cycle, a
toggle, an add/delete, or an emit edit), rebuild the `ModuleDraft`, call `render_manifest` then
`validate_manifest`, and store the `Result<(), String>` for the status line. This is pure and
synchronous (no nix), and a small KDL doc parses cheaply enough per keystroke for a TUI.

Create gating: Create is enabled only when the name is non-empty and `validate_manifest` on the
rendered draft is `Ok`. On Enter at Create, emit `Nav::Scaffold { name, manifest }` with the
rendered manifest (the existing path). Cancel / Esc / Ctrl-c emit `Nav::Back`.

Starter content: a fresh screen seeds one starter entry (an `arg`) and a starter emit line
(`set "services.<node>.enable" #true`), so a newly authored module does something out of the
box, matching today's scaffold behaviour. The author edits from there.

### Data flow

Editor state (header inputs + entries + sub-fields + emit textarea) -> build `ModuleDraft` ->
`render_manifest` -> `validate_manifest` (status line + create gate). On Create, the rendered
manifest flows through the unchanged `Nav::Scaffold` -> `Outcome::Scaffold` ->
`commit_scaffold` path to `modules/<name>/knixl-module.kdl`.

## Testing

Library (`knixl-modules`):
- `render_manifest` is deterministic (same draft -> byte-identical output) and produces the
  expected shape for: a flat draft (arg + prop + scalar child + repeated child), and a draft
  with a structured child carrying nested arg/prop sub-fields. Node defaults to name when blank.
- `validate_manifest` returns `Ok` for a known-good rendered draft and `Err` (with a message)
  for a known-bad one (e.g. an emit `set` referencing an undeclared binding, which the dry pass
  rejects), proving it is wired to the real type-pass.

TUI (`tui/author.rs`) reducer tests (no terminal):
- Focus moves across the dynamically computed list and wraps, including across entries and a
  structured child's sub-fields.
- Add-entry appends a row; add-subfield appends a sub-field under a child; delete removes the
  selected entry/sub-field and clamps focus.
- Type cycles `Str|Bool|Int` both ways; kind cycles; required and repeated toggle.
- Create is disabled while invalid (bad draft) and while the name is empty, and enabled once
  the name is set and the draft validates; Create emits `Nav::Scaffold` carrying a manifest
  that `validate_manifest` accepts.
- The tiny-terminal resize hint still renders (as today).

## Out of scope (slice 2 and beyond)

- Loading and editing an existing module manifest (the KDL round-trip / comment-preservation
  problem). Slice 1 only authors new modules.
- A fully structured emit-statement editor (nested statement rows). Slice 1 edits emit as
  validated free text.
- Editing `migrations` blocks, `doc=` strings per field (rendered empty for now; the author
  fills them in the file), or the module `version` (fixed at `0.1.0` for a new module).
- Any change to the generation pipeline, the lock, or non-declarative (built-in) modules.
