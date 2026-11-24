//! Walking the KDL tree. Each module lowers its own node; a container says which children it
//! delegates (`Module::delegates`), and this lowers those through the registry and hands the
//! results back to the container's `adopt`. Modules never see the registry.

use kdl::KdlNode;

use crate::{Delegate, LowerCtx, LowerError, LowerOutput, Module, Registry};

pub struct Lowerer<'r> {
    registry: &'r Registry,
}

impl<'r> Lowerer<'r> {
    pub fn new(registry: &'r Registry) -> Self {
        Self { registry }
    }

    /// The caller has already validated `node` against the module's schema.
    pub fn lower(
        &self,
        module: &dyn Module,
        node: &KdlNode,
        ctx: &mut LowerCtx,
    ) -> Result<LowerOutput, LowerError> {
        let own = module.lower(node, ctx)?;
        let children = match module.delegates() {
            None => return Ok(own),
            Some(Delegate::Undeclared) => {
                let declared: Vec<&str> = module
                    .schema()
                    .children
                    .iter()
                    .map(|c| c.name.as_str())
                    .collect();
                self.lower_children(node, &declared, ctx)?
            }
            Some(Delegate::Block(name)) => match knixl_kdl::children_named(node, name).next() {
                Some(block) => self.lower_children(block, &[], ctx)?,
                None => Vec::new(),
            },
        };
        module.adopt(node, own, children)
    }

    /// A child that fails its module's schema, or that no module claims, becomes a
    /// diagnostic rather than an error.
    pub fn lower_children(
        &self,
        node: &KdlNode,
        skip: &[&str],
        ctx: &mut LowerCtx,
    ) -> Result<Vec<LowerOutput>, LowerError> {
        let mut outputs = Vec::new();
        for child in node.children().into_iter().flat_map(|d| d.nodes()) {
            let name = child.name().value();
            if skip.contains(&name) {
                continue;
            }
            let Some(module) = self.registry.get(name) else {
                ctx.reject(child.span(), format!("no module claims node `{name}`"));
                continue;
            };
            if let Err(diags) = module.schema().validate(child) {
                ctx.report(diags);
                continue;
            }
            let mut out = self.lower(module, child, ctx)?;
            out.attribute(&module.id().name);
            outputs.push(out);
        }
        Ok(outputs)
    }
}
