//! EmitTemplate: the substitution grammar for declarative modules. Parsed once from a
//! module's `emit { }` block, interpreted per-node against a bindings tree built from the
//! validated input. Three statement forms only (set, when-flag, for-each). SPEC-GRADE SKETCH.

use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use crate::{Bucket, LowerError, LowerOutput, NodeSchema, Unit};

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

pub enum Binding {
    Scalar(Scalar),
    Scope(std::collections::BTreeMap<String, Binding>),
    List(Vec<Binding>),
}
pub enum Scalar { Bool(bool), Int(i128), Str(String) }
impl Scalar {
    pub fn as_str(&self) -> Result<String, LowerError> {
        match self { Scalar::Str(s) => Ok(s.clone()), _ => Err(LowerError::Other("expected string".into())) }
    }
}

pub struct Bindings(pub std::collections::BTreeMap<String, Binding>);

/// Loop variables, checked BEFORE top-level bindings (so for-each shadows; loader warns).
pub struct LoopScopes(Vec<(String, /* item */ usize)>);
impl LoopScopes {
    pub fn new() -> Self { Self(Vec::new()) }
    pub fn push(&mut self, _var: &str, _item: &Binding) { todo!() }
    pub fn pop(&mut self) { self.0.pop(); }
}
impl Default for LoopScopes { fn default() -> Self { Self::new() } }

impl Lookup {
    /// First segment checks loop scopes, then top-level; rest walk Scope maps.
    /// Resolving to a non-Scalar in value position is a template AUTHORING error,
    /// caught at module-load time by a dry type-pass, not here.
    fn resolve<'a>(&self, _b: &'a Bindings, _loops: &'a LoopScopes) -> Result<&'a Scalar, LowerError> {
        todo!("dotted resolve")
    }
}

// ---- interpret ----

impl EmitTemplate {
    /// Built once from the schema+node at lower() time (walks the SCHEMA so resolution is total).
    pub fn build_bindings(_schema: &NodeSchema, _node: &kdl::KdlNode) -> Bindings {
        todo!("arg->Scalar, prop->Scalar, flag->Bool, scalar child->Scalar, \
               structured child->Scope, repeated child->List (source order)")
    }

    pub fn bind(&self, _node: &kdl::KdlNode) -> Result<Bindings, LowerError> { todo!() }

    pub fn interpret(&self, b: &Bindings) -> Result<LowerOutput, LowerError> {
        let mut units = Vec::new();
        let mut loops = LoopScopes::new();
        self.run(&self.stmts, b, &mut loops, &mut units)?;
        Ok(LowerOutput { units })
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
    fn interpret(&self, _b: &Bindings, _loops: &LoopScopes) -> Result<NixExpr, LowerError> {
        todo!("Bool/Int direct; Str/IndentStr join parts; Collect -> NixExpr::List")
    }
}

fn interp_str(_t: &str, _b: &Bindings, _loops: &LoopScopes) -> Result<String, LowerError> { todo!() }
fn resolve_flag(_flag: &str, _b: &Bindings, _loops: &LoopScopes) -> Result<bool, LowerError> { todo!() }
fn resolve_list<'a>(_source: &str, _b: &'a Bindings) -> Result<&'a [Binding], LowerError> { todo!() }

// ---- the declarative module: one Module impl carrying every KDL-defined module ----

use crate::{Module, ModuleId};

pub struct DeclarativeModule {
    id: ModuleId,
    node_name: String,
    schema: NodeSchema,
    template: EmitTemplate,
}

impl DeclarativeModule {
    pub fn from_kdl(_doc: &kdl::KdlDocument, _source: &std::path::Path)
        -> Result<Self, LowerError> {
        todo!("parse module/schema/emit; dry-check template against schema")
    }
}

impl Module for DeclarativeModule {
    fn id(&self) -> ModuleId { self.id.clone() }
    fn node_name(&self) -> &str { &self.node_name }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &kdl::KdlNode, _ctx: &mut crate::LowerCtx) -> Result<LowerOutput, LowerError> {
        let bindings = self.template.bind(node)?;
        self.template.interpret(&bindings)
    }
}
