//! `os`: core host configuration (identity, boot, and system tunables). A built-in, not a
//! declarative module: it carries arbitrary-key attribute sets (`boot.kernel.sysctl`,
//! `nix.settings`) and `pkgs` references (`boot.kernelPackages`, `environment.systemPackages`),
//! none of which the single-level template grammar can express (see docs/04-template-grammar.md).
//!
//! `networking.hostName` is deliberately NOT here: it defaults to the host's label and is owned
//! by the `host` module, which is the only one that knows the label (docs/03).
use crate::builtin::host::unit_default;
use crate::{
    Child, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, ValueTy,
};
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use knixl_kdl::{child_arg_str, children_named, first_arg_str};
use std::collections::BTreeMap;

pub struct Os {
    schema: NodeSchema,
}

impl Os {
    pub fn new() -> Self {
        Self { schema: schema() }
    }
}
impl Default for Os {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Os {
    fn id(&self) -> ModuleId {
        ModuleId {
            name: "os".into(),
            version: "1.0.0".parse().unwrap(),
        }
    }
    fn node_name(&self) -> &str {
        "os"
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(&self, node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let mut units: Vec<Unit> = Vec::new();

        if let Some(v) = child_arg_str(node, "state-version") {
            units.push(unit_default(assign(
                idents(&["system", "stateVersion"]),
                s(&v),
            )));
        }

        if let Some(loader) = child_arg_str(node, "boot-loader") {
            if loader != "systemd-boot" {
                return Err(LowerError::Other(format!(
                    "os: unknown boot-loader `{loader}` (only `systemd-boot`)"
                )));
            }
            units.push(unit_default(assign(
                idents(&["boot", "loader", "systemd-boot", "enable"]),
                NixExpr::Bool(true),
            )));
        }

        if let Some(b) = child_bool(node, "efi-can-touch-variables") {
            units.push(unit_default(assign(
                idents(&["boot", "loader", "efi", "canTouchEfiVariables"]),
                NixExpr::Bool(b),
            )));
        }

        if let Some(kp) = child_arg_str(node, "kernel-package") {
            units.push(unit_default(assign(
                idents(&["boot", "kernelPackages"]),
                pkg(&kp),
            )));
        }

        let sysctl = collect_prop_map(node, "sysctl");
        if !sysctl.is_empty() {
            let mut m: BTreeMap<AttrKey, NixExpr> = BTreeMap::new();
            for (k, v) in &sysctl {
                m.insert(AttrKey::Quoted(k.clone()), scalar_expr(v));
            }
            units.push(unit_default(assign(
                idents(&["boot", "kernel", "sysctl"]),
                NixExpr::AttrSet(m),
            )));
        }

        if let Some(tz) = child_arg_str(node, "timezone") {
            units.push(unit_default(assign(idents(&["time", "timeZone"]), s(&tz))));
        }
        if let Some(loc) = child_arg_str(node, "locale") {
            units.push(unit_default(assign(
                idents(&["i18n", "defaultLocale"]),
                s(&loc),
            )));
        }
        if let Some(b) = child_bool(node, "mutable-users") {
            units.push(unit_default(assign(
                idents(&["users", "mutableUsers"]),
                NixExpr::Bool(b),
            )));
        }

        let features: Vec<String> = children_named(node, "experimental-feature")
            .filter_map(first_arg_str)
            .collect();
        if !features.is_empty() {
            units.push(unit_default(assign(
                idents(&["nix", "settings", "experimental-features"]),
                NixExpr::List(features.iter().map(|f| s(f)).collect()),
            )));
        }
        let trusted: Vec<String> = children_named(node, "trusted-user")
            .filter_map(first_arg_str)
            .collect();
        if !trusted.is_empty() {
            units.push(unit_default(assign(
                idents(&["nix", "settings", "trusted-users"]),
                NixExpr::List(trusted.iter().map(|u| s(u)).collect()),
            )));
        }
        for (k, v) in collect_prop_map(node, "nix-setting") {
            units.push(unit_default(assign(
                AttrPath(vec![
                    AttrKey::Ident("nix".into()),
                    AttrKey::Ident("settings".into()),
                    AttrKey::Ident(k),
                ]),
                scalar_expr(&v),
            )));
        }

        let packages: Vec<String> = children_named(node, "system-package")
            .filter_map(first_arg_str)
            .collect();
        if !packages.is_empty() {
            units.push(unit_default(assign(
                idents(&["environment", "systemPackages"]),
                NixExpr::List(packages.iter().map(|p| pkg(p)).collect()),
            )));
        }

        Ok(LowerOutput::units(units))
    }
}

// ---- helpers ----

fn s(v: &str) -> NixExpr {
    NixExpr::Str(v.to_string())
}

fn pkg(name: &str) -> NixExpr {
    NixExpr::Select(
        Box::new(NixExpr::Ref("pkgs".into())),
        vec![name.to_string()],
    )
}

fn idents(segs: &[&str]) -> AttrPath {
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

/// A bool-flag child: present with an explicit bool wins, bare presence is true, absence is None.
fn child_bool(node: &KdlNode, name: &str) -> Option<bool> {
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

/// Read the props of every `<name>` child into one map, preserving native scalar types. Keys sort
/// (BTreeMap) so emit is deterministic regardless of KDL source order.
fn collect_prop_map(node: &KdlNode, name: &str) -> BTreeMap<String, kdl::KdlValue> {
    let mut map = BTreeMap::new();
    for child in children_named(node, name) {
        for entry in child.entries().iter().filter(|e| e.name().is_some()) {
            map.insert(
                entry.name().unwrap().value().to_string(),
                entry.value().clone(),
            );
        }
    }
    map
}

fn scalar_expr(v: &kdl::KdlValue) -> NixExpr {
    if let Some(b) = v.as_bool() {
        NixExpr::Bool(b)
    } else if let Some(i) = v.as_integer() {
        NixExpr::Int(i)
    } else {
        NixExpr::Str(v.as_string().unwrap_or_default().to_string())
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "Core host configuration: identity, boot, and system tunables.".into(),
        args: vec![],
        props: vec![],
        children: vec![
            node_child(
                "state-version",
                ValueTy::Str,
                "system.stateVersion, e.g. \"25.11\".",
            ),
            node_child(
                "boot-loader",
                ValueTy::Str,
                "Boot loader (only \"systemd-boot\"): boot.loader.systemd-boot.enable.",
            ),
            node_child(
                "efi-can-touch-variables",
                ValueTy::Bool,
                "boot.loader.efi.canTouchEfiVariables.",
            ),
            node_child(
                "kernel-package",
                ValueTy::Str,
                "boot.kernelPackages, a pkgs attr, e.g. \"linuxPackages_6_18\".",
            ),
            node_child("timezone", ValueTy::Str, "time.timeZone."),
            node_child("locale", ValueTy::Str, "i18n.defaultLocale."),
            node_child("mutable-users", ValueTy::Bool, "users.mutableUsers."),
            node_child(
                "sysctl",
                ValueTy::Node,
                "boot.kernel.sysctl entries as props, e.g. sysctl \"net.ipv4.ip_forward\"=1.",
            ),
            node_child(
                "experimental-feature",
                ValueTy::Str,
                "nix.settings.experimental-features entry. Repeatable.",
            ),
            node_child(
                "trusted-user",
                ValueTy::Str,
                "nix.settings.trusted-users entry. Repeatable.",
            ),
            node_child(
                "nix-setting",
                ValueTy::Node,
                "Scalar nix.settings entries as props, e.g. nix-setting \"max-jobs\"=4.",
            ),
            node_child(
                "system-package",
                ValueTy::Str,
                "environment.systemPackages entry, a pkgs attr. Repeatable.",
            ),
        ],
        open_children: false,
    }
}

fn node_child(name: &str, ty: ValueTy, doc: &str) -> Child {
    Child {
        name: name.into(),
        ty,
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
        let m = Os::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "nas".into() }, &reg, &mut diags, vec![]);
        m.lower(&node(src), &mut ctx).expect("lower ok").units
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
    fn identity_and_boot_lower() {
        let units = lower_ok(
            "os {\n    state-version \"25.11\"\n    boot-loader \"systemd-boot\"\n    efi-can-touch-variables #true\n    kernel-package \"linuxPackages_6_18\"\n    timezone \"Europe/London\"\n    locale \"en_GB.UTF-8\"\n}",
        );
        assert!(
            matches!(find(&units, "system.stateVersion"), Some(NixExpr::Str(s)) if s == "25.11")
        );
        assert!(matches!(
            find(&units, "boot.loader.systemd-boot.enable"),
            Some(NixExpr::Bool(true))
        ));
        assert!(matches!(
            find(&units, "boot.loader.efi.canTouchEfiVariables"),
            Some(NixExpr::Bool(true))
        ));
        assert!(matches!(
            find(&units, "boot.kernelPackages"),
            Some(NixExpr::Select(_, segs)) if segs == &["linuxPackages_6_18".to_string()]
        ));
        assert!(
            matches!(find(&units, "time.timeZone"), Some(NixExpr::Str(s)) if s == "Europe/London")
        );
        assert!(
            matches!(find(&units, "i18n.defaultLocale"), Some(NixExpr::Str(s)) if s == "en_GB.UTF-8")
        );
    }

    #[test]
    fn sysctl_emits_a_native_typed_attrset() {
        let units = lower_ok("os {\n    sysctl \"net.ipv4.ip_forward\"=1 \"vm.swappiness\"=10\n}");
        let NixExpr::AttrSet(m) = find(&units, "boot.kernel.sysctl").unwrap() else {
            panic!("sysctl not an attrset")
        };
        assert!(matches!(
            m.get(&AttrKey::Quoted("net.ipv4.ip_forward".into())),
            Some(NixExpr::Int(1))
        ));
        assert!(matches!(
            m.get(&AttrKey::Quoted("vm.swappiness".into())),
            Some(NixExpr::Int(10))
        ));
    }

    #[test]
    fn nix_settings_lists_and_scalars() {
        let units = lower_ok(
            "os {\n    experimental-feature \"nix-command\"\n    experimental-feature \"flakes\"\n    trusted-user \"wes\"\n    nix-setting \"max-jobs\"=4\n}",
        );
        let NixExpr::List(feats) = find(&units, "nix.settings.experimental-features").unwrap()
        else {
            panic!()
        };
        assert_eq!(feats.len(), 2);
        let NixExpr::List(users) = find(&units, "nix.settings.trusted-users").unwrap() else {
            panic!()
        };
        assert!(matches!(&users[0], NixExpr::Str(s) if s == "wes"));
        assert!(matches!(
            find(&units, "nix.settings.max-jobs"),
            Some(NixExpr::Int(4))
        ));
    }

    #[test]
    fn mutable_users_and_packages() {
        let units = lower_ok(
            "os {\n    mutable-users #false\n    system-package \"vim\"\n    system-package \"git\"\n}",
        );
        assert!(matches!(
            find(&units, "users.mutableUsers"),
            Some(NixExpr::Bool(false))
        ));
        let NixExpr::List(pkgs) = find(&units, "environment.systemPackages").unwrap() else {
            panic!()
        };
        assert_eq!(pkgs.len(), 2);
        assert!(matches!(
            &pkgs[0],
            NixExpr::Select(_, segs) if segs == &["vim".to_string()]
        ));
    }

    #[test]
    fn empty_os_emits_nothing() {
        assert!(lower_ok("os {\n}").is_empty());
    }
}
