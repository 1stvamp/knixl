//! EmitTemplate: the substitution grammar for declarative modules. Parsed once from a
//! module's `emit { }` block, interpreted per-node against a bindings tree built from the
//! validated input. Five statement forms (set, when-flag, when-config, for-each, list). SPEC-GRADE SKETCH.

use crate::{Bucket, Child, Field, LowerError, LowerOutput, NodeSchema, Unit, ValueTy};
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr, RawNix};
use knixl_kdl::children_named;

pub struct EmitTemplate {
    pub stmts: Vec<Stmt>,
}

pub enum Stmt {
    Set {
        path: PathTemplate,
        value: ValueTemplate,
    },
    WhenFlag {
        flag: String,
        body: Vec<Stmt>,
    }, // generation-time gate
    WhenConfig {
        cond: Vec<StrPart>,
        body: Vec<Stmt>,
    }, // runtime lib.mkIf off config.*
    ForEach {
        var: String,
        source: String,
        body: Vec<Stmt>,
    }, // binds <var> per item
    List {
        path: PathTemplate,
        source: String,
        body: Vec<Stmt>,
    }, // fold a repeated child into a list of attribute sets
}

pub struct PathTemplate(pub Vec<SegmentTemplate>);

pub enum SegmentTemplate {
    Ident(String),     // services, nginx  -> AttrKey::Ident
    QuotedLit(String), // "/"              -> AttrKey::Quoted, may interpolate
    Interp(Lookup),    // {host}           -> AttrKey::Quoted (dynamic name)
}

pub enum ValueTemplate {
    Bool(bool),
    Int(i128),
    Str(Vec<StrPart>),       // "{upstream}"
    IndentStr(Vec<StrPart>), // (indent-str #""" ... """#)
    Collect(String),         // (collect "alias") -> List of that child's first arg
}

pub enum StrPart {
    Lit(String),
    Interp(Lookup),
}

/// A dotted lookup into the bindings tree: `host`, `acme.email`, `loc.match`.
pub struct Lookup(pub Vec<String>);

// ---- bindings ----

#[derive(Clone)]
pub enum Binding {
    Scalar(Scalar),
    Scope(std::collections::BTreeMap<String, Binding>),
    List(Vec<Binding>),
}
#[derive(Clone)]
pub enum Scalar {
    Bool(bool),
    Int(i128),
    Str(String),
}
impl Scalar {
    pub fn as_str(&self) -> Result<String, LowerError> {
        match self {
            Scalar::Str(s) => Ok(s.clone()),
            _ => Err(LowerError::Other("expected string".into())),
        }
    }
}

pub struct Bindings(pub std::collections::BTreeMap<String, Binding>);

pub struct LoopScopes(Vec<(String, Binding)>);
impl LoopScopes {
    pub fn new() -> Self {
        Self(Vec::new())
    }
    pub fn push(&mut self, var: &str, item: &Binding) {
        self.0.push((var.to_string(), item.clone()));
    }
    pub fn pop(&mut self) {
        self.0.pop();
    }
    /// Most-recently pushed binding for `var`, so an inner loop shadows an outer one.
    fn get(&self, var: &str) -> Option<&Binding> {
        self.0
            .iter()
            .rev()
            .find(|(name, _)| name == var)
            .map(|(_, b)| b)
    }
}
impl Default for LoopScopes {
    fn default() -> Self {
        Self::new()
    }
}

impl Lookup {
    fn resolve<'a>(
        &self,
        b: &'a Bindings,
        loops: &'a LoopScopes,
    ) -> Result<&'a Scalar, LowerError> {
        let (first, rest) = self
            .0
            .split_first()
            .ok_or_else(|| LowerError::Other("empty lookup".into()))?;

        // Loop variables shadow top-level bindings.
        let mut current = loops
            .get(first)
            .or_else(|| b.0.get(first))
            .ok_or_else(|| LowerError::Other(format!("unknown binding `{first}`")))?;

        for seg in rest {
            current = match current {
                Binding::Scope(map) => map
                    .get(seg)
                    .ok_or_else(|| LowerError::Other(format!("`{seg}` is not a field here")))?,
                _ => return Err(LowerError::Other(format!("`{first}` is not a scope"))),
            };
        }

        match current {
            Binding::Scalar(s) => Ok(s),
            _ => Err(LowerError::Other(format!(
                "`{}` resolves to a non-scalar",
                self.0.join(".")
            ))),
        }
    }
}

// ---- interpret ----

impl EmitTemplate {
    /// Built once from the schema+node at lower() time (walks the SCHEMA so resolution is total).
    pub fn build_bindings(schema: &NodeSchema, node: &kdl::KdlNode) -> Bindings {
        let mut map = std::collections::BTreeMap::new();

        let positional: Vec<&kdl::KdlEntry> = node
            .entries()
            .iter()
            .filter(|e| e.name().is_none())
            .collect();
        for (i, field) in schema.args.iter().enumerate() {
            if let Some(entry) = positional.get(i) {
                map.insert(
                    field.name.clone(),
                    Binding::Scalar(scalar_from(entry.value())),
                );
            }
        }
        for field in &schema.props {
            if let Some(v) = node.get(field.name.as_str()) {
                map.insert(field.name.clone(), Binding::Scalar(scalar_from(v)));
            }
        }
        for child in &schema.children {
            let matching: Vec<&kdl::KdlNode> = children_named(node, &child.name).collect();
            if child.repeated {
                let list = matching
                    .iter()
                    .map(|c| binding_for_child(child, c))
                    .collect();
                map.insert(child.name.clone(), Binding::List(list));
            } else if matches!(child.ty, ValueTy::Bool) {
                // Flag: an explicit boolean wins, otherwise presence-as-true.
                let val = matching
                    .first()
                    .and_then(|c| c.entries().iter().find(|e| e.name().is_none()))
                    .and_then(|e| e.value().as_bool())
                    .unwrap_or(!matching.is_empty());
                map.insert(child.name.clone(), Binding::Scalar(Scalar::Bool(val)));
            } else if let Some(c) = matching.first() {
                map.insert(child.name.clone(), binding_for_child(child, c));
            }
        }
        Bindings(map)
    }

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
                        condition: cond.map(|c| {
                            NixExpr::Raw(RawNix {
                                src: c.to_string(),
                                span: None,
                            })
                        }),
                        doc: None,
                    };
                    out.push(Unit {
                        bucket: Bucket::Default,
                        assignment: a,
                        module: String::new(),
                    });
                }
                Stmt::WhenFlag { flag, body } => {
                    if resolve_flag(flag, b, loops)? {
                        self.run(body, b, loops, cond, out)?;
                    }
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
                    for item in resolve_list(source, b)? {
                        // source order => stable
                        loops.push(var, item);
                        self.run(body, b, loops, cond, out)?;
                        loops.pop();
                    }
                }
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
                        condition: cond.map(|c| {
                            NixExpr::Raw(RawNix {
                                src: c.to_string(),
                                span: None,
                            })
                        }),
                        doc: None,
                    };
                    out.push(Unit {
                        bucket: Bucket::Default,
                        assignment,
                        module: String::new(),
                    });
                }
            }
        }
        Ok(())
    }
}

impl PathTemplate {
    fn interpret(&self, b: &Bindings, loops: &LoopScopes) -> Result<AttrPath, LowerError> {
        let mut segs = Vec::with_capacity(self.0.len());
        for s in &self.0 {
            segs.push(match s {
                SegmentTemplate::Ident(w) => AttrKey::Ident(w.clone()),
                SegmentTemplate::QuotedLit(t) => AttrKey::Quoted(interp_str(t, b, loops)?),
                SegmentTemplate::Interp(lk) => AttrKey::Quoted(lk.resolve(b, loops)?.as_str()?),
            });
        }
        Ok(AttrPath(segs))
    }
}

impl ValueTemplate {
    fn interpret(&self, b: &Bindings, loops: &LoopScopes) -> Result<NixExpr, LowerError> {
        Ok(match self {
            ValueTemplate::Bool(v) => NixExpr::Bool(*v),
            ValueTemplate::Int(v) => NixExpr::Int(*v),
            ValueTemplate::Str(parts) => NixExpr::Str(interp_parts(parts, b, loops)?),
            ValueTemplate::IndentStr(parts) => NixExpr::IndentStr(interp_parts(parts, b, loops)?),
            ValueTemplate::Collect(child) => {
                let mut items = Vec::new();
                for item in resolve_list(child, b)? {
                    match item {
                        Binding::Scalar(s) => items.push(scalar_to_expr(s)),
                        _ => {
                            return Err(LowerError::Other(format!(
                                "collect `{child}` expects scalar items"
                            )))
                        }
                    }
                }
                NixExpr::List(items)
            }
        })
    }
}

fn interp_str(t: &str, b: &Bindings, loops: &LoopScopes) -> Result<String, LowerError> {
    interp_parts(&parse_str_parts(t), b, loops)
}

fn interp_parts(parts: &[StrPart], b: &Bindings, loops: &LoopScopes) -> Result<String, LowerError> {
    let mut out = String::new();
    for part in parts {
        match part {
            StrPart::Lit(s) => out.push_str(s),
            StrPart::Interp(lk) => out.push_str(&lk.resolve(b, loops)?.as_str()?),
        }
    }
    Ok(out)
}

fn resolve_flag(flag: &str, b: &Bindings, loops: &LoopScopes) -> Result<bool, LowerError> {
    // A flag may be a dotted lookup (e.g. `network.managed` inside a `list` body), so split
    // it the same way every other lookup is split.
    match Lookup(flag.split('.').map(str::to_string).collect()).resolve(b, loops)? {
        Scalar::Bool(v) => Ok(*v),
        _ => Err(LowerError::Other(format!("`{flag}` is not a boolean flag"))),
    }
}

fn resolve_list<'a>(source: &str, b: &'a Bindings) -> Result<&'a [Binding], LowerError> {
    match b.0.get(source) {
        Some(Binding::List(items)) => Ok(items),
        Some(_) => Err(LowerError::Other(format!(
            "`{source}` is not a repeated child"
        ))),
        None => Ok(&[]), // absent repeated child => no items
    }
}

enum AttrNode {
    Leaf(NixExpr),
    Branch(std::collections::BTreeMap<AttrKey, AttrNode>),
}

fn attr_key_str(k: &AttrKey) -> String {
    match k {
        AttrKey::Ident(s) | AttrKey::Quoted(s) => s.clone(),
    }
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

fn scalar_from(v: &kdl::KdlValue) -> Scalar {
    if let Some(b) = v.as_bool() {
        Scalar::Bool(b)
    } else if let Some(i) = v.as_integer() {
        Scalar::Int(i)
    } else if let Some(s) = v.as_string() {
        Scalar::Str(s.to_string())
    } else {
        Scalar::Str(String::new())
    }
}

fn scalar_to_expr(s: &Scalar) -> NixExpr {
    match s {
        Scalar::Bool(v) => NixExpr::Bool(*v),
        Scalar::Int(v) => NixExpr::Int(*v),
        Scalar::Str(v) => NixExpr::Str(v.clone()),
    }
}

/// A structured child (with sub-args/props) becomes a `Scope`; a scalar child becomes a
/// `Scalar` from its first positional argument.
fn binding_for_child(child: &Child, node: &kdl::KdlNode) -> Binding {
    if child.args.is_empty() && child.props.is_empty() {
        let scalar = node
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .map(|e| scalar_from(e.value()))
            .unwrap_or(Scalar::Bool(true));
        return Binding::Scalar(scalar);
    }
    let mut map = std::collections::BTreeMap::new();
    let positional: Vec<&kdl::KdlEntry> = node
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .collect();
    for (i, f) in child.args.iter().enumerate() {
        if let Some(e) = positional.get(i) {
            map.insert(f.name.clone(), Binding::Scalar(scalar_from(e.value())));
        }
    }
    for f in &child.props {
        if let Some(v) = node.get(f.name.as_str()) {
            map.insert(f.name.clone(), Binding::Scalar(scalar_from(v)));
        } else if matches!(f.ty, ValueTy::Bool) && !f.required {
            // An absent *optional* bool prop defaults to false, so a `when-flag` on it
            // inside a `list`/`for-each` body has something to resolve. A required prop is
            // left absent so its lookup still errors at generate (no silent fallback).
            map.insert(f.name.clone(), Binding::Scalar(Scalar::Bool(false)));
        }
    }
    Binding::Scope(map)
}

/// Split a dotted path on top-level `.`, keeping `{...}` interpolations and `"..."` quoted
/// segments intact (a dot inside either is literal).
fn split_path(s: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let mut cur = String::new();
    let mut depth = 0u32;
    let mut in_quote = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quote = !in_quote;
                cur.push(c);
            }
            '{' if !in_quote => {
                depth += 1;
                cur.push(c);
            }
            '}' if !in_quote => {
                depth = depth.saturating_sub(1);
                cur.push(c);
            }
            '.' if depth == 0 && !in_quote => segs.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        segs.push(cur);
    }
    segs
}

fn classify_segment(seg: &str) -> SegmentTemplate {
    if let Some(inner) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        SegmentTemplate::Interp(Lookup(inner.split('.').map(str::to_string).collect()))
    } else if let Some(inner) = seg.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        SegmentTemplate::QuotedLit(inner.to_string())
    } else {
        SegmentTemplate::Ident(seg.to_string())
    }
}

/// Split a literal string into literal and `{lookup}` parts.
fn parse_str_parts(s: &str) -> Vec<StrPart> {
    let mut parts = Vec::new();
    let mut rest = s;
    while let Some(open) = rest.find('{') {
        if open > 0 {
            parts.push(StrPart::Lit(rest[..open].to_string()));
        }
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let lookup = &after[..close];
                parts.push(StrPart::Interp(Lookup(
                    lookup.split('.').map(str::to_string).collect(),
                )));
                rest = &after[close + 1..];
            }
            None => {
                parts.push(StrPart::Lit(rest.to_string()));
                return parts;
            }
        }
    }
    if !rest.is_empty() {
        parts.push(StrPart::Lit(rest.to_string()));
    }
    parts
}

// ---- KDL parsing of the schema and emit blocks ----

fn parse_schema_block(schema_node: &kdl::KdlNode) -> Result<NodeSchema, LowerError> {
    let mut args = Vec::new();
    let mut props = Vec::new();
    let mut children = Vec::new();
    if let Some(body) = schema_node.children() {
        for n in body.nodes() {
            match n.name().value() {
                "arg" => args.push(parse_field(n)?),
                "prop" => props.push(parse_field(n)?),
                "child" => children.push(parse_child(n)?),
                other => return Err(LowerError::Other(format!("unexpected `{other}` in schema"))),
            }
        }
    }
    // Declarative modules are leaves: they read their own subtree, never delegate.
    Ok(NodeSchema {
        summary: String::new(),
        args,
        props,
        children,
        open_children: false,
    })
}

fn parse_field(n: &kdl::KdlNode) -> Result<Field, LowerError> {
    let name =
        arg_str(n, 0).ok_or_else(|| LowerError::Other("schema field missing name".into()))?;
    Ok(Field {
        name,
        ty: ty_from(prop_str(n, "type").as_deref()),
        required: prop_bool(n, "required").unwrap_or(false),
        doc: prop_str(n, "doc").unwrap_or_default(),
    })
}

fn parse_child(n: &kdl::KdlNode) -> Result<Child, LowerError> {
    let name =
        arg_str(n, 0).ok_or_else(|| LowerError::Other("schema child missing name".into()))?;
    let required = prop_bool(n, "required").unwrap_or(false);
    let repeated = prop_bool(n, "repeated").unwrap_or(false);
    let doc = prop_str(n, "doc").unwrap_or_default();

    // A child with a block is structured: its own args/props form a Scope.
    if let Some(body) = n.children() {
        let mut args = Vec::new();
        let mut props = Vec::new();
        for s in body.nodes() {
            match s.name().value() {
                "arg" => args.push(parse_field(s)?),
                "prop" => props.push(parse_field(s)?),
                other => {
                    return Err(LowerError::Other(format!(
                        "unexpected `{other}` in structured child"
                    )))
                }
            }
        }
        Ok(Child {
            name,
            ty: ValueTy::Node,
            required,
            repeated,
            delegate: false,
            doc,
            args,
            props,
        })
    } else {
        Ok(Child {
            name,
            ty: ty_from(prop_str(n, "type").as_deref()),
            required,
            repeated,
            delegate: false,
            doc,
            args: Vec::new(),
            props: Vec::new(),
        })
    }
}

fn ty_from(ty: Option<&str>) -> ValueTy {
    match ty {
        Some("bool") => ValueTy::Bool,
        Some("int") => ValueTy::Int,
        _ => ValueTy::Str,
    }
}

fn parse_stmts(doc: Option<&kdl::KdlDocument>) -> Result<Vec<Stmt>, LowerError> {
    let mut stmts = Vec::new();
    if let Some(body) = doc {
        for n in body.nodes() {
            stmts.push(parse_stmt(n)?);
        }
    }
    Ok(stmts)
}

fn parse_stmt(n: &kdl::KdlNode) -> Result<Stmt, LowerError> {
    match n.name().value() {
        "set" => {
            let positional: Vec<&kdl::KdlEntry> =
                n.entries().iter().filter(|e| e.name().is_none()).collect();
            let path = positional
                .first()
                .and_then(|e| e.value().as_string())
                .ok_or_else(|| LowerError::Other("`set` missing path".into()))?;
            let value = positional
                .get(1)
                .ok_or_else(|| LowerError::Other("`set` missing value".into()))?;
            Ok(Stmt::Set {
                path: parse_path(path),
                value: parse_value(value)?,
            })
        }
        "when-flag" => {
            let flag = arg_str(n, 0)
                .ok_or_else(|| LowerError::Other("`when-flag` missing flag".into()))?;
            Ok(Stmt::WhenFlag {
                flag,
                body: parse_stmts(n.children())?,
            })
        }
        "when-config" => {
            // An empty or whitespace-only condition would emit `lib.mkIf () <value>`, invalid
            // Nix. Reject it at load, where the rest of the grammar errors live.
            let cond = arg_str(n, 0)
                .filter(|c| !c.trim().is_empty())
                .ok_or_else(|| {
                    LowerError::Other("`when-config` needs a non-empty condition".into())
                })?;
            Ok(Stmt::WhenConfig {
                cond: parse_str_parts(&cond),
                body: parse_stmts(n.children())?,
            })
        }
        "for-each" => {
            // `for-each "loc" in "location"`: the bare `in` is noise; take first + last.
            let args: Vec<String> = n
                .entries()
                .iter()
                .filter(|e| e.name().is_none())
                .filter_map(|e| e.value().as_string().map(str::to_string))
                .collect();
            let var = args
                .first()
                .cloned()
                .ok_or_else(|| LowerError::Other("`for-each` missing var".into()))?;
            let source = args
                .last()
                .cloned()
                .ok_or_else(|| LowerError::Other("`for-each` missing source".into()))?;
            Ok(Stmt::ForEach {
                var,
                source,
                body: parse_stmts(n.children())?,
            })
        }
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
            Ok(Stmt::List {
                path: parse_path(&path),
                source,
                body: parse_stmts(n.children())?,
            })
        }
        other => Err(LowerError::Other(format!(
            "unknown emit statement `{other}`"
        ))),
    }
}

fn parse_path(s: &str) -> PathTemplate {
    PathTemplate(
        split_path(s)
            .iter()
            .map(|seg| classify_segment(seg))
            .collect(),
    )
}

fn parse_value(entry: &kdl::KdlEntry) -> Result<ValueTemplate, LowerError> {
    match entry.ty().map(|t| t.value()) {
        Some("collect") => {
            let child = entry
                .value()
                .as_string()
                .ok_or_else(|| LowerError::Other("collect needs a child name".into()))?;
            Ok(ValueTemplate::Collect(child.to_string()))
        }
        Some("indent-str") => {
            let s = entry
                .value()
                .as_string()
                .ok_or_else(|| LowerError::Other("indent-str needs a string".into()))?;
            Ok(ValueTemplate::IndentStr(parse_str_parts(s)))
        }
        Some(other) => Err(LowerError::Other(format!(
            "unknown value annotation `{other}`"
        ))),
        None => {
            let v = entry.value();
            if let Some(b) = v.as_bool() {
                Ok(ValueTemplate::Bool(b))
            } else if let Some(i) = v.as_integer() {
                Ok(ValueTemplate::Int(i))
            } else if let Some(s) = v.as_string() {
                Ok(ValueTemplate::Str(parse_str_parts(s)))
            } else {
                Err(LowerError::Other("unsupported set value".into()))
            }
        }
    }
}

fn arg_str(node: &kdl::KdlNode, idx: usize) -> Option<String> {
    node.entries()
        .iter()
        .filter(|e| e.name().is_none())
        .nth(idx)
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}

fn prop_str(node: &kdl::KdlNode, key: &str) -> Option<String> {
    node.get(key)
        .and_then(|v| v.as_string())
        .map(str::to_string)
}

fn prop_bool(node: &kdl::KdlNode, key: &str) -> Option<bool> {
    node.get(key).and_then(|v| v.as_bool())
}

// ---- module-load dry type-pass (docs/04): catch bad lookups at load, not at generate ----

type ShapeMap = std::collections::BTreeMap<String, Shape>;

enum Shape {
    Scalar,
    Scope(ShapeMap),
    List(Box<Shape>),
}

fn schema_shape(schema: &NodeSchema) -> ShapeMap {
    let mut m = ShapeMap::new();
    for f in &schema.args {
        m.insert(f.name.clone(), Shape::Scalar);
    }
    for f in &schema.props {
        m.insert(f.name.clone(), Shape::Scalar);
    }
    for c in &schema.children {
        let base = child_shape(c);
        m.insert(
            c.name.clone(),
            if c.repeated {
                Shape::List(Box::new(base))
            } else {
                base
            },
        );
    }
    m
}

fn child_shape(c: &Child) -> Shape {
    if c.args.is_empty() && c.props.is_empty() {
        Shape::Scalar
    } else {
        let mut m = ShapeMap::new();
        for f in c.args.iter().chain(c.props.iter()) {
            m.insert(f.name.clone(), Shape::Scalar);
        }
        Shape::Scope(m)
    }
}

/// Verify every template lookup resolves to the right shape against the schema. Runs once
/// at load, so `{acme.email}` resolving to a scope (rather than a scalar) fails here.
fn dry_check(schema: &NodeSchema, template: &EmitTemplate) -> Result<(), LowerError> {
    let shapes = schema_shape(schema);
    let mut loops: Vec<(&str, &Shape)> = Vec::new();
    let mut errors = Vec::new();
    check_stmts(&template.stmts, &shapes, &mut loops, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(LowerError::Other(format!(
            "template does not type-check: {}",
            errors.join("; ")
        )))
    }
}

fn lookup_shape<'a>(
    segs: &[String],
    shapes: &'a ShapeMap,
    loops: &[(&'a str, &'a Shape)],
) -> Result<&'a Shape, String> {
    let (first, rest) = segs
        .split_first()
        .ok_or_else(|| "empty lookup".to_string())?;
    let mut cur = loops
        .iter()
        .rev()
        .find(|(n, _)| n == first)
        .map(|(_, s)| *s)
        .or_else(|| shapes.get(first))
        .ok_or_else(|| format!("unknown binding `{first}`"))?;
    for seg in rest {
        cur = match cur {
            Shape::Scope(m) => m
                .get(seg)
                .ok_or_else(|| format!("`{seg}` is not a field here"))?,
            _ => return Err(format!("`{first}` is not a scope")),
        };
    }
    Ok(cur)
}

fn expect_scalar(
    segs: &[String],
    shapes: &ShapeMap,
    loops: &[(&str, &Shape)],
    errors: &mut Vec<String>,
) {
    match lookup_shape(segs, shapes, loops) {
        Ok(Shape::Scalar) => {}
        Ok(_) => errors.push(format!("`{}` is not a scalar", segs.join("."))),
        Err(e) => errors.push(e),
    }
}

fn check_stmts<'a>(
    stmts: &'a [Stmt],
    shapes: &'a ShapeMap,
    loops: &mut Vec<(&'a str, &'a Shape)>,
    errors: &mut Vec<String>,
) {
    for st in stmts {
        match st {
            Stmt::Set { path, value } => {
                for seg in &path.0 {
                    match seg {
                        SegmentTemplate::Interp(lk) => expect_scalar(&lk.0, shapes, loops, errors),
                        SegmentTemplate::QuotedLit(t) => {
                            check_str_lookups(t, shapes, loops, errors)
                        }
                        SegmentTemplate::Ident(_) => {}
                    }
                }
                match value {
                    ValueTemplate::Str(parts) | ValueTemplate::IndentStr(parts) => {
                        for part in parts {
                            if let StrPart::Interp(lk) = part {
                                expect_scalar(&lk.0, shapes, loops, errors);
                            }
                        }
                    }
                    ValueTemplate::Collect(child) => {
                        match lookup_shape(std::slice::from_ref(child), shapes, loops) {
                            Ok(Shape::List(_)) => {}
                            Ok(_) => {
                                errors.push(format!("collect `{child}` is not a repeated child"))
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                    ValueTemplate::Bool(_) | ValueTemplate::Int(_) => {}
                }
            }
            Stmt::WhenFlag { flag, body } => {
                let segs: Vec<String> = flag.split('.').map(str::to_string).collect();
                expect_scalar(&segs, shapes, loops, errors);
                check_stmts(body, shapes, loops, errors);
            }
            Stmt::WhenConfig { cond, body } => {
                for part in cond {
                    if let StrPart::Interp(lk) = part {
                        expect_scalar(&lk.0, shapes, loops, errors);
                    }
                }
                check_stmts(body, shapes, loops, errors);
            }
            Stmt::ForEach { var, source, body } => {
                match lookup_shape(std::slice::from_ref(source), shapes, loops) {
                    Ok(Shape::List(inner)) => {
                        loops.push((var.as_str(), inner));
                        check_stmts(body, shapes, loops, errors);
                        loops.pop();
                    }
                    Ok(_) => errors.push(format!(
                        "for-each source `{source}` is not a repeated child"
                    )),
                    Err(e) => errors.push(e),
                }
            }
            Stmt::List { path, source, body } => {
                for seg in &path.0 {
                    match seg {
                        SegmentTemplate::Interp(lk) => expect_scalar(&lk.0, shapes, loops, errors),
                        SegmentTemplate::QuotedLit(t) => {
                            check_str_lookups(t, shapes, loops, errors)
                        }
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
        }
    }
}

fn check_str_lookups(
    raw: &str,
    shapes: &ShapeMap,
    loops: &[(&str, &Shape)],
    errors: &mut Vec<String>,
) {
    for part in parse_str_parts(raw) {
        if let StrPart::Interp(lk) = part {
            expect_scalar(&lk.0, shapes, loops, errors);
        }
    }
}

// ---- the declarative module: one Module impl carrying every KDL-defined module ----

use crate::{Module, ModuleId};

pub struct DeclarativeModule {
    id: ModuleId,
    node_name: String,
    schema: NodeSchema,
    template: EmitTemplate,
    migrations: Vec<crate::MigrationNote>,
}

impl DeclarativeModule {
    pub fn from_kdl(doc: &kdl::KdlDocument, source: &std::path::Path) -> Result<Self, LowerError> {
        let where_ = || source.display().to_string();
        let module = doc
            .nodes()
            .iter()
            .find(|n| n.name().value() == "module")
            .ok_or_else(|| LowerError::Other(format!("{}: missing `module` node", where_())))?;

        let name = prop_str(module, "name")
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `name`", where_())))?;
        let version_str = prop_str(module, "version")
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `version`", where_())))?;
        let version = version_str.parse().map_err(|e| {
            LowerError::Other(format!("{}: bad version `{version_str}`: {e}", where_()))
        })?;

        let body = module
            .children()
            .ok_or_else(|| LowerError::Other(format!("{}: empty module", where_())))?;

        let mut summary = String::new();
        let mut node_name = None;
        let mut schema = None;
        let mut template = None;
        let mut migrations = Vec::new();
        for child in body.nodes() {
            match child.name().value() {
                "summary" => summary = arg_str(child, 0).unwrap_or_default(),
                "claims-node" => node_name = arg_str(child, 0),
                "schema" => schema = Some(parse_schema_block(child)?),
                "emit" => {
                    template = Some(EmitTemplate {
                        stmts: parse_stmts(child.children())?,
                    })
                }
                "migrations" => migrations = parse_migrations(child, &where_)?,
                other => {
                    return Err(LowerError::Other(format!(
                        "{}: unexpected `{other}` in module",
                        where_()
                    )))
                }
            }
        }

        let node_name = node_name.ok_or_else(|| {
            LowerError::Other(format!("{}: module missing `claims-node`", where_()))
        })?;
        let mut schema = schema
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `schema`", where_())))?;
        schema.summary = summary;
        let template = template
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `emit`", where_())))?;

        dry_check(&schema, &template)
            .map_err(|e| LowerError::Other(format!("{}: {e}", where_())))?;

        Ok(DeclarativeModule {
            id: ModuleId { name, version },
            node_name,
            schema,
            template,
            migrations,
        })
    }
}

/// Parse a `migrations` block: each `to "<version>"` child holds one or more `note` lines.
fn parse_migrations(
    node: &kdl::KdlNode,
    where_: &impl Fn() -> String,
) -> Result<Vec<crate::MigrationNote>, LowerError> {
    let mut steps = Vec::new();
    let Some(body) = node.children() else {
        return Ok(steps);
    };
    for step in body.nodes() {
        if step.name().value() != "to" {
            return Err(LowerError::Other(format!(
                "{}: unexpected `{}` in migrations (expected `to`)",
                where_(),
                step.name().value()
            )));
        }
        let ver_str = arg_str(step, 0).ok_or_else(|| {
            LowerError::Other(format!("{}: migration `to` needs a version", where_()))
        })?;
        let to = ver_str.parse().map_err(|e| {
            LowerError::Other(format!(
                "{}: bad migration version `{ver_str}`: {e}",
                where_()
            ))
        })?;
        let notes = step
            .children()
            .map(|b| {
                b.nodes()
                    .iter()
                    .filter(|n| n.name().value() == "note")
                    .filter_map(|n| arg_str(n, 0))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        steps.push(crate::MigrationNote { to, notes });
    }
    Ok(steps)
}

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
        let name = arg_str(n, 0).unwrap_or_default();
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
                        name: arg_str(s, 0).unwrap_or_default(),
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
            "summary" => summary = arg_str(child, 0).unwrap_or_default(),
            "claims-node" => node = arg_str(child, 0).unwrap_or_default(),
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

impl Module for DeclarativeModule {
    fn id(&self) -> ModuleId {
        self.id.clone()
    }
    fn node_name(&self) -> &str {
        &self.node_name
    }
    fn kind(&self) -> crate::ModuleKind {
        crate::ModuleKind::Declarative
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(
        &self,
        node: &kdl::KdlNode,
        _ctx: &mut crate::LowerCtx,
    ) -> Result<LowerOutput, LowerError> {
        let bindings = EmitTemplate::build_bindings(&self.schema, node);
        self.template.interpret(&bindings)
    }
    fn migration_notes(&self, from: &semver::Version, to: &semver::Version) -> Vec<String> {
        crate::notes_in_range(&self.migrations, from, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_web_service() -> DeclarativeModule {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../modules/web-service/knixl-module.kdl"
        ))
        .expect("read manifest");
        let doc = src.parse::<kdl::KdlDocument>().expect("parse manifest");
        DeclarativeModule::from_kdl(&doc, std::path::Path::new("web-service")).expect("from_kdl")
    }

    fn node(src: &str) -> kdl::KdlNode {
        src.parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone()
    }

    #[test]
    fn migration_notes_apply_only_to_crossed_steps() {
        use crate::Module;
        let m = load_web_service();
        let v = |s: &str| s.parse::<semver::Version>().unwrap();

        // Fresh install crossing both steps: both notes, in ascending order.
        let all = m.migration_notes(&v("1.0.0"), &v("1.2.0"));
        assert_eq!(all.len(), 2, "1.0.0 -> 1.2.0 crosses both steps: {all:?}");
        assert!(all[0].contains("enableACME"), "1.1.0 note first: {all:?}");
        assert!(
            all[1].contains("serverAliases"),
            "1.2.0 note second: {all:?}"
        );

        // Only the final step is crossed.
        let one = m.migration_notes(&v("1.1.0"), &v("1.2.0"));
        assert_eq!(one, vec![all[1].clone()]);

        // No move, no notes.
        assert!(m.migration_notes(&v("1.2.0"), &v("1.2.0")).is_empty());
    }

    fn lower(module: &DeclarativeModule, n: &kdl::KdlNode) -> LowerOutput {
        let reg = crate::Registry::new();
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(
            crate::Scope { host: "web".into() },
            &reg,
            &mut diags,
            vec![],
        );
        module.lower(n, &mut ctx).expect("lower")
    }

    fn path_str(a: &Assignment) -> String {
        a.path
            .0
            .iter()
            .map(|k| match k {
                AttrKey::Ident(s) => s.clone(),
                AttrKey::Quoted(s) => format!("\"{s}\""),
            })
            .collect::<Vec<_>>()
            .join(".")
    }

    fn find<'a>(out: &'a LowerOutput, path: &str) -> Option<&'a NixExpr> {
        out.units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| path_str(a) == path)
            .map(|a| &a.value)
    }

    #[test]
    fn web_service_expands_the_manifest() {
        let module = load_web_service();
        assert_eq!(module.node_name(), "web-service");

        let n = node("web-service \"example.com\" {\n    upstream \"http://127.0.0.1:3000\"\n    acme email=\"ops@example.com\"\n    hardened #true\n}");
        let out = lower(&module, &n);

        assert!(matches!(
            find(&out, "services.nginx.enable"),
            Some(NixExpr::Bool(true))
        ));

        // {host} interpolated into the path
        assert!(find(&out, "services.nginx.virtualHosts.\"example.com\".forceSSL").is_some());

        // {upstream} interpolated into the value; "/" is a quoted segment
        match find(
            &out,
            "services.nginx.virtualHosts.\"example.com\".locations.\"/\".proxyPass",
        ) {
            Some(NixExpr::Str(s)) => assert_eq!(s, "http://127.0.0.1:3000"),
            other => panic!("proxyPass = {other:?}"),
        }

        // {acme.email} resolves through the acme Scope
        match find(&out, "security.acme.certs.\"example.com\".email") {
            Some(NixExpr::Str(s)) => assert_eq!(s, "ops@example.com"),
            other => panic!("email = {other:?}"),
        }

        // when-flag hardened=true includes the indent-str block
        match find(
            &out,
            "services.nginx.virtualHosts.\"example.com\".locations.\"/\".extraConfig",
        ) {
            Some(NixExpr::IndentStr(s)) => assert!(s.contains("X-Frame-Options")),
            other => panic!("extraConfig = {other:?}"),
        }

        // collect with no aliases => empty list
        match find(
            &out,
            "services.nginx.virtualHosts.\"example.com\".serverAliases",
        ) {
            Some(NixExpr::List(items)) => assert!(items.is_empty()),
            other => panic!("serverAliases = {other:?}"),
        }
    }

    #[test]
    fn web_service_handles_repeats_and_a_disabled_flag() {
        let module = load_web_service();
        let n = node("web-service \"ex.com\" {\n    upstream \"u\"\n    acme email=\"e\"\n    alias \"a.com\"\n    alias \"b.com\"\n    location \"/api\" upstream=\"http://up:4000\"\n}");
        let out = lower(&module, &n);

        // collect folds both aliases in source order
        match find(&out, "services.nginx.virtualHosts.\"ex.com\".serverAliases") {
            Some(NixExpr::List(items)) => assert_eq!(items.len(), 2),
            other => panic!("serverAliases = {other:?}"),
        }

        // for-each binds loc.match into the path and loc.upstream into the value
        match find(
            &out,
            "services.nginx.virtualHosts.\"ex.com\".locations.\"/api\".proxyPass",
        ) {
            Some(NixExpr::Str(s)) => assert_eq!(s, "http://up:4000"),
            other => panic!("for-each proxyPass = {other:?}"),
        }

        // hardened absent => flag false => no extraConfig emitted
        assert!(find(
            &out,
            "services.nginx.virtualHosts.\"ex.com\".locations.\"/\".extraConfig"
        )
        .is_none());
    }

    #[test]
    fn dry_check_rejects_a_non_scalar_lookup_in_value_position() {
        // `acme` is a structured child (a Scope), so using {acme} as a value is an error
        // that must surface at load, not at generate.
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n        child \"acme\" {\n            prop \"email\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        set \"services.x.{host}\" \"{acme}\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(format!("{err}").contains("not a scalar"), "got: {err}");
    }

    #[test]
    fn dry_check_rejects_for_each_over_a_non_repeated_child() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"upstream\" type=\"string\"\n    }\n    emit {\n        for-each \"u\" in \"upstream\" {\n            set \"a.{u}\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("not a repeated child"),
            "got: {err}"
        );
    }

    #[test]
    fn when_config_stamps_an_interpolated_runtime_condition() {
        use knixl_ir::{Emit, Writer};
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n        arg \"svc\" type=\"string\" required=#true\n    }\n    emit {\n        when-config \"config.services.{svc}.enable\" {\n            set \"services.foo.{host}.enable\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
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
            w.into_string()
                .contains("lib.mkIf (config.services.postgresql.enable)"),
            "expected a lib.mkIf wrapper in the emitted text"
        );
    }

    #[test]
    fn a_set_outside_when_config_has_no_condition() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n    }\n    emit {\n        set \"services.foo.{host}.enable\" #true\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\""));
        assert!(out.units.iter().all(|u| u.assignment.condition.is_none()));
    }

    #[test]
    fn nested_when_config_and_combines() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n    }\n    emit {\n        when-config \"config.a.enable\" {\n            when-config \"config.b.enable\" {\n                set \"services.foo.{host}.enable\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond \"web\""));
        let a = out
            .units
            .iter()
            .map(|u| &u.assignment)
            .next()
            .expect("one assignment");
        match &a.condition {
            Some(NixExpr::Raw(r)) => assert_eq!(r.src, "(config.a.enable) && (config.b.enable)"),
            other => panic!("condition = {other:?}"),
        }
    }

    #[test]
    fn when_config_inside_for_each_interpolates_the_loop_var() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        child \"item\" type=\"string\" repeated=#true\n    }\n    emit {\n        for-each \"it\" in \"item\" {\n            when-config \"config.services.{it}.enable\" {\n                set \"p.{it}\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(
            &module,
            &node("cond {\n    item \"nginx\"\n    item \"sshd\"\n}"),
        );

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
        assert_eq!(
            cond_for("nginx").as_deref(),
            Some("config.services.nginx.enable")
        );
        assert_eq!(
            cond_for("sshd").as_deref(),
            Some("config.services.sshd.enable")
        );
    }

    #[test]
    fn when_flag_false_drops_a_when_config_body() {
        let manifest = "module name=\"cond\" version=\"0.1.0\" {\n    claims-node \"cond\"\n    schema {\n        child \"on\" type=\"bool\"\n    }\n    emit {\n        when-flag \"on\" {\n            when-config \"config.a.enable\" {\n                set \"p\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let module =
            DeclarativeModule::from_kdl(&doc, std::path::Path::new("cond")).expect("loads");
        let out = lower(&module, &node("cond")); // `on` absent => flag false
        assert!(
            out.units.is_empty(),
            "generation-time gate should drop the body"
        );
    }

    #[test]
    fn when_config_rejects_an_empty_condition() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n    }\n    emit {\n        when-config \"\" {\n            set \"p\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("non-empty condition"),
            "got: {err}"
        );
    }

    #[test]
    fn dry_check_rejects_a_non_scalar_lookup_in_a_condition() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"acme\" {\n            prop \"email\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        when-config \"config.{acme}.enable\" {\n            set \"p\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(format!("{err}").contains("not a scalar"), "got: {err}");
    }

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
            "/../../modules/web-service/knixl-module.kdl"
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
                        assert!(
                            matches!(map.get(&AttrKey::Ident("name".into())), Some(NixExpr::Str(s)) if s == "incusbr0")
                        );
                        assert!(
                            matches!(map.get(&AttrKey::Ident("type".into())), Some(NixExpr::Str(s)) if s == "bridge")
                        );
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
            Some(NixExpr::List(items)) => items
                .iter()
                .map(|e| match e {
                    NixExpr::AttrSet(m) => match m.get(&AttrKey::Ident("name".into())) {
                        Some(NixExpr::Str(s)) => s.clone(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                })
                .collect(),
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
            let a = out
                .units
                .iter()
                .map(|u| &u.assignment)
                .find(|a| path_str(a) == "virtualisation.incus.preseed.networks")
                .unwrap()
                .clone();
            let mut w = Writer::new();
            a.emit(&mut w);
            w.into_string()
        };
        let text = render();
        assert!(text.contains("= ["), "renders a list: {text}");
        assert!(
            text.contains("name = \"x\""),
            "renders the element attr: {text}"
        );
        assert!(
            text.contains("\"ipv4.address\""),
            "renders the quoted nested key: {text}"
        );
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
        let out = lower(
            &m,
            &node("net {\n    network name=\"on\" managed=#true\n    network name=\"off\"\n}"),
        );
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
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad"))
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("not a repeated child"),
            "got: {err}"
        );
    }

    #[test]
    fn list_rejects_a_duplicate_inner_attr_path() {
        let manifest = "module name=\"dup\" version=\"0.1.0\" {\n    claims-node \"dup\"\n    schema {\n        child \"x\" repeated=#true {\n            prop \"a\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        list \"p.q\" from \"x\" {\n            set \"name\" \"{x.a}\"\n            set \"name\" \"{x.a}\"\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("dup"))
            .expect("loads (dup is a generate-time error)");
        let reg = crate::Registry::new();
        let mut diags = Vec::new();
        let mut ctx =
            crate::LowerCtx::new(crate::Scope { host: "h".into() }, &reg, &mut diags, vec![]);
        let err = m
            .lower(&node("dup {\n    x a=\"1\"\n}"), &mut ctx)
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("duplicate")
                || format!("{err}").contains("both a value and a set"),
            "got: {err}"
        );
    }

    #[test]
    fn a_missing_required_bool_subfield_still_errors() {
        // An optional bool subfield defaults to false, but a required one left absent must
        // still error at generate (no silent fallback).
        let manifest = "module name=\"req\" version=\"0.1.0\" {\n    claims-node \"req\"\n    schema {\n        child \"x\" repeated=#true {\n            prop \"name\" type=\"string\" required=#true\n            prop \"on\" type=\"bool\" required=#true\n        }\n    }\n    emit {\n        list \"p.q\" from \"x\" {\n            set \"name\" \"{x.name}\"\n            when-flag \"x.on\" {\n                set \"on\" #true\n            }\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let m = DeclarativeModule::from_kdl(&doc, std::path::Path::new("req")).expect("loads");
        let reg = crate::Registry::new();
        let mut diags = Vec::new();
        let mut ctx =
            crate::LowerCtx::new(crate::Scope { host: "h".into() }, &reg, &mut diags, vec![]);
        // `on` is required but omitted on this instance: its `when-flag` lookup must error.
        let err = m
            .lower(&node("req {\n    x name=\"a\"\n}"), &mut ctx)
            .err()
            .unwrap();
        assert!(
            format!("{err}").contains("on"),
            "expected a missing-field error for `on`: {err}"
        );
    }
}
