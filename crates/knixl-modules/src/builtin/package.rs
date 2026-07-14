//! `package`: add a package to the host's `environment.systemPackages`. Repeated under a
//! host (`package "ripgrep"`); each node contributes one `pkgs.<name>`. The pipeline's
//! list-merge folds the repeats into a single `environment.systemPackages` list.
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use crate::{Bucket, Field, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, ValueTy};

pub struct PackageModule { schema: NodeSchema }
impl PackageModule { pub fn new() -> Self { Self { schema: schema() } } }
impl Default for PackageModule { fn default() -> Self { Self::new() } }

impl Module for PackageModule {
    fn id(&self) -> ModuleId { ModuleId { name: "package".into(), version: "0.1.0".parse().unwrap() } }
    fn node_name(&self) -> &str { "package" }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let name = arg_name(node).ok_or_else(|| LowerError::missing("package name"))?;
        let assignment = Assignment {
            path: AttrPath(vec![
                AttrKey::Ident("environment".into()),
                AttrKey::Ident("systemPackages".into()),
            ]),
            value: NixExpr::List(vec![NixExpr::Select(
                Box::new(NixExpr::Ref("pkgs".into())),
                vec![name],
            )]),
            priority: None,
            condition: None,
            doc: None,
        };
        Ok(LowerOutput::units(vec![Unit {
            bucket: Bucket::Default,
            assignment,
            module: String::new(),
        }]))
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "Add a package to environment.systemPackages.".into(),
        args: vec![Field {
            name: "name".into(),
            ty: ValueTy::Str,
            required: true,
            doc: "The nixpkgs attribute name, e.g. ripgrep.".into(),
        }],
        props: vec![],
        children: vec![],
        open_children: false,
    }
}

/// The node's first positional string argument. Schema validation guarantees it is present.
fn arg_name(node: &KdlNode) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>().unwrap().nodes().first().unwrap().clone()
    }

    #[test]
    fn package_lowers_to_a_systempackages_list_entry() {
        let m = PackageModule::new();
        let n = node("package \"ripgrep\"");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags);

        let out = m.lower(&n, &mut ctx).unwrap();
        assert_eq!(out.units.len(), 1);
        let a = &out.units[0].assignment;

        let keys: Vec<&str> = a.path.0.iter().map(|k| match k {
            AttrKey::Ident(s) | AttrKey::Quoted(s) => s.as_str(),
        }).collect();
        assert_eq!(keys, vec!["environment", "systemPackages"]);
        assert!(matches!(&out.units[0].bucket, Bucket::Default));

        match &a.value {
            NixExpr::List(items) => {
                assert_eq!(items.len(), 1);
                match &items[0] {
                    NixExpr::Select(base, path) => {
                        assert!(matches!(base.as_ref(), NixExpr::Ref(r) if r == "pkgs"));
                        assert_eq!(path, &vec!["ripgrep".to_string()]);
                    }
                    other => panic!("expected pkgs.<name> select, got {other:?}"),
                }
            }
            other => panic!("expected a list value, got {other:?}"),
        }
    }
}
