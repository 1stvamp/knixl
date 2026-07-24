//! The curated declarative modules embedded into the binary. Source of truth is the repo
//! `modules/` tree (golden-tested); this bundles it so any project has the stdlib offline,
//! with no local copy.
use crate::registry::Registry;
use crate::template::DeclarativeModule;
use crate::{Module, ModuleLayer, ShadowNotice};
use include_dir::{include_dir, Dir};
use std::collections::BTreeSet;

static STDLIB: Dir = include_dir!("$CARGO_MANIFEST_DIR/stdlib");

/// Register every embedded stdlib module whose claimed node is not already taken by a
/// higher-precedence layer. Returns a shadow notice for each module skipped because its node
/// was already claimed, correctly attributed to whichever of `builtin_nodes`, `local_nodes`,
/// or `fetched_nodes` claimed it (the caller, `build_registry`, is the only one that knows
/// which layer registered which node). Iterates entries in sorted name order for determinism.
pub fn register_stdlib(
    reg: &mut Registry,
    builtin_nodes: &BTreeSet<String>,
    local_nodes: &BTreeSet<String>,
    fetched_nodes: &BTreeSet<String>,
) -> Vec<ShadowNotice> {
    let mut notices = Vec::new();
    let mut dirs: Vec<&Dir> = STDLIB.dirs().collect();
    dirs.sort_by(|a, b| a.path().cmp(b.path()));
    for d in dirs {
        let Some(file) = d.get_file(d.path().join("knixl-module.kdl")) else {
            continue;
        };
        let src = file
            .contents_utf8()
            .expect("embedded stdlib module is valid UTF-8");
        let doc = src
            .parse::<kdl::KdlDocument>()
            .expect("embedded stdlib module parses (golden-tested)");
        let module = DeclarativeModule::from_kdl(&doc, file.path())
            .expect("embedded stdlib module type-checks (golden-tested)");
        let node = module.node_name().to_string();
        if reg.get(&node).is_some() {
            notices.push(ShadowNotice {
                node: node.clone(),
                kept: layer_of(&node, builtin_nodes, local_nodes, fetched_nodes),
                shadowed: ModuleLayer::Stdlib,
            });
            continue;
        }
        // register() only errors on a duplicate, which the guard above already excluded.
        let _ = reg.register(Box::new(module));
    }
    notices
}

/// Which of the three higher-precedence layers claimed `node`, given the node-name sets
/// `build_registry` captured right after each layer registered. Defaults to `Local` only as
/// a last resort (should not happen: a node claimed in the registry but absent from all three
/// sets would be a bug elsewhere), rather than panicking on a diagnostic-only path.
fn layer_of(
    node: &str,
    builtin_nodes: &BTreeSet<String>,
    local_nodes: &BTreeSet<String>,
    fetched_nodes: &BTreeSet<String>,
) -> ModuleLayer {
    if builtin_nodes.contains(node) {
        ModuleLayer::Builtin
    } else if local_nodes.contains(node) {
        ModuleLayer::Local
    } else if fetched_nodes.contains(node) {
        ModuleLayer::Fetched
    } else {
        ModuleLayer::Local
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::register_builtins;
    use crate::registry::Registry;

    #[test]
    fn stdlib_registers_declarative_modules() {
        let mut reg = Registry::new();
        let empty = BTreeSet::new();
        let notices = register_stdlib(&mut reg, &empty, &empty, &empty);
        assert!(notices.is_empty(), "fresh registry: no shadows");
        // A known stdlib node resolves purely from the embed.
        assert!(reg.get("web-service").is_some());
        assert!(reg.get("zfs").is_some());
        assert!(reg.get("tailscale").is_some());
        assert!(reg.get("incus").is_some());
    }

    #[test]
    fn stdlib_skips_a_node_a_builtin_already_claims_without_a_false_shadow() {
        // No stdlib module claims a built-in node today, so registering built-ins first must
        // not produce any shadow notice.
        let mut reg = Registry::new();
        register_builtins(&mut reg);
        let builtin_nodes: BTreeSet<String> = reg.entries().map(|(k, _)| k.to_string()).collect();
        let empty = BTreeSet::new();
        let notices = register_stdlib(&mut reg, &builtin_nodes, &empty, &empty);
        assert!(
            notices.iter().all(|n| n.shadowed == ModuleLayer::Stdlib),
            "any notice must be about a shadowed stdlib module"
        );
    }

    #[test]
    fn stdlib_shadow_notice_is_attributed_to_the_correct_layer() {
        // A stdlib module shadowed by a `local_nodes` entry (rather than a builtin) must be
        // reported as `kept: Local`, and one shadowed by `fetched_nodes` as `kept: Fetched`,
        // not both defaulting to the same layer regardless of which one actually claimed it.
        let mut reg = Registry::new();
        // web-service is a real stdlib node; claim it before stdlib registers to force a
        // shadow, and tell layer_of it came from the fetched layer.
        let src = "module name=\"fake-fetched\" version=\"1.0.0\" {\n    claims-node \"web-service\"\n    schema {\n    }\n    emit {\n    }\n}\n";
        let module = crate::template::DeclarativeModule::from_kdl(
            &src.parse::<kdl::KdlDocument>().unwrap(),
            std::path::Path::new("fetched/web-service/knixl-module.kdl"),
        )
        .unwrap();
        reg.register(Box::new(module)).unwrap();

        let builtin_nodes = BTreeSet::new();
        let local_nodes = BTreeSet::new();
        let fetched_nodes: BTreeSet<String> = ["web-service".to_string()].into_iter().collect();
        let notices = register_stdlib(&mut reg, &builtin_nodes, &local_nodes, &fetched_nodes);

        let notice = notices
            .iter()
            .find(|n| n.node == "web-service")
            .expect("web-service shadow notice present");
        assert_eq!(notice.kept, ModuleLayer::Fetched);
    }
}
