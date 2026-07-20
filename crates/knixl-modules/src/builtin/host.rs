//! `host`: the container module. Consumes its own scalar fields, delegates the rest.
use crate::{
    Bucket, Child, Field, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit,
    ValueTy,
};
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use knixl_kdl::child_arg_str;

pub struct Host {
    schema: NodeSchema,
}

impl Host {
    pub fn new() -> Self {
        Self { schema: schema() }
    }
}
impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Host {
    fn id(&self) -> ModuleId {
        ModuleId {
            name: "host".into(),
            version: "1.0.0".parse().unwrap(),
        }
    }
    fn node_name(&self) -> &str {
        "host"
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let mut units = Vec::new();
        let mut raw = Vec::new();
        if let Some(sys) = child_arg_str(node, "system") {
            units.push(unit_default(assign(
                &["nixpkgs", "hostPlatform"],
                NixExpr::Str(sys),
            )));
        }
        // delegate everything except the fields host consumes itself. `nixpkgs` is
        // metadata (the declared baseline release, read by the pipeline's gather scan);
        // it contributes no `Unit` and must not be dispatched or linted.
        for out in ctx.lower_children(node, &["system", "nixpkgs"])? {
            units.extend(out.units);
            raw.extend(out.raw);
        }
        Ok(LowerOutput { units, raw })
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "A NixOS host: a system and the services it runs.".into(),
        args: vec![Field {
            name: "name".into(),
            ty: ValueTy::Str,
            required: true,
            doc: "The host name.".into(),
        }],
        props: vec![Field {
            name: "default".into(),
            ty: ValueTy::Bool,
            required: false,
            doc: "Mark this host as the default target for tooling (e.g. `knixl install`).".into(),
        }],
        children: vec![Child {
            name: "system".into(),
            ty: ValueTy::Str,
            required: true,
            repeated: false,
            delegate: false,
            doc: "The Nix system double, e.g. x86_64-linux.".into(),
            args: vec![],
            props: vec![],
        }, Child {
            name: "nixpkgs".into(),
            ty: ValueTy::Node,
            required: false,
            repeated: false,
            delegate: false,
            doc: "Declares this host's baseline nixpkgs release, e.g. `nixpkgs release=\"25.05\"`. \
                  Metadata only: read by the pipeline, never emitted.".into(),
            args: vec![],
            props: vec![Field {
                name: "release".into(),
                ty: ValueTy::Str,
                required: false,
                doc: "The declared nixpkgs release, e.g. \"25.05\".".into(),
            }],
        }],
        // Everything other than `system`/`nixpkgs` is a service, delegated to its own module.
        open_children: true,
    }
}

// ---- shared helpers (candidate for a knixl-modules::helpers module) ----

pub(crate) fn assign(path: &[&str], value: NixExpr) -> Assignment {
    Assignment {
        path: AttrPath(path.iter().map(|s| AttrKey::Ident((*s).into())).collect()),
        value,
        priority: None,
        condition: None,
        doc: None,
    }
}
pub(crate) fn unit_default(a: Assignment) -> Unit {
    Unit {
        bucket: Bucket::Default,
        assignment: a,
        module: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone()
    }

    #[test]
    fn host_accepts_a_default_flag() {
        let h = Host::new();
        let n = node("host \"web\" default=#true {\n    system \"x86_64-linux\"\n}");
        // The `default` marker is tooling metadata for `install`; the schema accepts it.
        assert!(
            h.schema().validate(&n).is_ok(),
            "default prop should validate"
        );
        // It emits nothing: lowering still yields only the hostPlatform assignment.
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, vec![]);
        let out = h.lower(&n, &mut ctx).unwrap();
        assert_eq!(out.units.len(), 1, "default emits nothing extra");
    }

    #[test]
    fn host_lowers_system_to_hostplatform() {
        let host = Host::new();
        let n = node("host \"web\" {\n    system \"x86_64-linux\"\n}");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, vec![]);

        let out = host.lower(&n, &mut ctx).unwrap();
        assert_eq!(out.units.len(), 1);
        let a = &out.units[0].assignment;
        let keys: Vec<&str> = a
            .path
            .0
            .iter()
            .map(|k| match k {
                AttrKey::Ident(s) | AttrKey::Quoted(s) => s.as_str(),
            })
            .collect();
        assert_eq!(keys, vec!["nixpkgs", "hostPlatform"]);
        assert!(matches!(&a.value, NixExpr::Str(s) if s == "x86_64-linux"));
    }

    #[test]
    fn host_recognises_nixpkgs_release_as_metadata_only() {
        let host = Host::new();
        let n =
            node("host \"web\" {\n    system \"x86_64-linux\"\n    nixpkgs release=\"25.05\"\n}");
        assert!(
            host.schema().validate(&n).is_ok(),
            "nixpkgs release should validate"
        );

        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, vec![]);
        let out = host.lower(&n, &mut ctx).unwrap();

        assert!(
            diags.iter().all(|d| !d.message.contains("nixpkgs")),
            "no diagnostic should mention nixpkgs, got: {diags:?}"
        );
        // Only the hostPlatform assignment from `system`; nixpkgs contributes nothing.
        assert_eq!(out.units.len(), 1);
        let a = &out.units[0].assignment;
        let rendered = format!("{a:?}");
        assert!(
            !rendered.contains("nixpkgs release"),
            "release node must not be emitted"
        );
        assert!(
            !rendered.contains("25.05"),
            "release value must not be emitted"
        );
    }
}
