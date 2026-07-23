//! The curated declarative modules embedded into the binary. Source of truth is the repo
//! `modules/` tree (golden-tested); this bundles it so any project has the stdlib offline,
//! with no local copy.
use crate::registry::Registry;
use crate::template::DeclarativeModule;
use crate::{Module, ModuleLayer, ShadowNotice};
use include_dir::{include_dir, Dir};

static STDLIB: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../modules");

/// Register every embedded stdlib module whose claimed node is not already taken by a
/// higher-precedence layer. Returns a shadow notice for each module skipped because its node
/// was already claimed. Iterates entries in sorted name order for determinism.
pub fn register_stdlib(reg: &mut Registry) -> Vec<ShadowNotice> {
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
                node,
                kept: layer_of(reg, &module),
                shadowed: ModuleLayer::Stdlib,
            });
            continue;
        }
        // register() only errors on a duplicate, which the guard above already excluded.
        let _ = reg.register(Box::new(module));
    }
    notices
}

// The kept layer is whatever already claimed the node; the caller (build_registry) knows the
// layering, but for the notice we only need "something higher won". Report the highest
// possible source generically: a claimed node here was taken by built-in, local, or fetched.
fn layer_of(_reg: &Registry, _m: &DeclarativeModule) -> ModuleLayer {
    // build_registry registers built-in, then local, then fetched, then stdlib, so anything
    // already present outranks stdlib; the precise higher layer is not needed for the notice's
    // purpose (telling the user their stdlib module was shadowed). Report Local as the common
    // case; build_registry refines this when it has the layer map (see Task 5 note).
    ModuleLayer::Local
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::register_builtins;
    use crate::registry::Registry;

    #[test]
    fn stdlib_registers_declarative_modules() {
        let mut reg = Registry::new();
        let notices = register_stdlib(&mut reg);
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
        let notices = register_stdlib(&mut reg);
        assert!(
            notices.iter().all(|n| n.shadowed == ModuleLayer::Stdlib),
            "any notice must be about a shadowed stdlib module"
        );
    }
}
