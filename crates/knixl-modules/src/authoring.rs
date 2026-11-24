//! Authoring a manifest in the TUI: an editable model of a `knixl-module.kdl` (`ModuleDraft`),
//! rendering a new one, loading an existing one, and writing edits back while keeping the
//! parts the editor doesn't model (versions, migrations, comments).

use crate::declarative::DeclarativeModule;
use knixl_kdl::first_arg_str;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldTy {
    Str,
    Bool,
    Int,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EntryKind {
    Arg,
    Prop,
    Child,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SubKind {
    Arg,
    Prop,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SubField {
    pub kind: SubKind,
    pub name: String,
    pub ty: FieldTy,
    pub required: bool,
    /// Index of the source sub-node this field was loaded from, within its parent structured
    /// child's block (set by `load_editable`; `None` for a field added in the editor).
    pub origin: Option<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SchemaEntry {
    pub kind: EntryKind,
    pub name: String,
    pub ty: FieldTy,
    pub required: bool,
    pub repeated: bool,           // Child only
    pub subfields: Vec<SubField>, // Child only; non-empty => structured child
    /// Index of the source node this entry was loaded from, within the `schema { }` block
    /// (set by `load_editable`; `None` for an entry added fresh in the editor). `render_manifest`
    /// does not read this: it always renders fresh nodes, `reconcile` is what uses it to
    /// preserve trivia on unmodified entries.
    pub origin: Option<usize>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModuleDraft {
    pub name: String,
    pub node: String,
    pub summary: String,
    pub entries: Vec<SchemaEntry>,
    pub emit: String,
}

/// Render a single sub-field, indented for a structured child's block. Shared by `render_entry`
/// and `reconcile`'s fresh sub-node path.
fn render_subfield(sf: &SubField) -> String {
    let esc = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let ty = |t: FieldTy| match t {
        FieldTy::Str => "string",
        FieldTy::Bool => "bool",
        FieldTy::Int => "int",
    };
    let skw = match sf.kind {
        SubKind::Arg => "arg",
        SubKind::Prop => "prop",
    };
    format!(
        "            {skw} \"{}\" type=\"{}\" required=#{} doc=\"\"\n",
        esc(sf.name.trim()),
        ty(sf.ty),
        sf.required,
    )
}

/// Render a single schema entry as one KDL node, indented for the `schema { }` block. Shared by
/// render_manifest and reconcile (which parses the text into a fresh node).
fn render_entry(e: &SchemaEntry) -> String {
    let esc = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let ty = |t: FieldTy| match t {
        FieldTy::Str => "string",
        FieldTy::Bool => "bool",
        FieldTy::Int => "int",
    };
    let mut s = String::new();
    let structured = e.kind == EntryKind::Child && !e.subfields.is_empty();
    if structured {
        // A structured child renders the block form. It carries `required=`/`repeated=` on
        // the line (parse_child reads both there); only `type=` is omitted, since a
        // child-with-block is Node-typed.
        s.push_str(&format!(
            "        child \"{}\" required=#{} repeated=#{} {{\n",
            esc(e.name.trim()),
            e.required,
            e.repeated,
        ));
        for sf in &e.subfields {
            s.push_str(&render_subfield(sf));
        }
        s.push_str("        }\n");
    } else if e.kind == EntryKind::Child {
        s.push_str(&format!(
            "        child \"{}\" type=\"{}\" required=#{} repeated=#{} doc=\"\"\n",
            esc(e.name.trim()),
            ty(e.ty),
            e.required,
            e.repeated,
        ));
    } else {
        let kw = match e.kind {
            EntryKind::Arg => "arg",
            EntryKind::Prop => "prop",
            EntryKind::Child => "child",
        };
        s.push_str(&format!(
            "        {kw} \"{}\" type=\"{}\" required=#{} doc=\"\"\n",
            esc(e.name.trim()),
            ty(e.ty),
            e.required,
        ));
    }
    s
}

/// Render a `ModuleDraft` (the TUI Editor screen's working state) into a full module
/// manifest as KDL text. Deterministic: builds a `String` in source order, no hashed
/// collections, so byte-identical output for byte-identical input. Ignores `SchemaEntry::origin`
/// / `SubField::origin`: every entry is rendered fresh, so a `New`-mode draft's output is
/// unaffected by whatever origins happen to be set.
pub fn render_manifest(draft: &ModuleDraft) -> String {
    let esc = |v: &str| v.replace('\\', "\\\\").replace('"', "\\\"");
    let node = if draft.node.trim().is_empty() {
        draft.name.trim()
    } else {
        draft.node.trim()
    };

    let mut s = String::new();
    s.push_str(&format!(
        "module name=\"{}\" version=\"0.1.0\" {{\n",
        esc(draft.name.trim())
    ));
    s.push_str(&format!("    summary \"{}\"\n", esc(draft.summary.trim())));
    s.push_str(&format!("    claims-node \"{}\"\n\n", esc(node)));
    s.push_str("    schema {\n");
    for e in &draft.entries {
        s.push_str(&render_entry(e));
    }
    s.push_str("    }\n\n");
    s.push_str("    emit {\n");
    for line in draft.emit.lines() {
        if line.trim().is_empty() {
            s.push('\n');
        } else {
            s.push_str(&format!("        {line}\n"));
        }
    }
    s.push_str("    }\n}\n");
    s
}

fn field_ty_from(ty: Option<&str>) -> FieldTy {
    match ty {
        Some("bool") => FieldTy::Bool,
        Some("int") => FieldTy::Int,
        _ => FieldTy::Str,
    }
}

/// Read a `schema { }` block's children into `SchemaEntry`s, one per node, each stamped with
/// its `origin` (its index within `schema_node`'s own children) so `reconcile` can find its
/// source node again later. A structured child's sub-fields get their own `origin`, an index
/// into the child's own block.
fn load_schema_entries(schema_node: &kdl::KdlNode) -> Vec<SchemaEntry> {
    let mut entries = Vec::new();
    let Some(body) = schema_node.children() else {
        return entries;
    };
    for (i, n) in body.nodes().iter().enumerate() {
        let kind = match n.name().value() {
            "arg" => EntryKind::Arg,
            "prop" => EntryKind::Prop,
            "child" => EntryKind::Child,
            _ => continue, // not part of the editor's model; leave it out of the draft
        };
        let name = first_arg_str(n).unwrap_or_default();
        let required = prop_bool(n, "required").unwrap_or(false);
        let repeated = prop_bool(n, "repeated").unwrap_or(false);
        let (ty, subfields) = match n.children() {
            // A child with a block is structured: no scalar type=, its own arg/prop subfields.
            Some(sub_body) => {
                let mut subs = Vec::new();
                for (j, s) in sub_body.nodes().iter().enumerate() {
                    let skind = match s.name().value() {
                        "arg" => SubKind::Arg,
                        "prop" => SubKind::Prop,
                        _ => continue,
                    };
                    subs.push(SubField {
                        kind: skind,
                        name: first_arg_str(s).unwrap_or_default(),
                        ty: field_ty_from(prop_str(s, "type").as_deref()),
                        required: prop_bool(s, "required").unwrap_or(false),
                        origin: Some(j),
                    });
                }
                (FieldTy::Str, subs)
            }
            None => (field_ty_from(prop_str(n, "type").as_deref()), Vec::new()),
        };
        entries.push(SchemaEntry {
            kind,
            name,
            ty,
            required,
            repeated,
            subfields,
            origin: Some(i),
        });
    }
    entries
}

/// A module manifest loaded for in-place editing: the parsed document (kept around so
/// `reconcile` can mutate it rather than rebuild it from scratch) plus the editor's flat view
/// of it, matching `ModuleDraft`'s shape.
pub struct Editable {
    pub doc: kdl::KdlDocument,
    pub name: String,
    pub node: String,
    pub summary: String,
    pub entries: Vec<SchemaEntry>,
    pub emit: String,
}

/// Load an existing manifest for editing. Unlike `DeclarativeModule::from_kdl`, this does not
/// dry-type-check the emit template: it only needs the header, the schema, and the emit block's
/// raw text, so the editor can round-trip content it does not model (migrations, doc= strings,
/// comments) back through `reconcile`.
pub fn load_editable(text: &str) -> Result<Editable, String> {
    let doc = text
        .parse::<kdl::KdlDocument>()
        .map_err(|e| e.to_string())?;
    let module = doc
        .nodes()
        .iter()
        .find(|n| n.name().value() == "module")
        .ok_or_else(|| "missing `module` node".to_string())?;
    let name = prop_str(module, "name").unwrap_or_default();
    let body = module
        .children()
        .ok_or_else(|| "empty module".to_string())?;

    let mut node = String::new();
    let mut summary = String::new();
    let mut entries = Vec::new();
    let mut emit = String::new();
    for child in body.nodes() {
        match child.name().value() {
            "summary" => summary = first_arg_str(child).unwrap_or_default(),
            "claims-node" => node = first_arg_str(child).unwrap_or_default(),
            "schema" => entries = load_schema_entries(child),
            "emit" => emit = child.children().map(|d| d.to_string()).unwrap_or_default(),
            _ => {} // migrations, or anything else the editor does not model
        }
    }

    Ok(Editable {
        doc,
        name,
        node,
        summary,
        entries,
        emit,
    })
}

/// The `arg`/`prop`/`child` keyword a KDL node was parsed from.
fn node_kw(node: &kdl::KdlNode) -> &str {
    node.name().value()
}

/// The `arg`/`prop`/`child` keyword a `SchemaEntry` renders as.
fn entry_kw(e: &SchemaEntry) -> &str {
    match e.kind {
        EntryKind::Arg => "arg",
        EntryKind::Prop => "prop",
        EntryKind::Child => "child",
    }
}

/// The `arg`/`prop` keyword a `SubField` renders as.
fn sub_kw(sf: &SubField) -> &str {
    match sf.kind {
        SubKind::Arg => "arg",
        SubKind::Prop => "prop",
    }
}

/// Find `kw`'s child node in `body` and set its first positional argument. A real manifest
/// always has `summary`/`claims-node` children, so a missing one is left alone rather than
/// treated as an error here: any structural problem with the manifest itself already surfaced
/// when `original` was parsed.
fn set_child_first_arg(body: &mut kdl::KdlDocument, kw: &str, val: &str) {
    let Some(node) = body.nodes_mut().iter_mut().find(|n| n.name().value() == kw) else {
        return;
    };
    match node.entries_mut().iter_mut().find(|e| e.name().is_none()) {
        Some(entry) => entry.set_value(val.to_string()),
        None => {
            node.insert(0, val.to_string());
        }
    }
}

/// Parse `text` (expected to be exactly one node, as produced by `render_entry` /
/// `render_subfield`) and take that node.
fn parse_one_node(text: &str) -> Result<kdl::KdlNode, String> {
    let mut doc = text
        .parse::<kdl::KdlDocument>()
        .map_err(|e| e.to_string())?;
    if doc.nodes().is_empty() {
        return Err(format!("expected a node, got nothing from: {text}"));
    }
    Ok(doc.nodes_mut().remove(0))
}

/// Set a boolean prop only when it differs from the KDL default (`false`) or the node already
/// carries it. This keeps an unedited node that omitted the prop clean on save (no `required=#false`
/// / `repeated=#false` churn), matching how `parse_child`/`parse_field` default an absent prop.
fn set_bool_prop_minimal(node: &mut kdl::KdlNode, key: &str, val: bool) {
    if val || node.get(key).is_some() {
        node.insert(key, kdl::KdlValue::Bool(val));
    }
}

/// Set the `type=` prop only when it differs from the KDL default (`"string"`) or the node
/// already carries it, so an unedited string field that omitted `type=` stays clean.
fn set_type_prop_minimal(node: &mut kdl::KdlNode, ty: FieldTy) {
    let s = match ty {
        FieldTy::Str => "string",
        FieldTy::Bool => "bool",
        FieldTy::Int => "int",
    };
    if ty != FieldTy::Str || node.get("type").is_some() {
        node.insert("type", s);
    }
}

/// Mutate a matched sub-node in place so only its editor-owned parts change: name, `type=`,
/// `required=`. Everything else (a `doc=` string, comments, formatting) is whatever the clone
/// already carried. Default-valued props absent on the original stay absent (no churn).
fn update_subfield_node(node: &mut kdl::KdlNode, sf: &SubField) {
    match node.entries_mut().iter_mut().find(|e| e.name().is_none()) {
        Some(entry) => entry.set_value(sf.name.trim().to_string()),
        None => {
            node.insert(0, sf.name.trim().to_string());
        }
    }
    set_type_prop_minimal(node, sf.ty);
    set_bool_prop_minimal(node, "required", sf.required);
}

/// Mutate a matched schema node in place so only its editor-owned parts change: name,
/// `required=`, `type=`/`repeated=` (as applicable to its kind), and, for a structured child,
/// its sub-children (reconciled the same way, one level down). Everything else a real manifest
/// carries on this node (a `doc=` string, comments, formatting) survives because `node` started
/// life as a clone of the original.
fn update_schema_node(node: &mut kdl::KdlNode, e: &SchemaEntry) {
    match node.entries_mut().iter_mut().find(|en| en.name().is_none()) {
        Some(entry) => entry.set_value(e.name.trim().to_string()),
        None => {
            node.insert(0, e.name.trim().to_string());
        }
    }
    set_bool_prop_minimal(node, "required", e.required);

    let structured = e.kind == EntryKind::Child && !e.subfields.is_empty();
    if structured {
        // A Node-typed (child-with-block) entry omits type=.
        node.remove("type");
    } else {
        set_type_prop_minimal(node, e.ty);
    }

    if e.kind == EntryKind::Child {
        set_bool_prop_minimal(node, "repeated", e.repeated);
    } else {
        node.remove("repeated");
    }

    if structured {
        let orig: Vec<kdl::KdlNode> = node
            .children()
            .map(|d| d.nodes().to_vec())
            .unwrap_or_default();
        let mut subs = Vec::with_capacity(e.subfields.len());
        for sf in &e.subfields {
            let n = match sf.origin {
                Some(i) if i < orig.len() && node_kw(&orig[i]) == sub_kw(sf) => {
                    let mut n = orig[i].clone(); // keeps trivia, comments, doc=
                    update_subfield_node(&mut n, sf); // mutate only editor-owned parts
                    n
                }
                // render_subfield's output is a controlled, always-valid single node.
                _ => parse_one_node(&render_subfield(sf)).expect("render_subfield is valid KDL"),
            };
            subs.push(n);
        }
        let mut child_doc = kdl::KdlDocument::new();
        *child_doc.nodes_mut() = subs;
        node.set_children(child_doc);
    } else {
        node.clear_children();
    }
}

/// Write a `ModuleDraft` back into the `KdlDocument` it was loaded from, mutating only the
/// parts the editor models (name, node, summary, schema entries, emit) and leaving everything
/// else (version, migrations, doc= strings, comments, formatting) as the original had it.
///
/// Matched entries (those with an `origin` whose source node still has the right keyword) are
/// cloned from the original and updated in place, so their trivia survives; anything else
/// (a new entry, or an entry whose origin no longer matches) is rendered fresh via `render_entry`
/// (so it gets `doc=""`, like a brand new entry from `render_manifest`).
pub fn reconcile(original: &kdl::KdlDocument, draft: &ModuleDraft) -> Result<String, String> {
    let mut doc = original.clone();
    let module = doc
        .nodes_mut()
        .iter_mut()
        .find(|n| n.name().value() == "module")
        .ok_or_else(|| "missing `module` node".to_string())?;
    if let Some(v) = module.get_mut("name") {
        *v = draft.name.trim().into();
    }
    let node_name = if draft.node.trim().is_empty() {
        draft.name.trim()
    } else {
        draft.node.trim()
    };
    let body = module
        .children_mut()
        .as_mut()
        .ok_or_else(|| "empty module".to_string())?;
    set_child_first_arg(body, "summary", draft.summary.trim());
    set_child_first_arg(body, "claims-node", node_name);

    let schema_node = body
        .nodes_mut()
        .iter_mut()
        .find(|n| n.name().value() == "schema");
    if let Some(schema) = schema_node {
        let orig: Vec<kdl::KdlNode> = schema
            .children()
            .map(|d| d.nodes().to_vec())
            .unwrap_or_default();
        let mut nodes = Vec::with_capacity(draft.entries.len());
        for e in &draft.entries {
            let node = match e.origin {
                Some(i) if i < orig.len() && node_kw(&orig[i]) == entry_kw(e) => {
                    let mut n = orig[i].clone(); // keeps trivia, comments, doc=
                    update_schema_node(&mut n, e); // mutate only editor-owned parts
                    n
                }
                _ => parse_one_node(&render_entry(e))?, // fresh node (doc="")
            };
            nodes.push(node);
        }
        let mut child_doc = kdl::KdlDocument::new();
        *child_doc.nodes_mut() = nodes;
        schema.set_children(child_doc);
    }

    let emit_node = body
        .nodes_mut()
        .iter_mut()
        .find(|n| n.name().value() == "emit");
    if let Some(emit) = emit_node {
        let parsed = format!("{}\n", draft.emit)
            .parse::<kdl::KdlDocument>()
            .map_err(|e| e.to_string())?;
        emit.set_children(parsed);
    }
    Ok(doc.to_string())
}

/// Load and dry-type-check a candidate manifest, the same load path a real module goes
/// through. Used by the TUI to give live feedback on a draft before it is written to disk.
pub fn validate_manifest(text: &str) -> Result<(), String> {
    let doc = text
        .parse::<kdl::KdlDocument>()
        .map_err(|e| e.to_string())?;
    DeclarativeModule::from_kdl(&doc, std::path::Path::new("draft"))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn prop_str(node: &kdl::KdlNode, key: &str) -> Option<String> {
    node.get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
}

fn prop_bool(node: &kdl::KdlNode, key: &str) -> Option<bool> {
    node.get(key).and_then(|v| v.as_bool())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_manifest_is_deterministic() {
        let draft = ModuleDraft {
            name: "cache".into(),
            node: String::new(), // defaults to name
            summary: "a cache".into(),
            entries: vec![SchemaEntry {
                kind: EntryKind::Arg,
                name: "host".into(),
                ty: FieldTy::Str,
                required: true,
                repeated: false,
                subfields: vec![],
                origin: None,
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
                SchemaEntry {
                    kind: EntryKind::Arg,
                    name: "host".into(),
                    ty: FieldTy::Str,
                    required: true,
                    repeated: false,
                    subfields: vec![],
                    origin: None,
                },
                SchemaEntry {
                    kind: EntryKind::Prop,
                    name: "port".into(),
                    ty: FieldTy::Int,
                    required: false,
                    repeated: false,
                    subfields: vec![],
                    origin: None,
                },
                SchemaEntry {
                    kind: EntryKind::Child,
                    name: "alias".into(),
                    ty: FieldTy::Str,
                    required: false,
                    repeated: true,
                    subfields: vec![],
                    origin: None,
                },
            ],
            emit: "set \"services.svc.enable\" #true".into(),
        };
        let m = render_manifest(&draft);
        assert!(
            m.contains("claims-node \"svc\""),
            "node defaults to name: {m}"
        );
        assert!(
            m.contains("arg \"host\" type=\"string\" required=#true"),
            "{m}"
        );
        assert!(
            m.contains("prop \"port\" type=\"int\" required=#false"),
            "{m}"
        );
        assert!(
            m.contains("child \"alias\" type=\"string\" required=#false repeated=#true"),
            "{m}"
        );
        assert!(
            m.contains("set \"services.svc.enable\" #true"),
            "emit spliced: {m}"
        );
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
                SchemaEntry {
                    kind: EntryKind::Arg,
                    name: "host".into(),
                    ty: FieldTy::Str,
                    required: true,
                    repeated: false,
                    subfields: vec![],
                    origin: None,
                },
                SchemaEntry {
                    kind: EntryKind::Child,
                    name: "acme".into(),
                    ty: FieldTy::Str,
                    required: true,
                    repeated: false,
                    subfields: vec![SubField {
                        kind: SubKind::Prop,
                        name: "email".into(),
                        ty: FieldTy::Str,
                        required: true,
                        origin: None,
                    }],
                    origin: None,
                },
            ],
            emit: "set \"services.web.virtualHosts.{host}.enable\" #true".into(),
        };
        let m = render_manifest(&draft);
        // Structured child: block form carrying required=/repeated=, but no type= (a
        // child-with-block is Node-typed).
        assert!(
            m.contains("child \"acme\" required=#true repeated=#false {"),
            "structured child block: {m}"
        );
        assert!(
            m.contains("prop \"email\" type=\"string\" required=#true"),
            "{m}"
        );
        assert!(
            !m.contains("child \"acme\" type="),
            "structured child omits type=: {m}"
        );
        validate_manifest(&m).expect("rendered structured draft is valid");
    }

    #[test]
    fn render_manifest_repeated_structured_child_drives_for_each() {
        // A repeated structured child (the web-service `location` pattern) must render
        // `repeated=#true` on the block line so a for-each over it dry-type-checks.
        let draft = ModuleDraft {
            name: "web".into(),
            node: "web".into(),
            summary: String::new(),
            entries: vec![SchemaEntry {
                kind: EntryKind::Child, name: "location".into(), ty: FieldTy::Str,
                required: false, repeated: true,
                subfields: vec![
                    SubField { kind: SubKind::Arg, name: "match".into(), ty: FieldTy::Str, required: true, origin: None },
                    SubField { kind: SubKind::Prop, name: "upstream".into(), ty: FieldTy::Str, required: true, origin: None },
                ],
                origin: None,
            }],
            emit: "for-each \"loc\" in \"location\" {\n    set \"services.web.locations.{loc.match}.proxyPass\" \"{loc.upstream}\"\n}".into(),
        };
        let m = render_manifest(&draft);
        assert!(
            m.contains("child \"location\" required=#false repeated=#true {"),
            "{m}"
        );
        validate_manifest(&m).expect("repeated structured child + for-each is valid");
    }

    #[test]
    fn validate_manifest_rejects_an_undeclared_binding() {
        // emit references {missing}, which the dry type-pass rejects at load.
        let draft = ModuleDraft {
            name: "bad".into(),
            node: "bad".into(),
            summary: String::new(),
            entries: vec![SchemaEntry {
                kind: EntryKind::Arg,
                name: "host".into(),
                ty: FieldTy::Str,
                required: true,
                repeated: false,
                subfields: vec![],
                origin: None,
            }],
            emit: "set \"services.bad.{missing}\" #true".into(),
        };
        let err = validate_manifest(&render_manifest(&draft)).unwrap_err();
        assert!(
            err.contains("missing") || err.contains("unknown binding"),
            "got: {err}"
        );
    }

    #[test]
    fn validate_manifest_reports_a_kdl_parse_error() {
        assert!(validate_manifest("this is { not valid").is_err());
    }

    fn web_service_manifest() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/stdlib/web-service/knixl-module.kdl"
        ))
        .expect("read web-service manifest")
    }

    #[test]
    fn load_editable_reads_header_entries_and_emit() {
        let ed = load_editable(&web_service_manifest()).expect("loads");
        assert_eq!(ed.node, "web-service");
        assert!(
            ed.entries.iter().any(|e| e.name == "host"),
            "has the host entry"
        );
        assert!(
            ed.entries
                .iter()
                .any(|e| e.kind == EntryKind::Child && e.name == "location" && e.repeated),
            "location is a repeated child"
        );
        assert!(
            ed.entries.iter().all(|e| e.origin.is_some()),
            "every loaded entry has an origin"
        );
        assert!(!ed.emit.trim().is_empty(), "emit text captured");
    }

    #[test]
    fn reconcile_with_no_edits_preserves_version_migrations_and_docs() {
        let src = web_service_manifest();
        let ed = load_editable(&src).expect("loads");
        let draft = ModuleDraft {
            name: ed.name.clone(),
            node: ed.node.clone(),
            summary: ed.summary.clone(),
            entries: ed.entries.clone(),
            emit: ed.emit.clone(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        validate_manifest(&out).expect("reconciled manifest is valid");
        // Content the editor does not model must survive.
        assert!(
            out.contains("migrations"),
            "migrations block preserved: {out}"
        );
        assert!(
            out.contains("serverAliases is generated"),
            "a migration note preserved"
        );
        assert!(
            out.contains("doc=\"Additional server name.\""),
            "a doc string preserved"
        );
        // version is not 0.1.0 (render_manifest's default) : the original version survived.
        let orig_ver = src
            .split("version=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        assert!(
            out.contains(&format!("version=\"{orig_ver}\"")),
            "version {orig_ver} preserved: {out}"
        );
    }

    #[test]
    fn reconcile_toggling_required_updates_only_that_node() {
        let ed = load_editable(&web_service_manifest()).expect("loads");
        let mut entries = ed.entries.clone();
        let host = entries
            .iter_mut()
            .find(|e| e.name == "host")
            .expect("host entry");
        let was = host.required;
        host.required = !was;
        let draft = ModuleDraft {
            name: ed.name.clone(),
            node: ed.node.clone(),
            summary: ed.summary.clone(),
            entries,
            emit: ed.emit.clone(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        validate_manifest(&out).expect("valid");
        // doc strings still present (node was updated in place, not rebuilt fresh).
        assert!(
            out.contains("doc=\"Additional server name.\""),
            "unrelated doc preserved: {out}"
        );
    }

    #[test]
    fn reconcile_adds_a_new_entry_and_drops_a_removed_one() {
        // A synthetic manifest with an entry (`spare`) the emit does not reference, so dropping
        // it still dry-type-checks. (web-service's emit references every schema entry, so a
        // removal there would fail validation on the dropped binding, not on reconcile.)
        let src = "module name=\"demo\" version=\"1.0.0\" {\n    summary \"s\"\n    claims-node \"demo\"\n    schema {\n        arg \"host\" type=\"string\" required=#true doc=\"h\"\n        prop \"spare\" type=\"string\" required=#false doc=\"unused\"\n    }\n    emit {\n        set \"services.demo.{host}.enable\" #true\n    }\n}\n";
        let ed = load_editable(src).expect("loads");
        let mut entries = ed.entries.clone();
        // add a fresh arg (origin None) and drop the unreferenced `spare` prop.
        entries.push(SchemaEntry {
            kind: EntryKind::Arg,
            name: "extra".into(),
            ty: FieldTy::Str,
            required: false,
            repeated: false,
            subfields: vec![],
            origin: None,
        });
        entries.retain(|e| e.name != "spare");
        let draft = ModuleDraft {
            name: ed.name.clone(),
            node: ed.node.clone(),
            summary: ed.summary.clone(),
            entries,
            emit: ed.emit.clone(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        assert!(out.contains("arg \"extra\""), "new entry rendered: {out}");
        assert!(!out.contains("\"spare\""), "removed entry gone: {out}");
        validate_manifest(&out).expect("valid");
    }

    #[test]
    fn reconcile_does_not_add_default_props_to_untouched_nodes() {
        // A bare `arg "host"` (no type=/required=) must stay clean after an unedited reconcile:
        // the defaults match the loader, so writing them back would be pure churn.
        let src = "module name=\"demo\" version=\"1.0.0\" {\n    summary \"s\"\n    claims-node \"demo\"\n    schema {\n        arg \"host\"\n    }\n    emit {\n        set \"services.demo.{host}.enable\" #true\n    }\n}\n";
        let ed = load_editable(src).expect("loads");
        let draft = ModuleDraft {
            name: ed.name.clone(),
            node: ed.node.clone(),
            summary: ed.summary.clone(),
            entries: ed.entries.clone(),
            emit: ed.emit.clone(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        assert!(
            !out.contains("required=#false"),
            "no default required= churn: {out}"
        );
        assert!(
            !out.contains("type=\"string\""),
            "no default type= churn: {out}"
        );
        validate_manifest(&out).expect("valid");
    }

    #[test]
    fn reconcile_replaces_the_emit_block() {
        let ed = load_editable(&web_service_manifest()).expect("loads");
        let draft = ModuleDraft {
            name: ed.name.clone(),
            node: ed.node.clone(),
            summary: ed.summary.clone(),
            entries: ed.entries.clone(),
            emit: "set \"services.nginx.enable\" #true".into(),
        };
        let out = reconcile(&ed.doc, &draft).expect("reconcile");
        assert!(
            out.contains("services.nginx.enable"),
            "new emit present: {out}"
        );
        assert!(
            out.contains("migrations"),
            "migrations still preserved: {out}"
        );
        validate_manifest(&out).expect("valid");
    }
}
