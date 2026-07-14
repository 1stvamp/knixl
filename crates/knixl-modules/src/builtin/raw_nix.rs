//! `raw-nix`: the escape hatch. Each child node's name is verbatim Nix source, passed
//! through unmodified into the main file. (A `raw-nix { #"""..."""# }` block parses as a
//! node whose name is the raw string.) The source should be validated as parseable Nix
//! before emit so a syntax error points at the KDL span, not at nixos-rebuild.
use kdl::KdlNode;
use knixl_ir::RawNix;
use crate::{Bucket, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, RawUnit};

pub struct RawNixModule { schema: NodeSchema }
impl RawNixModule { pub fn new() -> Self { Self { schema: schema() } } }
impl Default for RawNixModule { fn default() -> Self { Self::new() } }

impl Module for RawNixModule {
    fn id(&self) -> ModuleId { ModuleId { name: "raw-nix".into(), version: "1.0.0".parse().unwrap() } }
    fn node_name(&self) -> &str { "raw-nix" }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let mut raw = Vec::new();
        if let Some(doc) = node.children() {
            for child in doc.nodes() {
                let src = child.name().value().trim().to_string();
                raw.push(RawUnit { bucket: Bucket::Default, raw: RawNix { src, span: None } });
            }
        }
        Ok(LowerOutput { units: Vec::new(), raw })
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "Escape hatch: verbatim Nix passthrough.".into(),
        args: vec![],
        props: vec![],
        children: vec![],
        // Children are raw Nix source, not knixl nodes, so do not reject them.
        open_children: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};

    #[test]
    fn raw_nix_passes_child_source_through() {
        let src = "raw-nix {\n    #\"\"\"\n    systemd.services.nginx.serviceConfig.MemoryMax = \"512M\";\n    \"\"\"#\n}";
        let node = src.parse::<kdl::KdlDocument>().unwrap().nodes().first().unwrap().clone();
        let m = RawNixModule::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags);

        let out = m.lower(&node, &mut ctx).unwrap();
        assert!(out.units.is_empty());
        assert_eq!(out.raw.len(), 1);
        assert!(out.raw[0].raw.src.contains("MemoryMax"));
    }
}
