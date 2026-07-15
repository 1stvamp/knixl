//! knixl CLI. Every command is a thin policy over one Plan. Plan::compute is the only
//! thing that inspects the world. Exit codes are stable so CI can branch on them.
//! SPEC-GRADE SKETCH: Ctx::load and the write/report helpers are not written.

mod hub;
mod tui;

use std::io::IsTerminal;

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
    /// Open the interactive TUI (install, browse modules, scaffold a module).
    Tui,
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

        Cmd::Tui => unreachable!("tui is dispatched before Ctx::load"),
    }
}

/// `knixl install <pkg>`: resolve a host, draft the KDL edit, verify under nix, preview,
/// confirm, and regenerate. The host KDL is reverted on any failure or a declined confirm.
fn install(ctx: &Ctx, pkg: &str, host: Option<&str>, yes: bool, strict: bool) -> Code {
    use knixl_pipeline::install::{list_hosts, select_host};

    let hosts = match list_hosts(&ctx.root) {
        Ok(h) => h,
        Err(e) => { eprintln!("knixl: {e}"); return Code::Internal; }
    };
    let initial = match select_host(&hosts, host) {
        Ok(t) => t.clone(),
        Err(e) => { eprintln!("knixl: {e}"); return Code::Usage; }
    };
    let initial_idx = hosts.iter().position(|h| h.name == initial.name).unwrap_or(0);

    // `pkgs.<pkg>` existence is host-independent; resolve it once.
    let resolves = resolve_package(ctx, pkg);

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let chosen_idx = if interactive && !yes {
        match run_install_tui(ctx, pkg, &hosts, initial_idx, strict, resolves) {
            Ok(tui::Decision::Apply(i)) => i,
            Ok(tui::Decision::Cancel) => { println!("cancelled"); return Code::Clean; }
            Err(e) => { eprintln!("knixl: tui: {e}"); return Code::Internal; }
        }
    } else {
        // Non-interactive / --yes: the plain path. Hard-check the package here (the TUI
        // gates apply instead), then confirm unless --yes.
        match resolves {
            tui::Resolve::No => {
                eprintln!("knixl: no nixpkgs package named `{pkg}`");
                return Code::Validation;
            }
            tui::Resolve::Skipped if strict => {
                eprintln!("knixl: --strict: nix unavailable, cannot verify `{pkg}`");
                return Code::Validation;
            }
            tui::Resolve::Skipped => {
                eprintln!("warning: nix unavailable, skipping package check for `{pkg}`");
            }
            tui::Resolve::Yes => {}
        }
        if !yes && !confirm(&format!("install {pkg} on {}?", initial.name)) {
            println!("cancelled");
            return Code::Clean;
        }
        initial_idx
    };

    commit_install(&hosts[chosen_idx], pkg, strict)
}

/// Write the drafted package into the chosen host, verify it generates and parses, then
/// regenerate. Reverts the KDL on any failure. Shared by the TUI (after Apply) and the
/// plain path (after confirm).
fn commit_install(chosen: &knixl_pipeline::install::HostInfo, pkg: &str, strict: bool) -> Code {
    use knixl_pipeline::install::add_package;

    let original = match std::fs::read_to_string(&chosen.path) {
        Ok(s) => s,
        Err(e) => { eprintln!("knixl: {}: {e}", chosen.path.display()); return Code::Internal; }
    };
    let draft = match add_package(&original, pkg) {
        Ok(Some(d)) => d,
        Ok(None) => { println!("{pkg} is already installed on {}", chosen.name); return Code::Clean; }
        Err(e) => { eprintln!("knixl: cannot edit {}: {e}", chosen.path.display()); return Code::Internal; }
    };

    if let Err(e) = std::fs::write(&chosen.path, &draft) {
        eprintln!("knixl: {}: {e}", chosen.path.display());
        return Code::Internal;
    }
    let revert = || { let _ = std::fs::write(&chosen.path, &original); };

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
        println!("installed {pkg} on {}", chosen.name);
    } else {
        revert();
    }
    worst
}

/// Build the TUI state (initial preview for the selected host), run the loop, and return
/// the user's decision.
fn run_install_tui(
    ctx: &Ctx,
    pkg: &str,
    hosts: &[knixl_pipeline::install::HostInfo],
    initial_idx: usize,
    strict: bool,
    resolves: tui::Resolve,
) -> std::io::Result<tui::Decision> {
    let formatter = default_formatter();
    let tool: semver::Version = env!("CARGO_PKG_VERSION").parse().expect("tool version parses");

    let (nix0, parse0) = preview_host(&ctx.registry, &formatter, &tool, &hosts[initial_idx], pkg);
    let state = tui::InstallState {
        pkg: pkg.to_string(),
        hosts: hosts.to_vec(),
        selected: initial_idx,
        strict,
        resolves,
        parses: parse0,
        nix_preview: nix0,
    };
    let mut recompute = |s: &mut tui::InstallState| {
        let host = s.hosts[s.selected].clone();
        let (nix, parse) = preview_host(&ctx.registry, &formatter, &tool, &host, pkg);
        s.nix_preview = nix;
        s.parses = parse;
    };
    tui::run(state, &mut recompute)
}

/// Generate the drafted host in memory (no disk writes) and parse it, for the TUI preview.
fn preview_host(
    registry: &knixl_modules::Registry,
    formatter: &knixl_nix::Formatter,
    tool: &semver::Version,
    host: &knixl_pipeline::install::HostInfo,
    pkg: &str,
) -> (String, tui::Parse) {
    use knixl_pipeline::{generate, install::add_package, HostSource};
    let src = std::fs::read_to_string(&host.path).unwrap_or_default();
    let drafted = match add_package(&src, pkg) {
        Ok(Some(d)) => d,
        _ => src,
    };
    let nix = generate(
        &[HostSource { path: host.path.clone(), src: drafted }],
        registry,
        formatter,
        tool,
        None,
    )
    .ok()
    .and_then(|files| files.into_iter().map(|f| f.text).find(|t| t.contains("systemPackages")))
    .unwrap_or_else(|| "(preview unavailable)".to_string());

    let snippet = systempackages_snippet(&nix);
    (snippet, parse_text(&nix))
}

/// The `environment.systemPackages = [ ... ];` block, for a compact preview.
fn systempackages_snippet(nix: &str) -> String {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in nix.lines() {
        if line.contains("systemPackages") {
            in_block = true;
        }
        if in_block {
            out.push(line.trim_end());
            if line.contains("];") || (line.contains(';') && !line.contains('[')) {
                break;
            }
        }
    }
    if out.is_empty() {
        "(no packages)".to_string()
    } else {
        out.join("\n")
    }
}

/// Parse a generated file's text with nix, mapping to the TUI's `Parse` status.
fn parse_text(nix: &str) -> tui::Parse {
    use knixl_nix::nixeval::{NixError, NixEval};
    let tmp = std::env::temp_dir().join(format!("knixl-tui-parse-{}.nix", std::process::id()));
    if std::fs::write(&tmp, nix).is_err() {
        return tui::Parse::Skipped;
    }
    let verdict = NixEval::resolve().parses(&tmp);
    let _ = std::fs::remove_file(&tmp);
    match verdict {
        Ok(()) => tui::Parse::Ok,
        Err(NixError::Unavailable(_)) => tui::Parse::Skipped,
        Err(NixError::Failed(m)) => tui::Parse::Failed(m),
    }
}

/// `pkgs.<pkg>` existence against the lock's pinned rev (ambient fallback), as a `Resolve`.
fn resolve_package(ctx: &Ctx, pkg: &str) -> tui::Resolve {
    use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
    let rev = &ctx.lock.oracle.nixpkgs_rev;
    let src = if rev.is_empty() { Nixpkgs::Ambient } else { Nixpkgs::PinnedRev(rev.clone()) };
    match NixEval::resolve().package_exists(&src, pkg) {
        Ok(true) => tui::Resolve::Yes,
        Ok(false) => tui::Resolve::No,
        Err(NixError::Unavailable(_)) => tui::Resolve::Skipped,
        Err(NixError::Failed(_)) => tui::Resolve::No,
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
    let code = std::panic::catch_unwind(dispatch).unwrap_or(Code::Internal);
    std::process::exit(code as i32);
}

fn dispatch() -> Code {
    let cli = Cli::parse();
    // The TUI does not reconcile a project up front (Home works anywhere), so it skips
    // Ctx::load and its formatter requirement.
    if matches!(cli.cmd, Cmd::Tui) {
        return match hub::run(discover_root()) {
            Ok(()) => Code::Clean,
            Err(e) => { eprintln!("knixl: {e}"); Code::Internal }
        };
    }
    run(cli, &Ctx::load())
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

/// The pinned formatter. `KNIXL_FORMATTER` overrides the binary (e.g. `cat` in tests);
/// otherwise the binary is autodetected among the known names for the RFC-style nixfmt.
/// The recorded formatter *name* stays `nixfmt-rfc-style` regardless of the binary file
/// name, so the lock is stable across machines.
fn default_formatter() -> knixl_nix::Formatter {
    let bin = choose_formatter_bin(std::env::var("KNIXL_FORMATTER").ok(), formatter_runs);
    knixl_nix::Formatter::detect("nixfmt-rfc-style", bin.into(), "0.6.0")
}

/// Pick the formatter binary. `KNIXL_FORMATTER` wins; otherwise the first candidate that
/// runs (the packaged binary is `nixfmt`, but some setups expose `nixfmt-rfc-style`); if
/// none run, `nixfmt` so the not-found error names the usual binary.
fn choose_formatter_bin(env_override: Option<String>, runs: impl Fn(&str) -> bool) -> String {
    if let Some(bin) = env_override {
        return bin;
    }
    for cand in ["nixfmt", "nixfmt-rfc-style"] {
        if runs(cand) {
            return cand.to_string();
        }
    }
    "nixfmt".to_string()
}

/// Whether `bin --version` runs successfully (used to probe candidate formatter names).
fn formatter_runs(bin: &str) -> bool {
    std::process::Command::new(bin)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::choose_formatter_bin;

    #[test]
    fn env_override_wins() {
        assert_eq!(choose_formatter_bin(Some("cat".into()), |_| true), "cat");
    }

    #[test]
    fn prefers_nixfmt_when_present() {
        assert_eq!(choose_formatter_bin(None, |_| true), "nixfmt");
    }

    #[test]
    fn falls_back_to_alternative_name() {
        // nixfmt not found, but the nixfmt-rfc-style binary is.
        assert_eq!(choose_formatter_bin(None, |b| b == "nixfmt-rfc-style"), "nixfmt-rfc-style");
    }

    #[test]
    fn defaults_to_nixfmt_when_none_run() {
        assert_eq!(choose_formatter_bin(None, |_| false), "nixfmt");
    }
}
