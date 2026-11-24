//! What commands print and write: plans, reports, generated files and the lock.

use crate::Code;
use crate::Ctx;
use knixl_lock::{FileState, Plan};

fn state_label(state: &FileState) -> &'static str {
    match state {
        FileState::Clean => "clean",
        FileState::Stale { .. } => "stale",
        FileState::Drifted { .. } => "drifted",
        FileState::Missing { .. } => "missing",
        FileState::Orphaned => "orphaned",
    }
}

pub(crate) fn print_plan(p: &Plan, json: bool, warnings: &[String]) {
    if json {
        let files: Vec<String> = p
            .files
            .iter()
            .map(|f| {
                format!(
                    "{{\"path\":{},\"state\":{}}}",
                    json_str(&f.path.display().to_string()),
                    json_str(state_label(&f.state))
                )
            })
            .collect();
        // Warnings ride in the object so CI can fail on them: they only existed on stderr
        // before, which left `--json` unable to report anything but file states (#85).
        let warned: Vec<String> = warnings.iter().map(|w| json_str(w)).collect();
        println!(
            "{{\"files\":[{}],\"warnings\":[{}]}}",
            files.join(","),
            warned.join(",")
        );
        return;
    }
    if p.files.is_empty() {
        println!("no generated files tracked");
        return;
    }
    for f in &p.files {
        println!("{:>8}  {}", state_label(&f.state), f.path.display());
    }
}
pub(crate) fn print_migration_notes(plan: &Plan, registry: &knixl_modules::Registry) {
    // Module deltas are identical across files; read them from the first skew present.
    let Some(skew) = plan.files.iter().find_map(|f| f.skew.as_ref()) else {
        println!("(no migration notes)");
        return;
    };
    let mut printed = false;
    for (name, delta) in &skew.modules {
        let Some(module) = registry.get(name) else {
            continue;
        };
        let notes = module.migration_notes(&delta.locked, &delta.running);
        if notes.is_empty() {
            continue;
        }
        println!("{name} {} -> {}:", delta.locked, delta.running);
        for n in &notes {
            println!("  - {n}");
        }
        printed = true;
    }
    if !printed {
        println!("(no migration notes)");
    }
}

pub(crate) fn print_doc(registry: &knixl_modules::Registry, node: &str) -> Code {
    match registry.get(node) {
        Some(m) => {
            print!("{}", m.schema().render_doc(node));
            Code::Clean
        }
        None => {
            eprintln!("no module claims node `{node}`");
            Code::Clean
        }
    }
}

pub(crate) fn report_validation(errors: &[String], json: bool) {
    if json {
        // Emitted on stdout as the run's only object: this path returns Code::Validation
        // immediately, so no plan object follows it.
        let errs: Vec<String> = errors.iter().map(|e| json_str(e)).collect();
        println!("{{\"validation\":[{}]}}", errs.join(","));
    }
    for e in errors {
        eprintln!("validation: {e}");
    }
}

/// A JSON string literal. `{:?}` on a Rust string is close but emits `\u{XX}` for some
/// characters, which no JSON parser accepts, and these strings carry user-supplied node names.
fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub(crate) fn report_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

pub(crate) fn report_skew(_p: &Plan, _json: bool) {
    eprintln!("version skew: recorded versions differ from the running tool; run `knixl upgrade`");
}

pub(crate) fn report_taint(f: &knixl_lock::FilePlan, _json: bool) {
    eprintln!(
        "drift: {} was hand-edited; refusing to overwrite (use --accept-drift)",
        f.path.display()
    );
}

pub(crate) fn note_orphan(f: &knixl_lock::FilePlan, _json: bool) {
    eprintln!(
        "orphan: {} is no longer generated (use --prune to delete)",
        f.path.display()
    );
}

/// Write the freshly generated content for `f` to disk, creating parent directories.
pub(crate) fn write_file(ctx: &Ctx, f: &knixl_lock::FilePlan) {
    write_generated(&ctx.root, &ctx.generated, f);
}

pub(crate) fn write_generated(
    root: &std::path::Path,
    generated: &std::collections::BTreeMap<std::path::PathBuf, String>,
    f: &knixl_lock::FilePlan,
) {
    let target = root.join(&f.path);
    let Some(text) = generated.get(&f.path) else {
        eprintln!("knixl: no generated content for {}", f.path.display());
        return;
    };
    if let Some(parent) = target.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("knixl: {}: {e}", parent.display());
            return;
        }
    }
    if let Err(e) = std::fs::write(&target, text) {
        eprintln!("knixl: {}: {e}", target.display());
    }
}

pub(crate) fn delete_file(ctx: &Ctx, f: &knixl_lock::FilePlan) {
    let target = ctx.root.join(&f.path);
    if let Err(e) = std::fs::remove_file(&target) {
        eprintln!("knixl: {}: {e}", target.display());
    }
}

pub(crate) fn write_lock(ctx: &Ctx, lock: &knixl_lock::Lock) {
    let target = ctx.root.join("knixl.lock.kdl");
    if let Err(e) = std::fs::write(&target, lock.render()) {
        eprintln!("knixl: {}: {e}", target.display());
    }
}
