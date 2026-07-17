//! `package`: add a package to the host's `environment.systemPackages`. Repeated under a
//! host (`package "ripgrep"`); each node contributes one `pkgs.<name>`. The pipeline's
//! list-merge folds the repeats into a single `environment.systemPackages` list.
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, Binding, Formals, NixExpr};
use crate::{Bucket, Field, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, PinStrategy, Unit, ValueTy};

pub struct PackageModule { schema: NodeSchema }
impl PackageModule { pub fn new() -> Self { Self { schema: schema() } } }
impl Default for PackageModule { fn default() -> Self { Self::new() } }

impl Module for PackageModule {
    fn id(&self) -> ModuleId { ModuleId { name: "package".into(), version: "0.1.0".parse().unwrap() } }
    fn node_name(&self) -> &str { "package" }
    fn schema(&self) -> &NodeSchema { &self.schema }
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let name = arg_name(node).ok_or_else(|| LowerError::missing("package name"))?;
        let version = prop_str(node, "version");

        let select = match &version {
            None => NixExpr::Select(Box::new(NixExpr::Ref("pkgs".into())), vec![name.clone()]),
            Some(v) => {
                let pin = ctx.pin(&name, v).ok_or_else(|| {
                    LowerError::Other(format!(
                        "{name} {v} on {} is not resolved: run knixl install to pin it",
                        ctx.scope().host
                    ))
                })?;
                match pin.strategy {
                    PinStrategy::CommitMix => historical_pkg(&pin.nixpkgs_rev, &name),
                    PinStrategy::Override => override_pkg(&pin.nixpkgs_rev, &name),
                }
            }
        };

        let assignment = Assignment {
            path: AttrPath(vec![
                AttrKey::Ident("environment".into()),
                AttrKey::Ident("systemPackages".into()),
            ]),
            value: NixExpr::List(vec![select]),
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
        props: vec![Field {
            name: "version".into(),
            ty: ValueTy::Str,
            required: false,
            doc: "Pin to this version, resolved to a nixpkgs commit at install time.".into(),
        }],
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

/// The string value of a named property, if present.
fn prop_str(node: &KdlNode, key: &str) -> Option<String> {
    node.entries()
        .iter()
        .find(|e| e.name().map(|n| n.value()) == Some(key))
        .and_then(|e| e.value().as_string())
        .map(str::to_string)
}

/// The historical package as it existed at a pinned nixpkgs commit:
/// `(import (builtins.fetchGit { rev = ..; shallow = true; url = ..; }) { system =
/// pkgs.system; }).<name>`. Shared by both pin strategies: `CommitMix` uses this directly,
/// `Override` binds it to `_pin` and overrides the baseline package with its `src`/`version`.
fn historical_pkg(rev: &str, name: &str) -> NixExpr {
    let mut src = std::collections::BTreeMap::new();
    src.insert(AttrKey::Ident("shallow".into()), NixExpr::Bool(true));
    src.insert(
        AttrKey::Ident("url".into()),
        NixExpr::Str("https://github.com/NixOS/nixpkgs".into()),
    );
    src.insert(AttrKey::Ident("rev".into()), NixExpr::Str(rev.to_string()));
    let fetch = NixExpr::Apply(
        Box::new(NixExpr::Select(Box::new(NixExpr::Ref("builtins".into())), vec!["fetchGit".into()])),
        vec![NixExpr::AttrSet(src)],
    );
    let mut import_arg = std::collections::BTreeMap::new();
    import_arg.insert(
        AttrKey::Ident("system".into()),
        NixExpr::Select(Box::new(NixExpr::Ref("pkgs".into())), vec!["system".into()]),
    );
    let imported = NixExpr::Apply(
        Box::new(NixExpr::Ref("import".into())),
        vec![fetch, NixExpr::AttrSet(import_arg)],
    );
    NixExpr::Select(Box::new(imported), vec![name.to_string()])
}

/// `Override` strategy: keep the baseline `pkgs.<name>` derivation (its build inputs, deps,
/// etc.) but override just `src` and `version` from the historical pin, via
/// `let _pin = <historical_pkg>; in pkgs.<name>.overrideAttrs ({ ... }: { src = _pin.src;
/// version = _pin.version; })`. `_pin` is safe as a binding name: each package node emits
/// its own self-contained `let`, so there is no cross-element collision.
fn override_pkg(rev: &str, name: &str) -> NixExpr {
    let bind = "_pin".to_string();
    let mut attrs = std::collections::BTreeMap::new();
    attrs.insert(
        AttrKey::Ident("src".into()),
        NixExpr::Select(Box::new(NixExpr::Ref(bind.clone())), vec!["src".into()]),
    );
    attrs.insert(
        AttrKey::Ident("version".into()),
        NixExpr::Select(Box::new(NixExpr::Ref(bind.clone())), vec!["version".into()]),
    );
    let lambda = NixExpr::Lambda {
        formals: Formals { args: vec![], ellipsis: true }, // `{ ... }:` ignores previous attrs
        body: Box::new(NixExpr::AttrSet(attrs)),
    };
    let overridden = NixExpr::Apply(
        Box::new(NixExpr::Select(
            Box::new(NixExpr::Select(Box::new(NixExpr::Ref("pkgs".into())), vec![name.to_string()])),
            vec!["overrideAttrs".into()],
        )),
        vec![lambda],
    );
    NixExpr::Let {
        bindings: vec![Binding { name: bind, value: historical_pkg(rev, name) }],
        body: Box::new(overridden),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};
    use knixl_ir::Emit;

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>().unwrap().nodes().first().unwrap().clone()
    }

    #[test]
    fn package_lowers_to_a_systempackages_list_entry() {
        let m = PackageModule::new();
        let n = node("package \"ripgrep\"");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, vec![]);

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

    #[test]
    fn versioned_package_lowers_to_a_pinned_import_select() {
        let m = PackageModule::new();
        let n = node("package \"htop\" version=\"3.2.1\"");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let pins = vec![crate::ResolvedPin {
            package: "htop".into(),
            version: "3.2.1".into(),
            nixpkgs_rev: "abc123".into(),
            strategy: crate::PinStrategy::CommitMix,
        }];
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, pins);

        let out = m.lower(&n, &mut ctx).unwrap();
        // The emitted text must contain the pinned fetchGit import and select the package
        // from it, not from baseline `pkgs`.
        let a = &out.units[0].assignment;
        let rendered = format!("{:?}", a.value); // structural check below is the real assertion
        assert!(rendered.contains("abc123"), "carries the pinned commit: {rendered}");
        assert!(rendered.contains("htop"), "selects the package: {rendered}");
        assert!(rendered.contains("fetchGit"), "uses fetchGit: {rendered}");
        assert!(rendered.contains("shallow"), "uses shallow fetch: {rendered}");
        assert!(rendered.contains("system"), "passes pkgs.system to the import: {rendered}");
    }

    /// Lowers a versioned `package` node with the given strategy and returns the rendered
    /// Nix text for its `environment.systemPackages` list value, via the real emitter
    /// (`knixl_ir::emit`), not `Debug`. This is what actually reaches the generated file.
    fn render_pinned(strategy: crate::PinStrategy) -> String {
        let m = PackageModule::new();
        let n = node("package \"htop\" version=\"3.2.1\"");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let pins = vec![crate::ResolvedPin {
            package: "htop".into(),
            version: "3.2.1".into(),
            nixpkgs_rev: "abc123".into(),
            strategy,
        }];
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, pins);

        let out = m.lower(&n, &mut ctx).unwrap();
        let mut w = knixl_ir::Writer::new();
        out.units[0].assignment.value.emit(&mut w);
        w.into_string()
    }

    #[test]
    fn commit_mix_strategy_emits_the_bare_historical_import() {
        let rendered = render_pinned(crate::PinStrategy::CommitMix);

        assert!(rendered.contains("(import (builtins.fetchGit"), "imports from fetchGit: {rendered}");
        assert!(rendered.contains("abc123"), "carries the pinned commit: {rendered}");
        assert!(rendered.contains(").htop"), "selects the package from the import: {rendered}");
        assert!(!rendered.contains("overrideAttrs"), "CommitMix does not override: {rendered}");
        assert!(!rendered.contains("let"), "CommitMix introduces no let binding: {rendered}");
    }

    #[test]
    fn override_strategy_emits_a_let_bound_overrideattrs_call() {
        let rendered = render_pinned(crate::PinStrategy::Override);

        assert!(rendered.contains("(let"), "let/in is parenthesised in list position: {rendered}");
        assert!(rendered.contains("_pin = "), "binds the historical import to _pin: {rendered}");
        assert!(rendered.contains("(builtins.fetchGit"), "the binding still fetches from the pin: {rendered}");
        assert!(rendered.contains("abc123"), "carries the pinned commit: {rendered}");
        assert!(rendered.contains("in pkgs.htop.overrideAttrs"), "overrides the baseline package: {rendered}");
        assert!(rendered.contains("src = _pin.src;"), "src comes from the pin: {rendered}");
        assert!(rendered.contains("version = _pin.version;"), "version comes from the pin: {rendered}");
    }

    #[test]
    fn versioned_package_without_a_matching_pin_is_a_lower_error() {
        let m = PackageModule::new();
        let n = node("package \"htop\" version=\"3.2.1\"");
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "web".into() }, &reg, &mut diags, vec![]);
        assert!(m.lower(&n, &mut ctx).is_err(), "declared version with no lock pin is an error");
    }
}
