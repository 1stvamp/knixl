//! knixl CLI. Every command is a thin policy over one Plan. Plan::compute is the only
//! thing that inspects the world. Exit codes are stable so CI can branch on them.
//! SPEC-GRADE SKETCH: Ctx::load and the write/report helpers are not written.

use clap::{Parser, Subcommand};
use knixl_lock::{FileState, Plan};

#[derive(Parser)]
#[command(name = "knixl", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// Machine-readable output.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Recompute and report; write nothing.
    Plan { #[arg(long)] detailed_exitcode: bool },
    /// CI gate: succeed only if every file is Clean. Never writes, never prompts.
    Check,
    /// Apply. Silent for Stale/Missing; refuses Drifted/skew without the matching flag.
    Generate { #[arg(long)] accept_drift: bool, #[arg(long)] prune: bool },
    /// Version bump: show migration notes + diff, apply on --yes, then bump the lock.
    Upgrade { #[arg(long)] yes: bool },
    /// Print the typed reference for a module node (from schema()).
    Doc { node: String },
    /// Add a package to a host: draft the KDL, verify under nix, preview, then regenerate.
    Install {
        /// The nixpkgs attribute name, e.g. ripgrep.
        pkg: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        yes: bool,
        /// Treat a skipped nix check (nix absent) as an error.
        #[arg(long)]
        strict: bool,
    },
}

#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Code {
    Clean = 0,
    Internal = 1,
    // clap owns exit 2 on arg-parse failure; the variant reserves the documented code.
    #[allow(dead_code)]
    Usage = 2,
    Drift = 3,
    NeedsAck = 4,
    Validation = 5,
    RegenPending = 6,
}

/// Precedence, most severe first. Spelled out because severity != numeric order.
/// Validation beats all (cannot trust a plan on invalid input). Drift beats skew
/// (silent overwrite loses human edits). Skew beats plain regen-pending.
fn verdict(plan: &Plan) -> Code {
    if plan.has_validation_errors() { return Code::Validation; }
    if plan.any(FileState::is_drifted) { return Code::Drift; }
    if plan.requires_ack() { return Code::NeedsAck; }
    if plan.any(FileState::is_dirty) { return Code::RegenPending; }
    Code::Clean
}

fn run(cli: Cli, ctx: &Ctx) -> Code {
    // Plan::compute is pure; validation errors ride on the plan (verdict maps them to
    // the Validation exit code), so there is no fallible generation step here.
    let plan = Plan::compute(&ctx.inputs, &ctx.disk, &ctx.lock, &ctx.running);
    if plan.has_validation_errors() {
        report_validation(&plan.validation_errors, cli.json);
        return Code::Validation;
    }

    // Non-fatal generation lints. Reported for every command that acts on the plan; `doc`
    // does not reconcile a project, so it stays quiet.
    if !matches!(cli.cmd, Cmd::Doc { .. }) {
        report_warnings(&ctx.warnings);
    }

    match cli.cmd {
        Cmd::Plan { detailed_exitcode } => {
            print_plan(&plan, cli.json);
            if detailed_exitcode { verdict(&plan) } else { Code::Clean }
        }

        Cmd::Check => { print_plan(&plan, cli.json); verdict(&plan) }

        Cmd::Generate { accept_drift, prune } => {
            // A version bump must go through `upgrade`, never a side effect of generate.
            // Drift is NOT this gate: it is handled per file below as exit 3.
            if plan.skew_needs_ack() {
                report_skew(&plan, cli.json);
                eprintln!("version skew present: run `knixl upgrade` to review and apply");
                return Code::NeedsAck;
            }
            let mut worst = Code::Clean;
            for f in &plan.files {
                match &f.state {
                    FileState::Clean => {}
                    FileState::Stale { .. } | FileState::Missing { .. } => write_file(ctx, f),
                    FileState::Drifted { .. } if accept_drift => write_file(ctx, f),
                    FileState::Drifted { .. } => { report_taint(f, cli.json); worst = worst.max(Code::Drift); }
                    FileState::Orphaned if prune => delete_file(ctx, f),
                    FileState::Orphaned => { note_orphan(f, cli.json); worst = worst.max(Code::RegenPending); }
                }
            }
            // Commit the lock ONLY on a clean apply, so it never lies about disk.
            if worst == Code::Clean { write_lock(ctx, &plan.lock_next); }
            worst
        }

        Cmd::Upgrade { yes } => {
            if !plan.requires_ack() && !plan.any(FileState::is_dirty) {
                println!("already up to date");
                return Code::Clean;
            }
            print_migration_notes(&plan, &ctx.registry); // per (module, version delta)
            print_plan(&plan, cli.json);
            if !yes { eprintln!("re-run with --yes to apply"); return Code::NeedsAck; }
            for f in &plan.files {
                if !matches!(f.state, FileState::Clean) { write_file(ctx, f); }
            }
            write_lock(ctx, &plan.lock_next); // bump tool/module/formatter/oracle together
            Code::Clean
        }

        Cmd::Doc { node } => { print_doc(ctx, &node, cli.json); Code::Clean }

        Cmd::Install { pkg, host, yes, strict } => install(ctx, &pkg, host.as_deref(), yes, strict),
    }
}

/// `knixl install <pkg>`: resolve a host, draft the KDL edit, verify under nix, preview,
/// confirm, and regenerate. The host KDL is reverted on any failure or a declined confirm.
fn install(ctx: &Ctx, pkg: &str, host: Option<&str>, yes: bool, strict: bool) -> Code {
    use knixl_pipeline::install::{add_package, list_hosts, select_host};

    let hosts = match list_hosts(&ctx.root) {
        Ok(h) => h,
        Err(e) => { eprintln!("knixl: {e}"); return Code::Internal; }
    };
    let target = match select_host(&hosts, host) {
        Ok(t) => t.clone(),
        Err(e) => { eprintln!("knixl: {e}"); return Code::Usage; }
    };

    let original = match std::fs::read_to_string(&target.path) {
        Ok(s) => s,
        Err(e) => { eprintln!("knixl: {}: {e}", target.path.display()); return Code::Internal; }
    };
    let draft = match add_package(&original, pkg) {
        Ok(Some(d)) => d,
        Ok(None) => { println!("{pkg} is already installed on {}", target.name); return Code::Clean; }
        Err(e) => { eprintln!("knixl: cannot edit {}: {e}", target.path.display()); return Code::Internal; }
    };

    // Package existence first, before touching any file.
    if let Some(code) = verify_package(ctx, pkg, strict) {
        return code;
    }

    // Write the draft, then verify it generates and parses; revert on any failure.
    if let Err(e) = std::fs::write(&target.path, &draft) {
        eprintln!("knixl: {}: {e}", target.path.display());
        return Code::Internal;
    }
    let revert = || { let _ = std::fs::write(&target.path, &original); };

    let drafted = Ctx::load();
    let plan = Plan::compute(&drafted.inputs, &drafted.disk, &drafted.lock, &drafted.running);
    if plan.has_validation_errors() {
        report_validation(&plan.validation_errors, false);
        revert();
        return Code::Validation;
    }
    if let Some(code) = verify_parse(&drafted, strict) {
        revert();
        return code;
    }

    // Preview.
    println!("\n{}", target.path.display());
    println!("+   package \"{pkg}\"\n");
    print_plan(&plan, false);

    if !yes && !confirm(&format!("install {pkg} on {}?", target.name)) {
        revert();
        println!("cancelled");
        return Code::Clean;
    }

    // Apply: regenerate changed outputs and commit the lock.
    let mut worst = Code::Clean;
    for f in &plan.files {
        match &f.state {
            FileState::Stale { .. } | FileState::Missing { .. } => write_file(&drafted, f),
            FileState::Drifted { .. } => { report_taint(f, false); worst = worst.max(Code::Drift); }
            FileState::Clean | FileState::Orphaned => {}
        }
    }
    if worst == Code::Clean {
        write_lock(&drafted, &plan.lock_next);
        println!("installed {pkg} on {}", target.name);
    } else {
        revert();
    }
    worst
}

/// Check `pkgs.<pkg>` resolves against the lock's pinned rev (ambient fallback). Returns
/// `Some(code)` to stop, `None` to proceed. A missing nix is a warning unless `--strict`.
fn verify_package(ctx: &Ctx, pkg: &str, strict: bool) -> Option<Code> {
    use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
    let rev = &ctx.lock.oracle.nixpkgs_rev;
    let src = if rev.is_empty() { Nixpkgs::Ambient } else { Nixpkgs::PinnedRev(rev.clone()) };
    match NixEval::resolve().package_exists(&src, pkg) {
        Ok(true) => None,
        Ok(false) => { eprintln!("knixl: no nixpkgs package named `{pkg}`"); Some(Code::Validation) }
        Err(NixError::Unavailable(_)) if strict => {
            eprintln!("knixl: --strict: nix unavailable, cannot verify `{pkg}`");
            Some(Code::Validation)
        }
        Err(NixError::Unavailable(_)) => {
            eprintln!("warning: nix unavailable, skipping package check for `{pkg}`");
            None
        }
        Err(NixError::Failed(m)) => { eprintln!("knixl: nix check failed: {m}"); Some(Code::Validation) }
    }
}

/// Parse the drafted generated files. `Some(code)` to stop, `None` to proceed. A missing
/// nix skips silently (the package step already reported it).
fn verify_parse(ctx: &Ctx, _strict: bool) -> Option<Code> {
    use knixl_nix::nixeval::{NixError, NixEval};
    let nix = NixEval::resolve();
    for (path, text) in &ctx.generated {
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("out.nix");
        let tmp = std::env::temp_dir().join(format!("knixl-parse-{}-{name}", std::process::id()));
        if std::fs::write(&tmp, text).is_err() { continue; }
        let verdict = nix.parses(&tmp);
        let _ = std::fs::remove_file(&tmp);
        match verdict {
            Ok(()) => {}
            Err(NixError::Unavailable(_)) => return None,
            Err(NixError::Failed(m)) => {
                eprintln!("knixl: generated {} does not parse: {m}", path.display());
                return Some(Code::Validation);
            }
        }
    }
    None
}

fn confirm(prompt: &str) -> bool {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok()
        && matches!(line.trim().chars().next(), Some('y') | Some('Y'))
}

fn main() {
    // A panic anywhere in planning or applying maps to the Internal exit code (docs/05),
    // so callers get a stable code instead of a raw abort.
    let code = std::panic::catch_unwind(|| run(Cli::parse(), &Ctx::load()))
        .unwrap_or(Code::Internal);
    std::process::exit(code as i32);
}

// ---- everything below: NOT written. Wiring for the next session. ----

struct Ctx {
    inputs: knixl_lock::reconcile::Inputs,
    disk: knixl_lock::reconcile::DiskState,
    lock: knixl_lock::Lock,
    running: knixl_lock::reconcile::Versions,
    registry: knixl_modules::Registry,
    root: std::path::PathBuf,
    generated: std::collections::BTreeMap<std::path::PathBuf, String>,
    warnings: Vec<String>,
}
impl Ctx {
    fn load() -> Ctx {
        let root = discover_root();
        let tool = env!("CARGO_PKG_VERSION").parse().expect("tool version parses");
        let project = knixl_pipeline::gather::gather(&root, &default_formatter(), tool)
            .unwrap_or_else(|e| {
                eprintln!("knixl: {e}");
                std::process::exit(Code::Internal as i32);
            });
        Ctx {
            inputs: project.inputs,
            disk: project.disk,
            lock: project.lock,
            running: project.versions,
            registry: project.registry,
            root: project.root,
            generated: project.generated,
            warnings: project.warnings,
        }
    }
}

/// Walk up from the working directory to the first dir holding a lock or a `hosts/`.
fn discover_root() -> std::path::PathBuf {
    let start = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mut dir = start.as_path();
    loop {
        if dir.join("knixl.lock.kdl").exists() || dir.join("hosts").is_dir() {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return start,
        }
    }
}

/// The pinned formatter. `KNIXL_FORMATTER` overrides the binary (e.g. `cat` in tests).
fn default_formatter() -> knixl_nix::Formatter {
    let bin = std::env::var("KNIXL_FORMATTER").unwrap_or_else(|_| "nixfmt-rfc-style".into());
    knixl_nix::Formatter::detect("nixfmt-rfc-style", bin.into(), "0.6.0")
}

fn state_label(state: &FileState) -> &'static str {
    match state {
        FileState::Clean => "clean",
        FileState::Stale { .. } => "stale",
        FileState::Drifted { .. } => "drifted",
        FileState::Missing { .. } => "missing",
        FileState::Orphaned => "orphaned",
    }
}

fn print_plan(p: &Plan, json: bool) {
    if json {
        let files: Vec<String> = p
            .files
            .iter()
            .map(|f| format!("{{\"path\":{:?},\"state\":{:?}}}", f.path.display().to_string(), state_label(&f.state)))
            .collect();
        println!("{{\"files\":[{}]}}", files.join(","));
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
fn print_migration_notes(plan: &Plan, registry: &knixl_modules::Registry) {
    // Module deltas are identical across files; read them from the first skew present.
    let Some(skew) = plan.files.iter().find_map(|f| f.skew.as_ref()) else {
        println!("(no migration notes)");
        return;
    };
    let mut printed = false;
    for (name, delta) in &skew.modules {
        let Some(module) = registry.get(name) else { continue };
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

fn print_doc(ctx: &Ctx, node: &str, _json: bool) {
    match ctx.registry.get(node) {
        Some(m) => print!("{}", m.schema().render_doc(node)),
        None => eprintln!("no module claims node `{node}`"),
    }
}

fn report_validation(errors: &[String], _json: bool) {
    for e in errors {
        eprintln!("validation: {e}");
    }
}

fn report_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

fn report_skew(_p: &Plan, _json: bool) {
    eprintln!("version skew: recorded versions differ from the running tool; run `knixl upgrade`");
}

fn report_taint(f: &knixl_lock::FilePlan, _json: bool) {
    eprintln!("drift: {} was hand-edited; refusing to overwrite (use --accept-drift)", f.path.display());
}

fn note_orphan(f: &knixl_lock::FilePlan, _json: bool) {
    eprintln!("orphan: {} is no longer generated (use --prune to delete)", f.path.display());
}

/// Write the freshly generated content for `f` to disk, creating parent directories.
fn write_file(ctx: &Ctx, f: &knixl_lock::FilePlan) {
    let target = ctx.root.join(&f.path);
    let Some(text) = ctx.generated.get(&f.path) else {
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

fn delete_file(ctx: &Ctx, f: &knixl_lock::FilePlan) {
    let target = ctx.root.join(&f.path);
    if let Err(e) = std::fs::remove_file(&target) {
        eprintln!("knixl: {}: {e}", target.display());
    }
}

fn write_lock(ctx: &Ctx, lock: &knixl_lock::Lock) {
    let target = ctx.root.join("knixl.lock.kdl");
    if let Err(e) = std::fs::write(&target, lock.render()) {
        eprintln!("knixl: {}: {e}", target.display());
    }
}
