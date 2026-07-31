//! `guest`: a NixOS system container whose config is a nested knixl module tree (ADR 0011).
//! A built-in module; see docs/04-template-grammar.md for why.
use crate::builtin::host::unit_default;
use crate::{
    Bucket, Child, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, ValueTy,
};
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use knixl_kdl::{child_arg_str, children_named, first_arg_str};

pub struct Guest {
    schema: NodeSchema,
}

impl Guest {
    pub fn new() -> Self {
        Self { schema: schema() }
    }
}
impl Default for Guest {
    fn default() -> Self {
        Self::new()
    }
}

/// Guest-level children consumed by this module (envelope + the nested `config` block); everything
/// else would be a stray node. `config` is handled specially, the rest map to container envelope
/// options.
const CONSUMED: &[&str] = &[
    "autostart",
    "ephemeral",
    "private-network",
    "host-address",
    "local-address",
    "bind-mount",
    "config",
];

impl Module for Guest {
    fn id(&self) -> ModuleId {
        ModuleId {
            name: "guest".into(),
            version: "1.0.0".parse().unwrap(),
        }
    }
    fn node_name(&self) -> &str {
        "guest"
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let name =
            first_arg_str(node).ok_or_else(|| LowerError::Other("`guest` needs a name".into()))?;
        let mut units: Vec<Unit> = Vec::new();

        // --- envelope: containers.<name>.<opt> ---
        if let Some(b) = flag(node, "autostart") {
            units.push(env_unit(&name, "autoStart", NixExpr::Bool(b)));
        }
        if let Some(b) = flag(node, "ephemeral") {
            units.push(env_unit(&name, "ephemeral", NixExpr::Bool(b)));
        }
        if let Some(b) = flag(node, "private-network") {
            units.push(env_unit(&name, "privateNetwork", NixExpr::Bool(b)));
        }
        if let Some(a) = child_arg_str(node, "host-address") {
            units.push(env_unit(&name, "hostAddress", NixExpr::Str(a)));
        }
        if let Some(a) = child_arg_str(node, "local-address") {
            units.push(env_unit(&name, "localAddress", NixExpr::Str(a)));
        }
        for bm in children_named(node, "bind-mount") {
            let mount = first_arg_str(bm)
                .ok_or_else(|| LowerError::Other("`bind-mount` needs a mount point".into()))?;
            let host_path = bm
                .get("host-path")
                .and_then(|v| v.as_string())
                .ok_or_else(|| LowerError::missing(&format!("bind-mount `{mount}`.host-path")))?
                .to_string();
            let read_only = bm
                .get("read-only")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut m: std::collections::BTreeMap<AttrKey, NixExpr> =
                std::collections::BTreeMap::new();
            m.insert(AttrKey::Ident("hostPath".into()), NixExpr::Str(host_path));
            if read_only {
                m.insert(AttrKey::Ident("isReadOnly".into()), NixExpr::Bool(true));
            }
            units.push(unit_default(Assignment {
                path: AttrPath(vec![
                    AttrKey::Ident("containers".into()),
                    AttrKey::Quoted(name.clone()),
                    AttrKey::Ident("bindMounts".into()),
                    AttrKey::Quoted(mount),
                ]),
                value: NixExpr::AttrSet(m),
                priority: None,
                condition: None,
                doc: None,
            }));
        }

        // --- nested config: lower the `config { }` block's modules and re-root under
        //     containers.<name>.config (ADR 0011). ---
        if let Some(config) = children_named(node, "config").next() {
            for out in ctx.lower_children(config, &[])? {
                if !out.raw.is_empty() {
                    return Err(LowerError::Other(format!(
                        "guest `{name}`: raw-nix inside a guest config cannot be re-rooted"
                    )));
                }
                for unit in out.units {
                    if !matches!(unit.bucket, Bucket::Default) {
                        return Err(LowerError::Other(format!(
                            "guest `{name}`: a side-file module cannot nest into a guest config"
                        )));
                    }
                    units.push(unit_default(reroot(&name, unit.assignment)));
                }
            }
        }

        Ok(LowerOutput::units(units))
    }
}

/// Prepend `containers.<name>.config` to an assignment produced by a nested module.
fn reroot(name: &str, a: Assignment) -> Assignment {
    let mut segs = vec![
        AttrKey::Ident("containers".into()),
        AttrKey::Quoted(name.into()),
        AttrKey::Ident("config".into()),
    ];
    segs.extend(a.path.0);
    Assignment {
        path: AttrPath(segs),
        ..a
    }
}

fn env_unit(name: &str, opt: &str, value: NixExpr) -> Unit {
    unit_default(Assignment {
        path: AttrPath(vec![
            AttrKey::Ident("containers".into()),
            AttrKey::Quoted(name.into()),
            AttrKey::Ident(opt.into()),
        ]),
        value,
        priority: None,
        condition: None,
        doc: None,
    })
}

/// A bool-flag child: explicit bool wins, bare presence is true, absence is None.
fn flag(node: &KdlNode, name: &str) -> Option<bool> {
    let child = children_named(node, name).next()?;
    Some(
        child
            .entries()
            .iter()
            .find(|e| e.name().is_none())
            .and_then(|e| e.value().as_bool())
            .unwrap_or(true),
    )
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "A NixOS system container whose config is a nested knixl module tree.".into(),
        args: vec![crate::Field {
            name: "name".into(),
            ty: ValueTy::Str,
            required: true,
            doc: "The container name (containers.<name>).".into(),
        }],
        props: vec![],
        children: CONSUMED
            .iter()
            .map(|&c| node_child(c, envelope_doc(c)))
            .collect(),
        open_children: false,
    }
}

fn envelope_doc(name: &str) -> &'static str {
    match name {
        "autostart" => "containers.<name>.autoStart.",
        "ephemeral" => "containers.<name>.ephemeral.",
        "private-network" => "containers.<name>.privateNetwork.",
        "host-address" => "containers.<name>.hostAddress.",
        "local-address" => "containers.<name>.localAddress.",
        "bind-mount" => "A bind mount: bind-mount \"/path\" host-path= [read-only=].",
        "config" => "The nested NixOS config: ordinary knixl modules, re-rooted under config.",
        _ => "",
    }
}

fn node_child(name: &str, doc: &str) -> Child {
    Child {
        name: name.into(),
        ty: ValueTy::Node,
        required: false,
        repeated: true,
        delegate: false,
        doc: doc.into(),
        args: vec![],
        props: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::register_builtins;
    use crate::stdlib::register_stdlib;
    use crate::{Registry, Scope};
    use std::collections::BTreeSet;

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone()
    }

    /// A registry with built-ins + stdlib, so a guest's config block can dispatch real modules.
    fn full_registry() -> Registry {
        let mut reg = Registry::new();
        register_builtins(&mut reg);
        let builtin_nodes: BTreeSet<String> = reg.entries().map(|(k, _)| k.to_string()).collect();
        let empty = BTreeSet::new();
        let _ = register_stdlib(&mut reg, &builtin_nodes, &empty, &empty);
        reg
    }

    fn lower_ok(src: &str) -> Vec<Unit> {
        let g = Guest::new();
        let reg = full_registry();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(
            Scope {
                host: "vmhost".into(),
            },
            &reg,
            &mut diags,
            vec![],
        );
        g.lower(&node(src), &mut ctx).expect("lower ok").units
    }

    fn find<'a>(units: &'a [Unit], path: &str) -> Option<&'a NixExpr> {
        units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| {
                a.path
                    .0
                    .iter()
                    .map(|k| match k {
                        AttrKey::Ident(s) => s.clone(),
                        AttrKey::Quoted(s) => format!("\"{s}\""),
                    })
                    .collect::<Vec<_>>()
                    .join(".")
                    == path
            })
            .map(|a| &a.value)
    }

    #[test]
    fn envelope_options_lower() {
        let units = lower_ok(
            "guest \"llm\" {\n    autostart #true\n    private-network #true\n    host-address \"10.100.0.1\"\n    local-address \"10.100.0.2\"\n    bind-mount \"/data\" host-path=\"/srv/llm\" read-only=#true\n}",
        );
        assert!(matches!(
            find(&units, "containers.\"llm\".autoStart"),
            Some(NixExpr::Bool(true))
        ));
        assert!(matches!(
            find(&units, "containers.\"llm\".hostAddress"),
            Some(NixExpr::Str(s)) if s == "10.100.0.1"
        ));
        let NixExpr::AttrSet(bm) = find(&units, "containers.\"llm\".bindMounts.\"/data\"").unwrap()
        else {
            panic!("bindMounts not a set")
        };
        assert!(matches!(
            bm.get(&AttrKey::Ident("hostPath".into())),
            Some(NixExpr::Str(s)) if s == "/srv/llm"
        ));
        assert!(matches!(
            bm.get(&AttrKey::Ident("isReadOnly".into())),
            Some(NixExpr::Bool(true))
        ));
    }

    #[test]
    fn nested_config_is_rerooted_under_containers_name_config() {
        let units = lower_ok(
            "guest \"llm\" {\n    config {\n        os { state-version \"25.11\" }\n        openssh { }\n    }\n}",
        );
        // os.state-version, re-rooted
        assert!(matches!(
            find(&units, "containers.\"llm\".config.system.stateVersion"),
            Some(NixExpr::Str(s)) if s == "25.11"
        ));
        // a service module works unchanged inside the guest, re-rooted
        assert!(matches!(
            find(&units, "containers.\"llm\".config.services.openssh.enable"),
            Some(NixExpr::Bool(true))
        ));
    }

    #[test]
    fn raw_nix_inside_a_guest_config_is_rejected() {
        let g = Guest::new();
        let reg = full_registry();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(
            Scope {
                host: "vmhost".into(),
            },
            &reg,
            &mut diags,
            vec![],
        );
        let n = node(
            "guest \"llm\" {\n    config {\n        raw-nix {\n            \"boot.tmp.cleanOnBoot = true;\"\n        }\n    }\n}",
        );
        let err = format!("{}", g.lower(&n, &mut ctx).err().expect("should error"));
        assert!(err.contains("raw-nix"), "got: {err}");
    }
}
