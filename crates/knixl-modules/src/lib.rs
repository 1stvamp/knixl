//! The Module trait and everything it needs. A built-in Rust module and a runtime-loaded
//! KDL module are indistinguishable to the generator: the declarative loader is itself
//! one Module impl. SPEC-GRADE SKETCH.

pub mod registry;
pub mod template;
pub mod builtin;

use kdl::KdlNode;
use knixl_ir::Assignment;
use miette::SourceSpan;
use semver::Version;

pub use registry::{Registry, RegistryError};

#[derive(Clone)]
pub struct ModuleId { pub name: String, pub version: Version }

pub trait Module: Send + Sync {
    fn id(&self) -> ModuleId;
    /// The KDL node name this module claims (e.g. "postgres"). The generator dispatches by it.
    fn node_name(&self) -> &str;
    /// Structured description of accepted args/props/children. Validates input AND drives `knixl doc`.
    fn schema(&self) -> &NodeSchema;
    /// Lower a node into bucketed assignments. The node is schema-valid here (the generator
    /// runs schema().validate() first), so lower() may assume presence.
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError>;
}

// ---- schema: validates INPUT shape (distinct from the oracle, which validates OUTPUT) ----

pub struct NodeSchema {
    pub summary: String,
    pub args: Vec<Field>,      // positional args on the node itself
    pub props: Vec<Field>,     // key=value on the node
    pub children: Vec<Child>,  // nested nodes
}

pub struct Field { pub name: String, pub ty: ValueTy, pub required: bool, pub doc: String }

pub struct Child {
    pub name: String,
    pub ty: ValueTy,
    pub required: bool,
    pub repeated: bool,   // `database "app"; database "metrics"` => repeated
    pub delegate: bool,   // true => another module's node, dispatched, not read here
    pub doc: String,
}

/// KDL-side INPUT types. Not oracle NixType (which is OUTPUT option types).
pub enum ValueTy { Bool, Int, Str, Enum(Vec<String>), Node }

impl NodeSchema {
    /// Missing-required, unknown-field, arity, value-type errors, each with a KDL span.
    pub fn validate(&self, _node: &KdlNode) -> Result<(), Vec<Diagnostic>> {
        todo!("shape validation")
    }
}

// ---- lowering ----

// registry + diags are consumed once lower_children/lint are implemented (Phase 1).
#[allow(dead_code)]
pub struct LowerCtx<'a> {
    scope: Scope,
    registry: &'a Registry,
    diags: &'a mut Vec<Diagnostic>,
}

pub struct Scope { pub host: String }

impl<'a> LowerCtx<'a> {
    pub fn new(scope: Scope, registry: &'a Registry, diags: &'a mut Vec<Diagnostic>) -> Self {
        Self { scope, registry, diags }
    }

    pub fn scope(&self) -> &Scope { &self.scope }

    /// Dispatch each child NOT in `consumed` to its registered module, collect outputs.
    /// Only container modules (host) call this; leaf modules read their own subtree.
    pub fn lower_children(&mut self, _node: &KdlNode, _consumed: &[&str])
        -> Result<Vec<LowerOutput>, LowerError> {
        todo!("walk children, registry.get(name), recurse; unknown node => diag")
    }

    pub fn lint(&mut self, _span: SourceSpan, _msg: impl Into<String>) {
        todo!("push a non-fatal diagnostic")
    }
}

pub struct LowerOutput { pub units: Vec<Unit> }
pub struct Unit { pub bucket: Bucket, pub assignment: Assignment }

/// A module says "main file" or "a named side-file"; the generator resolves the path.
pub enum Bucket { Default, Named(String) }

#[derive(Debug)]
pub struct Diagnostic { pub span: Option<SourceSpan>, pub message: String }

#[derive(Debug, thiserror::Error)]
pub enum LowerError {
    #[error("missing required input: {0}")]
    Missing(String),
    #[error("{0}")]
    Other(String),
}
impl LowerError { pub fn missing(s: &str) -> Self { Self::Missing(s.to_string()) } }
