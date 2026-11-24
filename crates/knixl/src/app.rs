//! Connecting the TUI to the commands: what each screen calls, and what its outcome commits.

use crate::install::InstallHooks;
use std::collections::BTreeMap;

use knixl_lock::model::HostBaseline;

use crate::discover_root;
use crate::install::commit_tui_install;
use crate::install::make_verify;
use crate::install::tui_session;
use crate::resolve::resolve_pending_baseline;
use crate::tui;
use crate::updates::commit_tui_updates;
use crate::updates::updates_report;
use crate::Code;
use crate::Ctx;

/// Open the TUI for the given entry: discover the project, list hosts, and inject a verify
/// function that (off the event-loop thread) drafts the host and checks it under nix.
pub(crate) fn open_tui(
    entry: tui::Entry,
    (build, pin, strategy): InstallHooks,
) -> Result<tui::Outcome, String> {
    use knixl_pipeline::install::list_hosts;
    let root = discover_root();
    let hosts = list_hosts(&root).map_err(|e| e.to_string())?;
    let modules = browse_modules(&root);
    let updates: Option<tui::UpdatesFn> = root.join("generated/flake.nix").is_file().then(|| {
        let root = root.clone();
        std::sync::Arc::new(move || updates_report(&root)) as tui::UpdatesFn
    });
    tui::run(
        entry,
        root.clone(),
        hosts,
        make_verify(root.clone()),
        modules,
        build,
        pin,
        strategy,
        updates,
    )
}

/// Enumerate registered modules for the Browse screen: node name, kind tag, rendered schema
/// doc, and a host-insertion skeleton. Built here (not in the TUI) since the registry is not
/// `Send`. An unreadable project yields an empty list rather than failing the whole TUI.
fn browse_modules(root: &std::path::Path) -> Vec<tui::BrowseModule> {
    use knixl_modules::ModuleKind;
    let Ok(registry) = knixl_pipeline::gather::registry(root) else {
        return Vec::new();
    };
    let manifests = declarative_manifests(root);
    registry
        .entries()
        .map(|(node, m)| {
            let schema = m.schema();
            tui::BrowseModule {
                node: node.to_string(),
                kind: match m.kind() {
                    ModuleKind::Builtin => "built-in".to_string(),
                    ModuleKind::Declarative => "declarative".to_string(),
                },
                doc: schema.render_doc(node),
                skeleton: skeleton_for(node, schema),
                manifest: manifests.get(node).cloned(),
            }
        })
        .collect()
}

/// Maps each declarative module's claimed node to its manifest path, for the Browse screen's
/// edit action. The registry itself does not carry the path (`DeclarativeModule` only keeps it
/// for error messages), so this walks `root/modules/*/knixl-module.kdl` directly and pairs each
/// manifest to the node it claims. An unreadable directory, or a manifest that fails to parse,
/// is skipped rather than failing the whole scan: Browse still shows a usable list, just without
/// an edit path for that one entry.
fn declarative_manifests(
    root: &std::path::Path,
) -> std::collections::BTreeMap<String, std::path::PathBuf> {
    let mut out = std::collections::BTreeMap::new();
    let dir = root.join("modules");
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in read_dir.filter_map(|e| e.ok()) {
        let manifest = entry.path().join("knixl-module.kdl");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        if let Ok(editable) = knixl_modules::authoring::load_editable(&text) {
            out.insert(editable.node, manifest);
        }
    }
    out
}

/// A starting skeleton for inserting a module node into a host: the node with placeholders
/// for its required positional args and required props, and an empty `{ }` block if it takes
/// children. A starting point the user then edits, not a guaranteed-valid node.
fn skeleton_for(node: &str, schema: &knixl_modules::NodeSchema) -> String {
    use knixl_modules::ValueTy;
    fn placeholder(ty: &ValueTy) -> String {
        match ty {
            ValueTy::Bool => "#true".to_string(),
            ValueTy::Int => "0".to_string(),
            ValueTy::Str | ValueTy::Node => "\"\"".to_string(),
            ValueTy::Enum(opts) => opts
                .first()
                .map(|o| format!("\"{o}\""))
                .unwrap_or_else(|| "\"\"".to_string()),
        }
    }

    let mut head = node.to_string();
    for arg in schema.args.iter().filter(|a| a.required) {
        head.push(' ');
        head.push_str(&placeholder(&arg.ty));
    }
    for prop in schema.props.iter().filter(|p| p.required) {
        head.push_str(&format!(" {}={}", prop.name, placeholder(&prop.ty)));
    }

    let has_block = schema.open_children || schema.children.iter().any(|c| c.required);
    if has_block {
        format!("{head} {{\n}}")
    } else {
        head
    }
}

/// Act on what the hub session decided: an Install outcome commits the package; a plain quit
/// or cancel does nothing.
pub(crate) fn finish_tui_outcome(outcome: tui::Outcome) -> Code {
    match outcome {
        // `knixl tui` (the Hub flow) has no `--no-abi-check` flag: `Entry::Hub` seeds it
        // `false` in `InstallModel::enter`, matching prior behaviour. The Hub flow also has
        // no `run` pre-pass (`Cmd::Tui` is dispatched before `Ctx::load`), so it resolves
        // this one host's pending baseline itself before committing (issue #22 review fix:
        // this used to be skipped entirely for an ambient install here). It also never
        // requests a version (`Entry::Hub` seeds `version: None`), so `strategy` and
        // `strategy_reason` are always `None` here too.
        tui::Outcome::Install {
            host,
            pkg,
            strict,
            version,
            pin,
            no_abi_check,
            strategy,
            strategy_reason,
        } => {
            let ctx = Ctx::load();
            let baseline_pending =
                match resolve_pending_baseline(&ctx, &host.name, &host.path, false, &[]) {
                    Ok(b) => b,
                    Err(code) => return code,
                };
            commit_tui_install(
                host,
                pkg,
                strict,
                version,
                pin,
                no_abi_check,
                strategy,
                strategy_reason,
                baseline_pending,
            )
        }
        tui::Outcome::Insert {
            host,
            node,
            skeleton,
        } => commit_insert(&host, &node, &skeleton),
        tui::Outcome::Scaffold { name, manifest } => commit_scaffold(&name, &manifest),
        tui::Outcome::SaveModule { path, text } => commit_save_module(&path, &text),
        tui::Outcome::Updates {
            refresh,
            if_security,
            pins,
        } => commit_tui_updates(refresh, if_security, &pins),
        tui::Outcome::Cancelled | tui::Outcome::Quit => Code::Clean,
    }
}

/// Write a scaffolded module manifest to `modules/<name>/knixl-module.kdl`, refusing to
/// overwrite an existing module.
fn commit_scaffold(name: &str, manifest: &str) -> Code {
    let dir = discover_root().join("modules").join(name);
    let path = dir.join("knixl-module.kdl");
    if path.exists() {
        eprintln!(
            "knixl: module `{name}` already exists at {}",
            path.display()
        );
        return Code::Validation;
    }
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("knixl: {}: {e}", dir.display());
        return Code::Internal;
    }
    if let Err(e) = std::fs::write(&path, manifest) {
        eprintln!("knixl: {}: {e}", path.display());
        return Code::Internal;
    }
    println!(
        "created module `{name}`: edit {} then declare it on a host",
        path.display()
    );
    Code::Clean
}

/// Overwrite an existing module manifest with edited text (Edit mode). Unlike commit_scaffold
/// this expects the file to exist and replaces it.
fn commit_save_module(path: &std::path::Path, text: &str) -> Code {
    if let Err(e) = std::fs::write(path, text) {
        eprintln!("knixl: {}: {e}", path.display());
        return Code::Internal;
    }
    println!("updated {}", path.display());
    Code::Clean
}

/// Scaffold a module node into a host's KDL: splice the skeleton and write the file. Unlike
/// install this does not regenerate, since the skeleton is a starting point the user edits
/// before running `knixl generate`.
fn commit_insert(host: &knixl_pipeline::install::HostInfo, node: &str, skeleton: &str) -> Code {
    use knixl_pipeline::install::add_node;
    let original = match std::fs::read_to_string(&host.path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("knixl: {}: {e}", host.path.display());
            return Code::Internal;
        }
    };
    match add_node(&original, node, skeleton) {
        Ok(Some(draft)) => {
            if let Err(e) = std::fs::write(&host.path, &draft) {
                eprintln!("knixl: {}: {e}", host.path.display());
                return Code::Internal;
            }
            println!(
                "added {node} to {}: edit {} then run `knixl generate`",
                host.name,
                host.path.display()
            );
            Code::Clean
        }
        Ok(None) => {
            println!("{node} is already declared on {}", host.name);
            Code::Clean
        }
        Err(e) => {
            eprintln!("knixl: cannot edit {}: {e}", host.path.display());
            Code::Internal
        }
    }
}

/// `knixl install` in a terminal without `--yes`: open the TUI on the package and commit what
/// it decides.
pub(crate) fn install_in_tui(
    ctx: &Ctx,
    pkg: &str,
    host: Option<&str>,
    strict: bool,
    build: bool,
    no_abi_check: bool,
    pending: &BTreeMap<String, HostBaseline>,
) -> Code {
    let (entry, hooks) = match tui_session(ctx, pkg, host, strict, build, no_abi_check, pending) {
        Ok(s) => s,
        Err(code) => return code,
    };
    match open_tui(entry, hooks) {
        Ok(tui::Outcome::Install {
            host,
            pkg,
            strict,
            version,
            pin,
            no_abi_check,
            strategy,
            strategy_reason,
        }) => {
            // The TUI may switch the target host: look the pending resolution up for
            // whichever host was actually chosen.
            let baseline_pending = pending.get(&host.name).cloned();
            commit_tui_install(
                host,
                pkg,
                strict,
                version,
                pin,
                no_abi_check,
                strategy,
                strategy_reason,
                baseline_pending,
            )
        }
        Ok(_) => {
            println!("cancelled");
            Code::Clean
        }
        Err(e) => {
            eprintln!("knixl: tui: {e}");
            Code::Internal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::commit_save_module;
    use crate::Code;

    #[test]
    fn commit_save_module_overwrites_an_existing_file() {
        let path = std::env::temp_dir().join(format!(
            "knixl-cli-commit-save-module-{}.kdl",
            std::process::id()
        ));
        std::fs::write(&path, "original").unwrap();
        let code = commit_save_module(&path, "updated text");
        assert!(code == Code::Clean, "commit_save_module writes cleanly");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated text");
        let _ = std::fs::remove_file(&path);
    }
}
