//! `nix-ld`: run prebuilt dynamically linked binaries. Built-in because the library list is
//! `pkgs` references, which the declarative grammar cannot express (see docs/04-template-grammar.md).
use crate::builtin::host::unit_default;
use crate::{
    Child, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, ValueTy,
};
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use knixl_kdl::{children_named, first_arg_str};

pub struct NixLd {
    schema: NodeSchema,
}

impl NixLd {
    pub fn new() -> Self {
        Self { schema: schema() }
    }
}
impl Default for NixLd {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for NixLd {
    fn id(&self) -> ModuleId {
        ModuleId {
            name: "nix-ld".into(),
            version: "1.0.0".parse().unwrap(),
        }
    }
    fn node_name(&self) -> &str {
        "nix-ld"
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(&self, node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        // The node's presence is the opt-in: a host declaring nix-ld wants it on.
        let mut units: Vec<Unit> = vec![unit_default(assign(
            path(&["programs", "nix-ld", "enable"]),
            NixExpr::Bool(true),
        ))];

        let libraries: Vec<String> = children_named(node, "library")
            .filter_map(first_arg_str)
            .collect();
        if !libraries.is_empty() {
            units.push(unit_default(assign(
                path(&["programs", "nix-ld", "libraries"]),
                NixExpr::List(libraries.iter().map(|l| pkg(l)).collect()),
            )));
        }

        Ok(LowerOutput::units(units))
    }
}

/// A `pkgs` reference, split on `.` so a nested attr such as `stdenv.cc.cc.lib` emits as a
/// select chain rather than one opaque segment.
fn pkg(name: &str) -> NixExpr {
    NixExpr::Select(
        Box::new(NixExpr::Ref("pkgs".into())),
        name.split('.').map(str::to_string).collect(),
    )
}

fn path(segs: &[&str]) -> AttrPath {
    AttrPath(segs.iter().map(|s| AttrKey::Ident((*s).into())).collect())
}

fn assign(path: AttrPath, value: NixExpr) -> Assignment {
    Assignment {
        path,
        value,
        priority: None,
        condition: None,
        doc: None,
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "Run prebuilt dynamically linked binaries (programs.nix-ld).".into(),
        args: vec![],
        props: vec![],
        children: vec![Child {
            name: "library".into(),
            ty: ValueTy::Str,
            required: false,
            repeated: true,
            delegate: false,
            doc: "programs.nix-ld.libraries entry, a pkgs attr, e.g. \"zlib\" or \"stdenv.cc.cc.lib\". Repeatable.".into(),
            args: vec![],
            props: vec![],
        }],
        open_children: false,
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

    fn lower_ok(src: &str) -> Vec<Unit> {
        let m = NixLd::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(
            Scope {
                host: "type40".into(),
            },
            &reg,
            &mut diags,
            vec![],
        );
        m.lower(&node(src), &mut ctx).expect("lower ok").units
    }

    fn find<'a>(units: &'a [Unit], want: &str) -> Option<&'a NixExpr> {
        units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| {
                a.path
                    .0
                    .iter()
                    .map(|k| match k {
                        AttrKey::Ident(s) | AttrKey::Quoted(s) => s.clone(),
                    })
                    .collect::<Vec<_>>()
                    .join(".")
                    == want
            })
            .map(|a| &a.value)
    }

    #[test]
    fn presence_alone_enables_nix_ld() {
        let units = lower_ok("nix-ld {\n}");
        assert!(matches!(
            find(&units, "programs.nix-ld.enable"),
            Some(NixExpr::Bool(true))
        ));
        assert!(
            find(&units, "programs.nix-ld.libraries").is_none(),
            "no library children, so no libraries list"
        );
    }

    #[test]
    fn libraries_lower_to_pkgs_references_in_source_order() {
        let units = lower_ok(
            "nix-ld {\n    library \"stdenv.cc.cc.lib\"\n    library \"zlib\"\n    library \"zstd\"\n    library \"elfutils\"\n}",
        );
        let NixExpr::List(libs) = find(&units, "programs.nix-ld.libraries").unwrap() else {
            panic!("libraries is not a list")
        };
        assert_eq!(libs.len(), 4);
        // A dotted name becomes a select chain, so it emits as pkgs.stdenv.cc.cc.lib.
        assert!(matches!(
            &libs[0],
            NixExpr::Select(_, segs)
                if segs == &["stdenv".to_string(), "cc".into(), "cc".into(), "lib".into()]
        ));
        assert!(matches!(
            &libs[1],
            NixExpr::Select(_, segs) if segs == &["zlib".to_string()]
        ));
        assert!(matches!(
            &libs[3],
            NixExpr::Select(_, segs) if segs == &["elfutils".to_string()]
        ));
    }
}
