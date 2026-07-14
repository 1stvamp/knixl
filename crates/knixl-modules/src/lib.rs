//! The Module trait and everything it needs. A built-in Rust module and a runtime-loaded
//! KDL module are indistinguishable to the generator: the declarative loader is itself
//! one Module impl. SPEC-GRADE SKETCH.

pub mod registry;
pub mod template;
pub mod builtin;

use kdl::KdlNode;
use knixl_ir::{Assignment, RawNix};
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
    /// A container (e.g. `host`) accepts children beyond those declared here and delegates
    /// them to their own modules; a leaf rejects unknown children as typos.
    pub open_children: bool,
}

pub struct Field { pub name: String, pub ty: ValueTy, pub required: bool, pub doc: String }

pub struct Child {
    pub name: String,
    pub ty: ValueTy,
    pub required: bool,
    pub repeated: bool,   // `database "app"; database "metrics"` => repeated
    pub delegate: bool,   // true => another module's node, dispatched, not read here
    pub doc: String,
    /// Sub-fields of a structured child (ty == Node): positional sub-args and key=value
    /// sub-props. Empty for scalar/flag children. These build the `Scope` bindings that a
    /// `{child.field}` lookup resolves against.
    pub args: Vec<Field>,
    pub props: Vec<Field>,
}

/// KDL-side INPUT types. Not oracle NixType (which is OUTPUT option types).
pub enum ValueTy { Bool, Int, Str, Enum(Vec<String>), Node }

impl NodeSchema {
    /// Missing-required, unknown-field, arity, value-type errors, each with a KDL span.
    pub fn validate(&self, node: &KdlNode) -> Result<(), Vec<Diagnostic>> {
        let mut diags = Vec::new();
        let node_span = node.span();

        // positional arguments
        let positional: Vec<&kdl::KdlEntry> =
            node.entries().iter().filter(|e| e.name().is_none()).collect();
        for (i, field) in self.args.iter().enumerate() {
            match positional.get(i) {
                Some(entry) => check_ty(entry.value(), &field.ty, &field.name, node_span, &mut diags),
                None if field.required => {
                    diags.push(diag_at(node_span, format!("missing required argument `{}`", field.name)));
                }
                None => {}
            }
        }
        if positional.len() > self.args.len() {
            diags.push(diag_at(
                node_span,
                format!(
                    "`{}` takes at most {} argument(s), got {}",
                    node.name().value(),
                    self.args.len(),
                    positional.len()
                ),
            ));
        }

        // properties (key=value)
        for entry in node.entries().iter().filter(|e| e.name().is_some()) {
            let name = entry.name().unwrap().value();
            match self.props.iter().find(|f| f.name == name) {
                Some(field) => check_ty(entry.value(), &field.ty, name, node_span, &mut diags),
                None => diags.push(diag_at(node_span, format!("unknown property `{name}`"))),
            }
        }
        for field in &self.props {
            let present = node
                .entries()
                .iter()
                .any(|e| e.name().map(|n| n.value()) == Some(field.name.as_str()));
            if field.required && !present {
                diags.push(diag_at(node_span, format!("missing required property `{}`", field.name)));
            }
        }

        // children
        let children: &[KdlNode] = match node.children() {
            Some(doc) => doc.nodes(),
            None => &[],
        };
        for spec in &self.children {
            let matching: Vec<&KdlNode> =
                children.iter().filter(|c| c.name().value() == spec.name).collect();
            if spec.required && matching.is_empty() {
                diags.push(diag_at(node_span, format!("missing required child `{}`", spec.name)));
            }
            if !spec.repeated && matching.len() > 1 {
                diags.push(diag_at(node_span, format!("child `{}` may appear at most once", spec.name)));
            }
            if !spec.delegate && !matches!(spec.ty, ValueTy::Node) {
                for c in matching {
                    match c.entries().iter().find(|e| e.name().is_none()) {
                        Some(e) => check_ty(e.value(), &spec.ty, &spec.name, c.span(), &mut diags),
                        // A bare boolean child is presence-as-true (see knixl_kdl::child_flag).
                        None if matches!(spec.ty, ValueTy::Bool) => {}
                        None => diags.push(diag_at(c.span(), format!("child `{}` needs a value", spec.name))),
                    }
                }
            }
        }
        if !self.open_children {
            for c in children {
                let name = c.name().value();
                if !self.children.iter().any(|spec| spec.name == name) {
                    diags.push(diag_at(c.span(), format!("unknown child `{name}`")));
                }
            }
        }

        if diags.is_empty() { Ok(()) } else { Err(diags) }
    }
}

fn diag_at(span: SourceSpan, message: String) -> Diagnostic {
    Diagnostic { span: Some(span), message }
}

fn check_ty(value: &kdl::KdlValue, ty: &ValueTy, name: &str, span: SourceSpan, diags: &mut Vec<Diagnostic>) {
    let ok = match ty {
        ValueTy::Bool => value.as_bool().is_some(),
        ValueTy::Int => value.as_integer().is_some(),
        ValueTy::Str => value.as_string().is_some(),
        ValueTy::Enum(variants) => value.as_string().is_some_and(|s| variants.iter().any(|v| v == s)),
        ValueTy::Node => true, // a block child, not a scalar value
    };
    if !ok {
        diags.push(diag_at(span, format!("`{name}` has the wrong type")));
    }
}

// ---- lowering ----

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
    pub fn lower_children(&mut self, node: &KdlNode, consumed: &[&str])
        -> Result<Vec<LowerOutput>, LowerError> {
        let registry = self.registry;
        let mut outputs = Vec::new();
        if let Some(doc) = node.children() {
            for child in doc.nodes() {
                let name = child.name().value();
                if consumed.contains(&name) { continue; }
                match registry.get(name) {
                    Some(module) => {
                        // Validate the delegated child against its own module's schema before
                        // lowering; a schema error is non-fatal here (collected as a diagnostic).
                        if let Err(diags) = module.schema().validate(child) {
                            self.diags.extend(diags);
                            continue;
                        }
                        outputs.push(module.lower(child, self)?);
                    }
                    None => self.lint(child.span(), format!("no module claims node `{name}`")),
                }
            }
        }
        Ok(outputs)
    }

    pub fn lint(&mut self, span: SourceSpan, msg: impl Into<String>) {
        self.diags.push(Diagnostic { span: Some(span), message: msg.into() });
    }
}

pub struct LowerOutput { pub units: Vec<Unit>, pub raw: Vec<RawUnit> }
pub struct Unit { pub bucket: Bucket, pub assignment: Assignment }
/// A verbatim raw-nix passthrough statement, bucketed like a Unit.
pub struct RawUnit { pub bucket: Bucket, pub raw: RawNix }

impl LowerOutput {
    pub fn new() -> Self { Self { units: Vec::new(), raw: Vec::new() } }
    /// Convenience for the common case of assignments with no raw passthrough.
    pub fn units(units: Vec<Unit>) -> Self { Self { units, raw: Vec::new() } }
}
impl Default for LowerOutput { fn default() -> Self { Self::new() } }

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

#[cfg(test)]
mod tests {
    use super::*;

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>().unwrap().nodes().first().unwrap().clone()
    }

    fn host_like_schema() -> NodeSchema {
        NodeSchema {
            summary: "test".into(),
            args: vec![Field { name: "name".into(), ty: ValueTy::Str, required: true, doc: String::new() }],
            props: vec![Field { name: "count".into(), ty: ValueTy::Int, required: false, doc: String::new() }],
            children: vec![Child {
                name: "system".into(),
                ty: ValueTy::Str,
                required: true,
                repeated: false,
                delegate: false,
                doc: String::new(),
                args: vec![],
                props: vec![],
            }],
            open_children: true,
        }
    }

    fn leaf_schema() -> NodeSchema {
        NodeSchema { open_children: false, ..host_like_schema() }
    }

    #[test]
    fn validate_accepts_a_well_formed_node() {
        let n = node("host \"web\" {\n    system \"x86_64-linux\"\n    extra \"delegated\"\n}");
        assert!(host_like_schema().validate(&n).is_ok());
    }

    #[test]
    fn validate_reports_missing_required_arg() {
        let n = node("host {\n    system \"x\"\n}");
        let errs = host_like_schema().validate(&n).unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("argument")));
    }

    #[test]
    fn validate_reports_missing_required_child() {
        let n = node("host \"web\"");
        let errs = host_like_schema().validate(&n).unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("system")));
    }

    #[test]
    fn validate_reports_unknown_property() {
        let n = node("host \"web\" bogus=1 {\n    system \"x\"\n}");
        let errs = host_like_schema().validate(&n).unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("bogus")));
    }

    #[test]
    fn validate_reports_wrong_arg_type() {
        let n = node("host #true {\n    system \"x\"\n}");
        let errs = host_like_schema().validate(&n).unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("name")));
    }

    #[test]
    fn open_schema_allows_undeclared_children_but_leaf_rejects_them() {
        let n = node("host \"web\" {\n    system \"x\"\n    extra \"y\"\n}");
        assert!(host_like_schema().validate(&n).is_ok());
        let errs = leaf_schema().validate(&n).unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("extra")));
    }

    #[test]
    fn validate_allows_a_bare_boolean_flag_child() {
        let schema = NodeSchema {
            summary: String::new(),
            args: vec![],
            props: vec![],
            children: vec![Child {
                name: "flag".into(),
                ty: ValueTy::Bool,
                required: false,
                repeated: false,
                delegate: false,
                doc: String::new(),
                args: vec![],
                props: vec![],
            }],
            open_children: false,
        };
        let n = node("svc {\n    flag\n}");
        assert!(schema.validate(&n).is_ok(), "a bare boolean flag is presence-as-true");
    }

    #[test]
    fn validate_reports_arity_for_non_repeated_child() {
        let n = node("host \"web\" {\n    system \"a\"\n    system \"b\"\n}");
        let errs = leaf_schema().validate(&n).unwrap_err();
        assert!(errs.iter().any(|d| d.message.contains("once")));
    }

    struct StubModule { schema: NodeSchema }
    impl StubModule {
        fn new() -> Self {
            Self {
                schema: NodeSchema {
                    summary: String::new(),
                    args: vec![],
                    props: vec![],
                    children: vec![],
                    open_children: true,
                },
            }
        }
    }
    impl Module for StubModule {
        fn id(&self) -> ModuleId { ModuleId { name: "stub".into(), version: "1.0.0".parse().unwrap() } }
        fn node_name(&self) -> &str { "stub" }
        fn schema(&self) -> &NodeSchema { &self.schema }
        fn lower(&self, _node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
            Ok(LowerOutput::units(vec![]))
        }
    }

    #[test]
    fn lower_children_dispatches_known_and_flags_unknown() {
        let mut reg = Registry::new();
        reg.register(Box::new(StubModule::new())).unwrap();
        let host = node("host \"web\" {\n    system \"x\"\n    stub\n    mystery\n}");
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags);
        let outputs = ctx.lower_children(&host, &["system"]).unwrap();
        drop(ctx);
        assert_eq!(outputs.len(), 1, "the one registered child (stub) is dispatched");
        assert!(diags.iter().any(|d| d.message.contains("mystery")), "unknown node flagged");
    }
}
