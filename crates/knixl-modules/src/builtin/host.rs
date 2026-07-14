//! `host`: the container module. Consumes its own scalar fields, delegates the rest.
use kdl::KdlNode;
use knixl_ir::{AttrKey, AttrPath, Assignment, NixExpr};
use knixl_kdl::child_arg_str;
use crate::{LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, Bucket};

pub struct Host { schema: NodeSchema }

impl Host { pub fn new() -> Self { Self { schema: schema() } } }
impl Default for Host { fn default() -> Self { Self::new() } }

impl Module for Host {
    fn id(&self) -> ModuleId { ModuleId { name: "host".into(), version: "1.0.0".parse().unwrap() } }
    fn node_name(&self) -> &str { "host" }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let mut units = Vec::new();
        if let Some(sys) = child_arg_str(node, "system") {
            units.push(unit_default(assign(&["nixpkgs", "hostPlatform"], NixExpr::Str(sys))));
        }
        // delegate everything except the fields host consumes itself
        for out in ctx.lower_children(node, &["system"])? { units.extend(out.units); }
        Ok(LowerOutput { units })
    }
}

fn schema() -> NodeSchema { todo!("host schema: arg host name; child system; delegate rest") }

// ---- shared helpers (candidate for a knixl-modules::helpers module) ----

pub(crate) fn assign(path: &[&str], value: NixExpr) -> Assignment {
    Assignment {
        path: AttrPath(path.iter().map(|s| AttrKey::Ident((*s).into())).collect()),
        value, priority: None, condition: None, doc: None,
    }
}
pub(crate) fn unit_default(a: Assignment) -> Unit { Unit { bucket: Bucket::Default, assignment: a } }
