//! EmitTemplate: the substitution grammar for declarative modules. See
//! docs/04-template-grammar.md for the grammar reference.

use crate::{Bucket, Child, LowerError, LowerOutput, NodeSchema, Unit, ValueTy};
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr, RawNix};
use knixl_kdl::{children_named, first_arg_str};

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
        optional: bool, // `list-opt`: drop the assignment when there are no items
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
    Str(Vec<StrPart>),        // "{upstream}"
    IndentStr(Vec<StrPart>),  // (indent-str #""" ... """#)
    Collect(String),          // (collect "alias") -> List of that child's first arg
    CollectOpt(String), // (collect-opt "x") -> like Collect, but the set is dropped when empty
    Secret(Vec<StrPart>), // (secret)"name" -> config.<backend>.secrets."name".path
    Scalar(Vec<StrPart>), // (scalar)"{x}" -> the bound scalar emitted as its native bool/int/string
    List(Vec<ValueTemplate>), // (list)"a" "b" -> [ "a" "b" ], each a plain bool/int/string
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
    /// Render this scalar as text for interpolation into a string or a dynamic attr
    /// segment (e.g. `{cap.bytes}` where `bytes` is an int arg).
    pub fn as_str(&self) -> Result<String, LowerError> {
        match self {
            Scalar::Str(s) => Ok(s.clone()),
            Scalar::Int(i) => Ok(i.to_string()),
            Scalar::Bool(b) => Ok(b.to_string()),
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
    fn build_bindings(schema: &NodeSchema, node: &kdl::KdlNode) -> Bindings {
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

    fn interpret(
        &self,
        b: &Bindings,
        backend: crate::SecretsBackend,
    ) -> Result<LowerOutput, LowerError> {
        let mut units = Vec::new();
        let mut loops = LoopScopes::new();
        self.run(&self.stmts, b, &mut loops, None, &mut units, backend)?;
        Ok(LowerOutput::units(units))
    }

    // self is only used for recursion here, which is why the lint below is allowed.
    #[allow(clippy::only_used_in_recursion)]
    fn run(
        &self,
        stmts: &[Stmt],
        b: &Bindings,
        loops: &mut LoopScopes,
        cond: Option<&str>,
        out: &mut Vec<Unit>,
        backend: crate::SecretsBackend,
    ) -> Result<(), LowerError> {
        for st in stmts {
            match st {
                Stmt::Set { path, value } => {
                    let v = value.interpret(b, loops, backend)?;
                    // An optional collect over an absent/empty child emits nothing, so it does not
                    // clobber a NixOS default (e.g. services.openssh.ports defaults to [ 22 ]).
                    if matches!(value, ValueTemplate::CollectOpt(_))
                        && matches!(&v, NixExpr::List(items) if items.is_empty())
                    {
                        continue;
                    }
                    let a = Assignment {
                        path: path.interpret(b, loops)?,
                        value: v,
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
                        self.run(body, b, loops, cond, out, backend)?;
                    }
                }
                Stmt::WhenConfig { cond: parts, body } => {
                    let inner = interp_parts(parts, b, loops)?;
                    // Nested conditions conjoin: `lib.mkIf ((A) && (B)) ..`.
                    let combined = match cond {
                        Some(outer) => format!("({outer}) && ({inner})"),
                        None => inner,
                    };
                    self.run(body, b, loops, Some(&combined), out, backend)?;
                }
                Stmt::ForEach { var, source, body } => {
                    for item in resolve_list(source, b)? {
                        // source order => stable
                        loops.push(var, item);
                        self.run(body, b, loops, cond, out, backend)?;
                        loops.pop();
                    }
                }
                Stmt::List {
                    path,
                    source,
                    body,
                    optional,
                } => {
                    let mut elems = Vec::new();
                    for item in resolve_list(source, b)? {
                        loops.push(source, item); // the child name is the loop binding
                        let mut elem_units = Vec::new();
                        let res = self.run(body, b, loops, None, &mut elem_units, backend);
                        loops.pop();
                        res?;
                        elems.push(fold_units_into_attrset(elem_units)?);
                    }
                    if *optional && elems.is_empty() {
                        continue;
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
    fn interpret(
        &self,
        b: &Bindings,
        loops: &LoopScopes,
        backend: crate::SecretsBackend,
    ) -> Result<NixExpr, LowerError> {
        Ok(match self {
            ValueTemplate::List(items) => NixExpr::List(
                items
                    .iter()
                    .map(|v| v.interpret(b, loops, backend))
                    .collect::<Result<_, _>>()?,
            ),
            ValueTemplate::Bool(v) => NixExpr::Bool(*v),
            ValueTemplate::Int(v) => NixExpr::Int(*v),
            ValueTemplate::Str(parts) => NixExpr::Str(interp_parts(parts, b, loops)?),
            ValueTemplate::IndentStr(parts) => NixExpr::IndentStr(interp_parts(parts, b, loops)?),
            ValueTemplate::Collect(child) | ValueTemplate::CollectOpt(child) => {
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
            ValueTemplate::Scalar(parts) => match parts.as_slice() {
                [StrPart::Interp(lk)] => scalar_to_expr(lk.resolve(b, loops)?),
                _ => {
                    return Err(LowerError::Other(
                        "(scalar) needs exactly one {interpolation} and no surrounding text".into(),
                    ))
                }
            },
            ValueTemplate::Secret(parts) => {
                let name = interp_parts(parts, b, loops)?;
                let prefix = match backend {
                    crate::SecretsBackend::SopsNix => "sops.secrets",
                    crate::SecretsBackend::Agenix => "age.secrets",
                };
                let esc = name
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace("${", "\\${");
                NixExpr::Raw(RawNix {
                    src: format!("config.{prefix}.\"{esc}\".path"),
                    span: None,
                })
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
            // `(list)` makes the value and every positional after it the list's elements.
            let value = if value.ty().map(|t| t.value()) == Some("list") {
                ValueTemplate::List(
                    positional[1..]
                        .iter()
                        .map(|e| plain_value(e.value()))
                        .collect::<Result<_, _>>()?,
                )
            } else {
                parse_value(value)?
            };
            Ok(Stmt::Set {
                path: parse_path(path),
                value,
            })
        }
        "when-flag" => {
            let flag = first_arg_str(n)
                .ok_or_else(|| LowerError::Other("`when-flag` missing flag".into()))?;
            Ok(Stmt::WhenFlag {
                flag,
                body: parse_stmts(n.children())?,
            })
        }
        "when-config" => {
            // An empty or whitespace-only condition would emit `lib.mkIf () <value>`, invalid
            // Nix. Reject it at load, where the rest of the grammar errors live.
            let cond = first_arg_str(n)
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
        kw @ ("list" | "list-opt") => {
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
                .ok_or_else(|| LowerError::Other(format!("`{kw}` missing path")))?;
            let source = args
                .last()
                .cloned()
                .ok_or_else(|| LowerError::Other(format!("`{kw}` missing source")))?;
            Ok(Stmt::List {
                path: parse_path(&path),
                source,
                body: parse_stmts(n.children())?,
                optional: kw == "list-opt",
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
        Some("collect-opt") => {
            let child = entry
                .value()
                .as_string()
                .ok_or_else(|| LowerError::Other("collect-opt needs a child name".into()))?;
            Ok(ValueTemplate::CollectOpt(child.to_string()))
        }
        Some("indent-str") => {
            let s = entry
                .value()
                .as_string()
                .ok_or_else(|| LowerError::Other("indent-str needs a string".into()))?;
            Ok(ValueTemplate::IndentStr(parse_str_parts(s)))
        }
        Some("secret") => {
            let s = entry
                .value()
                .as_string()
                .ok_or_else(|| LowerError::Other("secret needs a name string".into()))?;
            Ok(ValueTemplate::Secret(parse_str_parts(s)))
        }
        Some("scalar") => {
            let s = entry.value().as_string().ok_or_else(|| {
                LowerError::Other("scalar needs an {interpolation} string".into())
            })?;
            Ok(ValueTemplate::Scalar(parse_str_parts(s)))
        }
        Some(other) => Err(LowerError::Other(format!(
            "unknown value annotation `{other}`"
        ))),
        None => plain_value(entry.value()),
    }
}

fn plain_value(v: &kdl::KdlValue) -> Result<ValueTemplate, LowerError> {
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
                    ValueTemplate::Str(parts)
                    | ValueTemplate::IndentStr(parts)
                    | ValueTemplate::Secret(parts)
                    | ValueTemplate::Scalar(parts) => {
                        for part in parts {
                            if let StrPart::Interp(lk) = part {
                                expect_scalar(&lk.0, shapes, loops, errors);
                            }
                        }
                    }
                    ValueTemplate::Collect(child) | ValueTemplate::CollectOpt(child) => {
                        match lookup_shape(std::slice::from_ref(child), shapes, loops) {
                            Ok(Shape::List(_)) => {}
                            Ok(_) => {
                                errors.push(format!("collect `{child}` is not a repeated child"))
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                    ValueTemplate::List(items) => {
                        for item in items {
                            if let ValueTemplate::Str(parts) = item {
                                for part in parts {
                                    if let StrPart::Interp(lk) = part {
                                        expect_scalar(&lk.0, shapes, loops, errors);
                                    }
                                }
                            }
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
            Stmt::List {
                path, source, body, ..
            } => {
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

impl EmitTemplate {
    pub fn parse(body: Option<&kdl::KdlDocument>) -> Result<Self, LowerError> {
        Ok(EmitTemplate {
            stmts: parse_stmts(body)?,
        })
    }

    /// Reject a lookup the schema can't satisfy, at load rather than at generate (docs/04).
    pub fn check(&self, schema: &NodeSchema) -> Result<(), LowerError> {
        dry_check(schema, self)
    }

    pub fn lower(
        &self,
        schema: &NodeSchema,
        node: &kdl::KdlNode,
        backend: crate::SecretsBackend,
    ) -> Result<LowerOutput, LowerError> {
        self.interpret(&Self::build_bindings(schema, node), backend)
    }
}
