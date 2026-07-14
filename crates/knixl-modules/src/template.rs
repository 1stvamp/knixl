//! EmitTemplate: the substitution grammar for declarative modules. Parsed once from a
//! module's `emit { }` block, interpreted per-node against a bindings tree built from the
//! validated input. Three statement forms only (set, when-flag, for-each). SPEC-GRADE SKETCH.

use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use knixl_kdl::children_named;
use crate::{Bucket, Child, Field, LowerError, LowerOutput, NodeSchema, Unit, ValueTy};

pub struct EmitTemplate { pub stmts: Vec<Stmt> }

pub enum Stmt {
    Set { path: PathTemplate, value: ValueTemplate },
    WhenFlag { flag: String, body: Vec<Stmt> },               // generation-time gate
    ForEach { var: String, source: String, body: Vec<Stmt> }, // binds <var> per item
}

pub struct PathTemplate(pub Vec<SegmentTemplate>);

pub enum SegmentTemplate {
    Ident(String),      // services, nginx  -> AttrKey::Ident
    QuotedLit(String),  // "/"              -> AttrKey::Quoted, may interpolate
    Interp(Lookup),     // {host}           -> AttrKey::Quoted (dynamic name)
}

pub enum ValueTemplate {
    Bool(bool),
    Int(i128),
    Str(Vec<StrPart>),        // "{upstream}"
    IndentStr(Vec<StrPart>),  // (indent-str #""" ... """#)
    Collect(String),          // (collect "alias") -> List of that child's first arg
}

pub enum StrPart { Lit(String), Interp(Lookup) }

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
pub enum Scalar { Bool(bool), Int(i128), Str(String) }
impl Scalar {
    pub fn as_str(&self) -> Result<String, LowerError> {
        match self { Scalar::Str(s) => Ok(s.clone()), _ => Err(LowerError::Other("expected string".into())) }
    }
}

pub struct Bindings(pub std::collections::BTreeMap<String, Binding>);

pub struct LoopScopes(Vec<(String, Binding)>);
impl LoopScopes {
    pub fn new() -> Self { Self(Vec::new()) }
    pub fn push(&mut self, var: &str, item: &Binding) { self.0.push((var.to_string(), item.clone())); }
    pub fn pop(&mut self) { self.0.pop(); }
    /// Most-recently pushed binding for `var`, so an inner loop shadows an outer one.
    fn get(&self, var: &str) -> Option<&Binding> {
        self.0.iter().rev().find(|(name, _)| name == var).map(|(_, b)| b)
    }
}
impl Default for LoopScopes { fn default() -> Self { Self::new() } }

impl Lookup {
    fn resolve<'a>(&self, b: &'a Bindings, loops: &'a LoopScopes) -> Result<&'a Scalar, LowerError> {
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
            _ => Err(LowerError::Other(format!("`{}` resolves to a non-scalar", self.0.join(".")))),
        }
    }
}

// ---- interpret ----

impl EmitTemplate {
    /// Built once from the schema+node at lower() time (walks the SCHEMA so resolution is total).
    pub fn build_bindings(schema: &NodeSchema, node: &kdl::KdlNode) -> Bindings {
        let mut map = std::collections::BTreeMap::new();

        let positional: Vec<&kdl::KdlEntry> =
            node.entries().iter().filter(|e| e.name().is_none()).collect();
        for (i, field) in schema.args.iter().enumerate() {
            if let Some(entry) = positional.get(i) {
                map.insert(field.name.clone(), Binding::Scalar(scalar_from(entry.value())));
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
                let list = matching.iter().map(|c| binding_for_child(child, c)).collect();
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
        self.run(&self.stmts, b, &mut loops, &mut units)?;
        Ok(LowerOutput::units(units))
    }

    // self only recurses today; kept as a method so template state stays reachable
    // once bind/interpret are fleshed out.
    #[allow(clippy::only_used_in_recursion)]
    fn run(&self, stmts: &[Stmt], b: &Bindings, loops: &mut LoopScopes, out: &mut Vec<Unit>)
        -> Result<(), LowerError>
    {
        for st in stmts {
            match st {
                Stmt::Set { path, value } => {
                    let a = Assignment {
                        path: path.interpret(b, loops)?,
                        value: value.interpret(b, loops)?, // Collect => NixExpr::List
                        priority: None, condition: None, doc: None,
                    };
                    out.push(Unit { bucket: Bucket::Default, assignment: a });
                }
                Stmt::WhenFlag { flag, body } => {
                    if resolve_flag(flag, b, loops)? { self.run(body, b, loops, out)?; }
                }
                Stmt::ForEach { var, source, body } => {
                    for item in resolve_list(source, b)? { // source order => stable
                        loops.push(var, item);
                        self.run(body, b, loops, out)?;
                        loops.pop();
                    }
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
                SegmentTemplate::Ident(w)     => AttrKey::Ident(w.clone()),
                SegmentTemplate::QuotedLit(t) => AttrKey::Quoted(interp_str(t, b, loops)?),
                SegmentTemplate::Interp(lk)   => AttrKey::Quoted(lk.resolve(b, loops)?.as_str()?),
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
                        _ => return Err(LowerError::Other(format!("collect `{child}` expects scalar items"))),
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
    match Lookup(vec![flag.to_string()]).resolve(b, loops)? {
        Scalar::Bool(v) => Ok(*v),
        _ => Err(LowerError::Other(format!("`{flag}` is not a boolean flag"))),
    }
}

fn resolve_list<'a>(source: &str, b: &'a Bindings) -> Result<&'a [Binding], LowerError> {
    match b.0.get(source) {
        Some(Binding::List(items)) => Ok(items),
        Some(_) => Err(LowerError::Other(format!("`{source}` is not a repeated child"))),
        None => Ok(&[]), // absent repeated child => no items
    }
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
    let positional: Vec<&kdl::KdlEntry> =
        node.entries().iter().filter(|e| e.name().is_none()).collect();
    for (i, f) in child.args.iter().enumerate() {
        if let Some(e) = positional.get(i) {
            map.insert(f.name.clone(), Binding::Scalar(scalar_from(e.value())));
        }
    }
    for f in &child.props {
        if let Some(v) = node.get(f.name.as_str()) {
            map.insert(f.name.clone(), Binding::Scalar(scalar_from(v)));
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
            '"' => { in_quote = !in_quote; cur.push(c); }
            '{' if !in_quote => { depth += 1; cur.push(c); }
            '}' if !in_quote => { depth = depth.saturating_sub(1); cur.push(c); }
            '.' if depth == 0 && !in_quote => segs.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() { segs.push(cur); }
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
        if open > 0 { parts.push(StrPart::Lit(rest[..open].to_string())); }
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let lookup = &after[..close];
                parts.push(StrPart::Interp(Lookup(lookup.split('.').map(str::to_string).collect())));
                rest = &after[close + 1..];
            }
            None => {
                parts.push(StrPart::Lit(rest.to_string()));
                return parts;
            }
        }
    }
    if !rest.is_empty() { parts.push(StrPart::Lit(rest.to_string())); }
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
    Ok(NodeSchema { summary: String::new(), args, props, children, open_children: false })
}

fn parse_field(n: &kdl::KdlNode) -> Result<Field, LowerError> {
    let name = arg_str(n, 0).ok_or_else(|| LowerError::Other("schema field missing name".into()))?;
    Ok(Field {
        name,
        ty: ty_from(prop_str(n, "type").as_deref()),
        required: prop_bool(n, "required").unwrap_or(false),
        doc: prop_str(n, "doc").unwrap_or_default(),
    })
}

fn parse_child(n: &kdl::KdlNode) -> Result<Child, LowerError> {
    let name = arg_str(n, 0).ok_or_else(|| LowerError::Other("schema child missing name".into()))?;
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
                    return Err(LowerError::Other(format!("unexpected `{other}` in structured child")))
                }
            }
        }
        Ok(Child { name, ty: ValueTy::Node, required, repeated, delegate: false, doc, args, props })
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
            Ok(Stmt::Set { path: parse_path(path), value: parse_value(value)? })
        }
        "when-flag" => {
            let flag = arg_str(n, 0).ok_or_else(|| LowerError::Other("`when-flag` missing flag".into()))?;
            Ok(Stmt::WhenFlag { flag, body: parse_stmts(n.children())? })
        }
        "for-each" => {
            // `for-each "loc" in "location"`: the bare `in` is noise; take first + last.
            let args: Vec<String> = n
                .entries()
                .iter()
                .filter(|e| e.name().is_none())
                .filter_map(|e| e.value().as_string().map(str::to_string))
                .collect();
            let var = args.first().cloned().ok_or_else(|| LowerError::Other("`for-each` missing var".into()))?;
            let source = args.last().cloned().ok_or_else(|| LowerError::Other("`for-each` missing source".into()))?;
            Ok(Stmt::ForEach { var, source, body: parse_stmts(n.children())? })
        }
        other => Err(LowerError::Other(format!("unknown emit statement `{other}`"))),
    }
}

fn parse_path(s: &str) -> PathTemplate {
    PathTemplate(split_path(s).iter().map(|seg| classify_segment(seg)).collect())
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
        Some(other) => Err(LowerError::Other(format!("unknown value annotation `{other}`"))),
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
    node.get(key).and_then(|v| v.as_string()).map(str::to_string)
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
        m.insert(c.name.clone(), if c.repeated { Shape::List(Box::new(base)) } else { base });
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
        Err(LowerError::Other(format!("template does not type-check: {}", errors.join("; "))))
    }
}

fn lookup_shape<'a>(
    segs: &[String],
    shapes: &'a ShapeMap,
    loops: &[(&'a str, &'a Shape)],
) -> Result<&'a Shape, String> {
    let (first, rest) = segs.split_first().ok_or_else(|| "empty lookup".to_string())?;
    let mut cur = loops
        .iter()
        .rev()
        .find(|(n, _)| n == first)
        .map(|(_, s)| *s)
        .or_else(|| shapes.get(first))
        .ok_or_else(|| format!("unknown binding `{first}`"))?;
    for seg in rest {
        cur = match cur {
            Shape::Scope(m) => m.get(seg).ok_or_else(|| format!("`{seg}` is not a field here"))?,
            _ => return Err(format!("`{first}` is not a scope")),
        };
    }
    Ok(cur)
}

fn expect_scalar(segs: &[String], shapes: &ShapeMap, loops: &[(&str, &Shape)], errors: &mut Vec<String>) {
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
                        SegmentTemplate::QuotedLit(t) => check_str_lookups(t, shapes, loops, errors),
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
                            Ok(_) => errors.push(format!("collect `{child}` is not a repeated child")),
                            Err(e) => errors.push(e),
                        }
                    }
                    ValueTemplate::Bool(_) | ValueTemplate::Int(_) => {}
                }
            }
            Stmt::WhenFlag { flag, body } => {
                expect_scalar(std::slice::from_ref(flag), shapes, loops, errors);
                check_stmts(body, shapes, loops, errors);
            }
            Stmt::ForEach { var, source, body } => {
                match lookup_shape(std::slice::from_ref(source), shapes, loops) {
                    Ok(Shape::List(inner)) => {
                        loops.push((var.as_str(), inner));
                        check_stmts(body, shapes, loops, errors);
                        loops.pop();
                    }
                    Ok(_) => errors.push(format!("for-each source `{source}` is not a repeated child")),
                    Err(e) => errors.push(e),
                }
            }
        }
    }
}

fn check_str_lookups(raw: &str, shapes: &ShapeMap, loops: &[(&str, &Shape)], errors: &mut Vec<String>) {
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
        let version = version_str
            .parse()
            .map_err(|e| LowerError::Other(format!("{}: bad version `{version_str}`: {e}", where_())))?;

        let body = module
            .children()
            .ok_or_else(|| LowerError::Other(format!("{}: empty module", where_())))?;

        let mut summary = String::new();
        let mut node_name = None;
        let mut schema = None;
        let mut template = None;
        for child in body.nodes() {
            match child.name().value() {
                "summary" => summary = arg_str(child, 0).unwrap_or_default(),
                "claims-node" => node_name = arg_str(child, 0),
                "schema" => schema = Some(parse_schema_block(child)?),
                "emit" => template = Some(EmitTemplate { stmts: parse_stmts(child.children())? }),
                other => {
                    return Err(LowerError::Other(format!("{}: unexpected `{other}` in module", where_())))
                }
            }
        }

        let node_name = node_name
            .ok_or_else(|| LowerError::Other(format!("{}: module missing `claims-node`", where_())))?;
        let mut schema =
            schema.ok_or_else(|| LowerError::Other(format!("{}: module missing `schema`", where_())))?;
        schema.summary = summary;
        let template =
            template.ok_or_else(|| LowerError::Other(format!("{}: module missing `emit`", where_())))?;

        dry_check(&schema, &template)
            .map_err(|e| LowerError::Other(format!("{}: {e}", where_())))?;

        Ok(DeclarativeModule { id: ModuleId { name, version }, node_name, schema, template })
    }
}

impl Module for DeclarativeModule {
    fn id(&self) -> ModuleId { self.id.clone() }
    fn node_name(&self) -> &str { &self.node_name }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &kdl::KdlNode, _ctx: &mut crate::LowerCtx) -> Result<LowerOutput, LowerError> {
        let bindings = EmitTemplate::build_bindings(&self.schema, node);
        self.template.interpret(&bindings)
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
        src.parse::<kdl::KdlDocument>().unwrap().nodes().first().unwrap().clone()
    }

    fn lower(module: &DeclarativeModule, n: &kdl::KdlNode) -> LowerOutput {
        let reg = crate::Registry::new();
        let mut diags = Vec::new();
        let mut ctx = crate::LowerCtx::new(crate::Scope { host: "web".into() }, &reg, &mut diags);
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
        out.units.iter().map(|u| &u.assignment).find(|a| path_str(a) == path).map(|a| &a.value)
    }

    #[test]
    fn web_service_expands_the_manifest() {
        let module = load_web_service();
        assert_eq!(module.node_name(), "web-service");

        let n = node("web-service \"example.com\" {\n    upstream \"http://127.0.0.1:3000\"\n    acme email=\"ops@example.com\"\n    hardened #true\n}");
        let out = lower(&module, &n);

        assert!(matches!(find(&out, "services.nginx.enable"), Some(NixExpr::Bool(true))));

        // {host} interpolated into the path
        assert!(find(&out, "services.nginx.virtualHosts.\"example.com\".forceSSL").is_some());

        // {upstream} interpolated into the value; "/" is a quoted segment
        match find(&out, "services.nginx.virtualHosts.\"example.com\".locations.\"/\".proxyPass") {
            Some(NixExpr::Str(s)) => assert_eq!(s, "http://127.0.0.1:3000"),
            other => panic!("proxyPass = {other:?}"),
        }

        // {acme.email} resolves through the acme Scope
        match find(&out, "security.acme.certs.\"example.com\".email") {
            Some(NixExpr::Str(s)) => assert_eq!(s, "ops@example.com"),
            other => panic!("email = {other:?}"),
        }

        // when-flag hardened=true includes the indent-str block
        match find(&out, "services.nginx.virtualHosts.\"example.com\".locations.\"/\".extraConfig") {
            Some(NixExpr::IndentStr(s)) => assert!(s.contains("X-Frame-Options")),
            other => panic!("extraConfig = {other:?}"),
        }

        // collect with no aliases => empty list
        match find(&out, "services.nginx.virtualHosts.\"example.com\".serverAliases") {
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
        match find(&out, "services.nginx.virtualHosts.\"ex.com\".locations.\"/api\".proxyPass") {
            Some(NixExpr::Str(s)) => assert_eq!(s, "http://up:4000"),
            other => panic!("for-each proxyPass = {other:?}"),
        }

        // hardened absent => flag false => no extraConfig emitted
        assert!(find(&out, "services.nginx.virtualHosts.\"ex.com\".locations.\"/\".extraConfig").is_none());
    }

    #[test]
    fn dry_check_rejects_a_non_scalar_lookup_in_value_position() {
        // `acme` is a structured child (a Scope), so using {acme} as a value is an error
        // that must surface at load, not at generate.
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        arg \"host\" type=\"string\" required=#true\n        child \"acme\" {\n            prop \"email\" type=\"string\" required=#true\n        }\n    }\n    emit {\n        set \"services.x.{host}\" \"{acme}\"\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad")).err().unwrap();
        assert!(format!("{err}").contains("not a scalar"), "got: {err}");
    }

    #[test]
    fn dry_check_rejects_for_each_over_a_non_repeated_child() {
        let manifest = "module name=\"bad\" version=\"0.1.0\" {\n    claims-node \"bad\"\n    schema {\n        child \"upstream\" type=\"string\"\n    }\n    emit {\n        for-each \"u\" in \"upstream\" {\n            set \"a.{u}\" #true\n        }\n    }\n}";
        let doc = manifest.parse::<kdl::KdlDocument>().unwrap();
        let err = DeclarativeModule::from_kdl(&doc, std::path::Path::new("bad")).err().unwrap();
        assert!(format!("{err}").contains("not a repeated child"), "got: {err}");
    }
}
