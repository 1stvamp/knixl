# In-TUI module editing design (#11, slice 2)

Date: 2026-07-20
Status: approved, ready for implementation plan
Issue: #11 (slice 2 of 2; slice 1 shipped new-module authoring in PR #41)

Let the TUI edit an existing declarative module: load its `knixl-module.kdl`, edit the schema
and emit through the slice-1 structured editor, and write it back without losing any content the
editor does not model (version, migrations, per-field `doc=` strings, comments, formatting).
Fidelity comes from mutating the parsed KDL document in place, not re-rendering from a model.

## Grounding (current state)

- The CLI crate was renamed `knixl-cli` -> `knixl` during open-sourcing. The Author screen is
  `crates/knixl/src/tui/author.rs`; the TUI orchestration is `crates/knixl/src/tui/mod.rs`;
  module discovery and the commit paths are `crates/knixl/src/main.rs`.
- Slice 1 gave the Author screen a structured schema editor (`entries: Vec<EntryState>` with
  nested sub-fields), an emit `textarea`, a dynamic focus list, and live validation via
  `knixl_modules::template::{render_manifest, validate_manifest}` (`validate_manifest` runs the
  real dry type-pass through `DeclarativeModule::from_kdl`). It writes a new manifest through
  `Nav::Scaffold` -> `Outcome::Scaffold` -> `commit_scaffold`, which refuses to overwrite.
- Declarative modules live at `modules/<dir>/knixl-module.kdl`. `browse_modules(root)`
  (`main.rs`) enumerates the registry into `BrowseModule { node, kind, doc, skeleton }` but does
  not retain the manifest path or source text. Built-in modules have no manifest.
- The Browse screen (`crates/knixl/src/tui/browse.rs`) lists modules and can scaffold a node
  into a host (`Nav::Insert`).
- `kdl` 6.7.1 is the parser. Its mutation API supports the reconcile: `KdlDocument::nodes_mut`,
  `KdlNode::{entries_mut, children_mut, set_children, name}`, `KdlEntry` set/insert/remove, and
  `to_string()` preserves each node's trivia (leading comments, whitespace) for nodes we do not
  touch.
- `DeclarativeModule::from_kdl` and the `Registry` are not `Send`; the TUI never holds them (the
  CLI precomputes `BrowseModule`s). Editing must follow the same rule: parse and reconcile off a
  plain `kdl::KdlDocument` that the model can own on the main thread.

Editing authoritative KDL in place is not a Nix-to-KDL round-trip and does not touch ADR 0001.

## Design

### Entry point: Browse -> Edit

`BrowseModule` gains `manifest: Option<PathBuf>` (Some for declarative modules, None for
built-ins). `browse_modules` fills it from the discovered `modules/<dir>/knixl-module.kdl` path.
The Browse screen adds an `edit` action (a key, e.g. `e`, shown in the footer) available only
when the selected module is declarative; it emits `Nav::EditModule { manifest: PathBuf }`. The
`App` reducer (`tui/mod.rs`) reads the file (synchronously, as `commit_insert` already reads
host files) and enters the Author screen in Edit mode with the path and text; an unreadable or
unparseable file falls back to a status message rather than opening a broken editor.

### Author screen: mode-aware

`AuthorModel` gains a mode:

```rust
enum Mode {
    New,
    Edit { path: PathBuf, original: kdl::KdlDocument },
}
```

The structured editing surface (header inputs, `entries`, emit textarea, focus list, cycles,
toggles, add/delete) is unchanged from slice 1. Two additions for Edit mode:

- Each `EntryState` (and its sub-field state) carries `origin: Option<usize>`: the index of the
  node it was loaded from (into the schema block's children, and into a structured child's own
  children for sub-fields). New entries added in the editor have `origin: None`.
- `AuthorModel::edit(size, path, text) -> Result<AuthorModel, String>` parses `text`, populates
  the header inputs (module `name=`, the `claims-node` and `summary` child args), builds
  `entries` from the `schema` block's `arg`/`prop`/`child` children (kind, name, type, required,
  repeated, sub-fields, each with its `origin`), fills the emit textarea from the `emit` block's
  reserialised children text, and stores `Mode::Edit { path, original }` holding the full parsed
  document.

### Fidelity: reconcile-on-save (in-place mutation)

The current manifest text is produced by a mode switch:

- `Mode::New`: `render_manifest(&draft)` (slice 1, unchanged).
- `Mode::Edit`: `reconcile(original, &draft)` in `knixl-modules`, which clones
  `original` and mutates the clone (reading each entry's `origin`):
  - Module node: set the `name=` entry from the name input.
  - `summary` child: set its first arg from the summary input. `claims-node` child: set its
    first arg from the node input.
  - `schema` block children: rebuild the children vector in `entries` order. For an entry with
    `origin: Some(i)`, take original child `i` (keeping all its trivia, comments, and `doc=`
    string) and update only the editor-owned parts: the node name (`arg`/`prop`/`child` on a
    kind change), the name arg, and the `type=`/`required=`/`repeated=` entries; for a
    structured child, recurse the same reconcile over its sub-children against the entry's
    sub-fields. For an entry with `origin: None`, build a fresh node (as `render_manifest`
    does, `doc=""`). Original children with no surviving entry are dropped.
  - `emit` block: replace its children with the parse of the emit textarea text (the author
    owns the emit formatting, so replacing it is expected and lossless from their view).
  - Everything else (module `version`, the `migrations` block, existing `doc=` strings on kept
    fields, comments, and any node the editor does not model) is left untouched.
  Returns the serialised document text.

Live validation is unchanged in shape: rebuild the current text (render or reconcile), run
`validate_manifest`, store the result for the status line and the save gate. Reconcile plus a
parse per keystroke is cheap for a small manifest.

### Save

- `Mode::New`: unchanged (`Nav::Scaffold` -> `commit_scaffold`, refuses overwrite).
- `Mode::Edit`: the primary control reads "save" (not "create"), gated on a valid reconcile.
  On save it emits `Nav::SaveModule { path, text }` -> `Outcome::SaveModule { path, text }` ->
  a new `commit_save_module(path, text)` in `main.rs` that overwrites the existing file (the
  file is known to exist; unlike `commit_scaffold` it does not refuse). It prints a short
  "updated <path>" line.

### Library surface (knixl-modules)

- `SchemaEntry` and `SubField` (slice 1) gain `origin: Option<usize>` (the source node index;
  `None` for entries added in the editor and for every entry in New mode). `render_manifest`
  ignores it, so New mode is byte-for-byte unchanged; only `reconcile` reads it. This keeps the
  origin travelling with its entry rather than in a parallel structure that could drift.
- `load_editable(text: &str) -> Result<Editable, String>` returning the parsed document plus the
  structured view the Author screen needs (header strings; the `Vec<SchemaEntry>` with origins
  set; the emit text). Pure and unit-tested.
- `reconcile(original: &kdl::KdlDocument, draft: &ModuleDraft) -> Result<String, String>`
  producing the mutated, serialised manifest, reading each entry's `origin` to find its source
  node. Pure and unit-tested.
- `validate_manifest` (slice 1) is reused unchanged for both modes.

## Testing

Library (`knixl-modules`), the reconcile is where the fidelity risk lives:
- Round-trip identity: `load_editable` then `reconcile` with no edits reproduces a manifest that
  `validate_manifest` accepts and preserves the `version`, the `migrations` block, `doc=`
  strings, and a comment line (assert the comment text and migrations survive verbatim).
- Field edit: toggling a field's `required` updates only that node's `required=` and leaves its
  `doc=` and the rest of the document intact.
- Add/remove/reorder entries: a new entry appears as a fresh node; a removed entry's node is
  gone; reordering reorders the schema children; untouched nodes keep their trivia.
- Structured child: editing a sub-field updates only that sub-node; the child's other sub-fields
  and `doc=` survive. A repeated structured child keeps `repeated=`.
- Emit replace: editing the emit text replaces the emit block and the result validates; the rest
  of the document is unchanged.
- Use `web-service`'s real manifest (it has `doc=` strings, a `migrations` block, and a repeated
  structured `location` child) as a fixture for the identity and preservation tests.

TUI (`crates/knixl/src/tui`), reducer tests (no terminal):
- `AuthorModel::edit` loads a manifest into the header inputs, entries (with origins), and emit
  textarea.
- In Edit mode the primary control is "save" and emits `Nav::SaveModule { path, text }` with a
  reconciled text that `validate_manifest` accepts; New mode still emits `Nav::Scaffold`.
- Browse emits `Nav::EditModule` only for a declarative module (its `manifest` is `Some`) and
  not for a built-in.

## Out of scope (a later slice, or the raw file)

- Editing the module `version`, the `migrations` block, or per-field `doc=` strings in the UI
  (all preserved, none editable here).
- Editing built-in (Rust) modules (they have no manifest).
- Preserving comments that sit inside a schema node that the user then deletes (the comment is
  attached to the dropped node and goes with it).
- Any change to generation, the lock, or the oracle.
