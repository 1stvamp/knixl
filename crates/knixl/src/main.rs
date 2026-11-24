//! knixl CLI entry point: argument parsing and the per-command policy functions.

use crate::app::finish_tui_outcome;
use crate::app::install_in_tui;
use crate::app::open_tui;
use crate::install::install;
use crate::output::delete_file;
use crate::output::note_orphan;
use crate::output::print_doc;
use crate::output::print_migration_notes;
use crate::output::print_plan;
use crate::output::report_skew;
use crate::output::report_taint;
use crate::output::report_validation;
use crate::output::report_warnings;
use crate::output::write_file;
use crate::output::write_generated;
use crate::output::write_lock;
use crate::resolve::build_pending_oracle_sets;
use crate::resolve::flake_input_module_pins;
use crate::resolve::module_source_pins;
use crate::resolve::options_hash_for_rev;
use crate::resolve::patch_inputs_for_pending;
use crate::resolve::print_resolved_module_sources;
use crate::resolve::resolve_pending_baselines;
use crate::resolve::resolve_pending_flake_inputs;
use crate::resolve::resolve_pending_host_modules;
use crate::resolve::resolve_pending_module_sources;
use crate::resolve::resolve_pending_project_modules;
use crate::resolve::resolve_pending_project_release;
use crate::resolve::write_module_source_caches;
use crate::resolve::PendingModuleSource;
use crate::updates::cmd_updates;
use crate::updates::updates_report;
mod app;
mod install;
mod output;
mod resolve;
mod tui;
mod updates;

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;

use clap::{Parser, Subcommand};
use knixl_lock::model::{FlakeInputPin, HostBaseline, OracleModulePin};
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
    /// Show what generate would change. Writes nothing.
    Plan {
        /// Exit with the same codes as check (non-zero when anything needs doing).
        #[arg(long)]
        detailed_exitcode: bool,
    },
    /// For CI: fail unless every generated file is up to date and unedited. Never writes.
    Check,
    /// Write the generated Nix and the lockfile.
    Generate {
        /// Regenerate files someone edited by hand, discarding their edits.
        #[arg(long)]
        accept_drift: bool,
        /// Delete generated files no host produces any more.
        #[arg(long)]
        prune: bool,
    },
    /// Review and apply version changes: knixl, modules, nixpkgs releases, flake inputs.
    Upgrade {
        /// Apply the changes (without it, only show them).
        #[arg(long)]
        yes: bool,
        /// Also move every nixpkgs release to the newest commit on its branch.
        #[arg(long)]
        refresh: bool,
        /// With --refresh: only refresh when the update fixes a CVE (see knixl updates).
        #[arg(long, requires = "refresh")]
        if_security: bool,
        /// With --if-security: only count fixes at or above this severity
        /// (low, medium, high or critical).
        #[arg(long, requires = "if_security", value_parser = ["low", "medium", "high", "critical"])]
        min_severity: Option<String>,
        /// With --refresh: move only as far as keeps a package's version matching a pattern,
        /// e.g. curl=8.19.* or linux=6.12.*, choosing among official channel releases.
        /// Repeatable.
        #[arg(long, requires = "refresh", value_name = "PACKAGE=PATTERN")]
        keep: Vec<String>,
    },
    /// Show what a module accepts: its arguments, properties and children.
    Doc {
        /// The node name, e.g. openssh.
        node: String,
    },
    /// Add a package to a host, check it with Nix, and regenerate.
    Install {
        /// The nixpkgs attribute name (e.g. ripgrep) or versioned form (e.g. ripgrep@13.0.0).
        pkg: String,
        /// The host to add it to (default: the host marked default=#true, else the only host).
        #[arg(long)]
        host: Option<String>,
        /// Don't ask for confirmation (and don't open the TUI).
        #[arg(long)]
        yes: bool,
        /// Fail, instead of warning, when Nix isn't available to check the package.
        #[arg(long)]
        strict: bool,
        /// Also build the package, to prove it builds as well as exists.
        #[arg(long)]
        build: bool,
        /// For a version pin, skip the trial build and always use the commit-mix strategy.
        #[arg(long)]
        no_abi_check: bool,
    },
    /// Open the interactive TUI: install a package, browse modules, create a module.
    Tui,
    /// Report what `upgrade --refresh` would change on each host: version changes and the
    /// security fixes among them. Needs a generated flake, Nix, and vulnix for the security part.
    Updates,
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

/// Precedence, most severe first: severity does not follow the numeric `Code` order. Drift
/// beats skew because a silent overwrite would lose a human edit.
fn verdict(plan: &Plan) -> Code {
    if plan.has_validation_errors() {
        return Code::Validation;
    }
    if plan.any(FileState::is_drifted) {
        return Code::Drift;
    }
    if plan.requires_ack() {
        return Code::NeedsAck;
    }
    if plan.any(FileState::is_dirty) {
        return Code::RegenPending;
    }
    Code::Clean
}

fn run(cli: Cli, ctx: &Ctx) -> Code {
    // `upgrade --refresh` (#129): re-resolve releases already in the lock to their branch tip.
    let refresh = matches!(cli.cmd, Cmd::Upgrade { refresh: true, .. });
    let keeps: Vec<(String, String)> = match &cli.cmd {
        Cmd::Upgrade { keep, .. } => {
            let mut out = Vec::new();
            for k in keep {
                match k.split_once('=') {
                    Some((p, v)) if !p.is_empty() && !v.is_empty() => {
                        out.push((p.to_string(), v.to_string()))
                    }
                    _ => {
                        eprintln!("knixl: --keep {k}: expected PACKAGE=PATTERN, e.g. curl=8.19.*");
                        return Code::Usage;
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    };
    // `--if-security`: decide from the update report whether the refresh is worth taking.
    if let Cmd::Upgrade {
        if_security: true,
        min_severity,
        ..
    } = &cli.cmd
    {
        let floor = min_severity.as_deref().unwrap_or("low");
        match updates_report(&ctx.root) {
            Ok(reports) => {
                let worst = reports
                    .iter()
                    .filter_map(|r| r.security.as_ref())
                    .flat_map(|s| s.fixed.iter().flat_map(|f| f.cves.iter()))
                    .map(|(_, score)| knixl_pipeline::updates::severity_rank(*score))
                    .max();
                if reports.iter().any(|r| r.security.is_none()) {
                    eprintln!("knixl: --if-security needs vulnix to tell, so nothing is refreshed");
                    return Code::Validation;
                }
                if worst.is_none_or(|w| w < knixl_pipeline::updates::severity_floor(floor)) {
                    println!("no security fixes at {floor} or above in the update, so nothing is refreshed");
                    return Code::Clean;
                }
            }
            Err(e) => {
                eprintln!("knixl: {e}");
                return Code::Internal;
            }
        }
    }
    // `install`/`upgrade` resolve a declared-but-unresolved nixpkgs baseline before the
    // validation gate, since they are its only remedy. Resolved in memory only; `pending` is
    // written by `Cmd::Upgrade` on `--yes`, or by `install`/`commit_install` in the confirmed,
    // revertable commit path.
    let pending: BTreeMap<String, HostBaseline> =
        if matches!(cli.cmd, Cmd::Upgrade { .. } | Cmd::Install { .. }) {
            match resolve_pending_baselines(ctx, refresh, &keeps) {
                Ok(p) => p,
                Err(code) => return code,
            }
        } else {
            BTreeMap::new()
        };

    // ADR 0014: flake inputs resolve on `upgrade` only; `install` never needs them, and an
    // unresolved input already refuses with a message pointing at `upgrade`.
    let pending_flake_inputs: Option<Vec<FlakeInputPin>> = if matches!(cli.cmd, Cmd::Upgrade { .. })
    {
        match resolve_pending_flake_inputs(ctx) {
            Ok(p) => p,
            Err(code) => return code,
        }
    } else {
        None
    };

    // The project's own `nixpkgs release` (knixl.kdl) resolves into the lock's `oracle` line on
    // `upgrade` only (#134): the rev hosts without a release validate against, and the one
    // image targets build from in the input-free flake.
    let pending_project_release: Option<(String, String)> =
        if matches!(cli.cmd, Cmd::Upgrade { .. }) {
            match resolve_pending_project_release(ctx, refresh, &keeps) {
                Ok(p) => p,
                Err(code) => return code,
            }
        } else {
            None
        };

    // Input-backed oracle modules (ADR 0014) take their pin from the flake inputs, pending first.
    let input_pins = flake_input_module_pins(
        &ctx.root,
        pending_flake_inputs
            .as_deref()
            .unwrap_or(&ctx.lock.flake_inputs),
    );

    // #35 phase 3: the project's declared `oracle-modules` (knixl.kdl) resolve the same way,
    // alongside the baseline pre-pass above (in memory only, never written here; see
    // `resolve_pending_project_modules`).
    let pending_modules: Option<Vec<OracleModulePin>> =
        if matches!(cli.cmd, Cmd::Upgrade { .. } | Cmd::Install { .. }) {
            match resolve_pending_project_modules(ctx, &input_pins) {
                Ok(p) => p,
                Err(code) => return code,
            }
        } else {
            None
        };

    // ADR 0008: a host's own `oracle-modules` override resolves the same way (in memory only;
    // see `resolve_pending_host_modules`), but can refuse the whole run since a host override
    // with no declared `nixpkgs release=` has nowhere in the lock to carry its pins.
    let pending_host_modules: BTreeMap<String, Vec<OracleModulePin>> =
        if matches!(cli.cmd, Cmd::Upgrade { .. } | Cmd::Install { .. }) {
            match resolve_pending_host_modules(ctx, &input_pins) {
                Ok(p) => p,
                Err(code) => return code,
            }
        } else {
            BTreeMap::new()
        };

    // Task 5b: declared `modules {}` sources (knixl.kdl) resolve the same way, alongside the
    // pre-passes above (in memory only, never written here; see
    // `resolve_pending_module_sources`).
    let pending_module_sources: Option<Vec<PendingModuleSource>> =
        if matches!(cli.cmd, Cmd::Upgrade { .. } | Cmd::Install { .. }) {
            match resolve_pending_module_sources(ctx) {
                Ok(p) => p,
                Err(code) => return code,
            }
        } else {
            None
        };

    // Plan off a lock patched with `pending` merged in, so the "not resolved: run knixl
    // upgrade" validation error does not block the very commands that would resolve it, and
    // `lock_next` (built from this patched lock) carries the pending baselines for
    // `Cmd::Upgrade` to write verbatim. `check`/`generate`/`plan` never populate `pending`, so
    // they always plan off the on-disk lock and inputs unchanged.
    let patched_lock = (!pending.is_empty()
        || !pending_host_modules.is_empty()
        || pending_module_sources.is_some()
        || pending_flake_inputs.is_some()
        || pending_project_release.is_some())
    .then(|| {
        let mut lock = ctx.lock.clone();
        if let Some((release, rev)) = &pending_project_release {
            lock.oracle.release = release.clone();
            lock.oracle.nixpkgs_rev = rev.clone();
            lock.oracle.options_hash = options_hash_for_rev(rev);
        }
        if let Some(pins) = &pending_flake_inputs {
            lock.flake_inputs = pins.clone();
        }
        lock.baselines
            .extend(pending.iter().map(|(host, b)| (host.clone(), b.clone())));
        // Overlay each host's pending module-override resolution onto its baseline (which the
        // extend above may just have created for a release resolved in this same run): the
        // baseline carries both fields together, so `plan.lock_next` (built from this patched
        // lock) writes the release and the module pins as one coherent host block.
        for (host, pins) in &pending_host_modules {
            lock.baselines
                .entry(host.clone())
                .or_insert_with(|| HostBaseline {
                    release: String::new(),
                    nixpkgs_rev: String::new(),
                    options_hash: String::new(),
                    modules: Vec::new(),
                })
                .modules = pins.clone();
        }
        // Task 5b: a freshly resolved (or GC'd) module-source set replaces `module_sources`
        // wholesale, exactly as `pending_modules` replaces `oracle.modules` wholesale, so
        // `build_lock_next` (which copies `lock.module_sources` straight from this patched
        // lock) carries the resolved pins through to `Cmd::Upgrade`'s/`commit_install`'s write.
        if let Some(pending) = &pending_module_sources {
            lock.module_sources = module_source_pins(pending);
        }
        lock
    });
    let patched_inputs = (!pending.is_empty() || pending_flake_inputs.is_some())
        .then(|| patch_inputs_for_pending(&ctx.inputs, &pending, pending_flake_inputs.is_some()));
    let lock_for_plan = patched_lock.as_ref().unwrap_or(&ctx.lock);
    // New flake-input pins and newly resolved baselines both change what gets generated (the
    // flake's text, and whether the hosts can share one nixpkgs at all), so plan from the
    // patched lock. Planning against the old one dropped the flake as orphaned and hid a
    // validation error the new baseline causes (#135).
    let regather =
        pending_flake_inputs.is_some() || !pending.is_empty() || pending_project_release.is_some();
    let regathered = match (regather, &patched_lock) {
        (true, Some(lock)) => match knixl_pipeline::gather::gather_with_lock(
            &ctx.root,
            &default_formatter(),
            ctx.running.tool.clone(),
            Some(lock.clone()),
        ) {
            Ok(p) => Some((
                patch_inputs_for_pending(&p.inputs, &pending, pending_flake_inputs.is_some()),
                p.generated,
            )),
            Err(e) => {
                eprintln!("knixl: {e}");
                return Code::Internal;
            }
        },
        _ => None,
    };
    let inputs_for_plan = regathered
        .as_ref()
        .map(|(inputs, _)| inputs)
        .or(patched_inputs.as_ref())
        .unwrap_or(&ctx.inputs);
    let generated_for_plan = regathered
        .as_ref()
        .map(|(_, generated)| generated)
        .unwrap_or(&ctx.generated);

    // `build_lock_next` (knixl-lock) sources the GLOBAL `oracle` pin from `running.oracle`,
    // not from the `lock` passed to `Plan::compute` (unlike per-host `baselines`, which it
    // takes from `lock` -- see `build_lock_next`): so `pending_modules` needs to land in a
    // patched `Versions`, not the patched lock above, for `plan.lock_next` to carry it through
    // to `Cmd::Upgrade`'s/`commit_install`'s write.
    let patched_running =
        (pending_modules.is_some() || pending_project_release.is_some()).then(|| {
            let mut oracle = ctx.running.oracle.clone();
            if let Some(pins) = &pending_modules {
                oracle.modules = pins.clone();
            }
            if let Some((release, rev)) = &pending_project_release {
                oracle.release = release.clone();
                oracle.nixpkgs_rev = rev.clone();
                oracle.options_hash = options_hash_for_rev(rev);
            }
            knixl_lock::reconcile::Versions {
                tool: ctx.running.tool.clone(),
                formatter: ctx.running.formatter.clone(),
                oracle,
                modules: ctx.running.modules.clone(),
            }
        });
    let running_for_plan = patched_running.as_ref().unwrap_or(&ctx.running);

    // Plan::compute is pure; validation errors ride on the plan (verdict maps them to
    // the Validation exit code), so there is no fallible generation step here.
    let plan = Plan::compute(inputs_for_plan, &ctx.disk, lock_for_plan, running_for_plan);
    if plan.has_validation_errors() {
        report_validation(&plan.validation_errors, cli.json);
        return Code::Validation;
    }

    // Non-fatal generation lints, reported for every command that acts on the plan.
    report_warnings(&ctx.warnings);

    match cli.cmd {
        Cmd::Plan { detailed_exitcode } => {
            print_plan(&plan, cli.json, &ctx.warnings);
            if detailed_exitcode {
                verdict(&plan)
            } else {
                Code::Clean
            }
        }

        Cmd::Check => {
            print_plan(&plan, cli.json, &ctx.warnings);
            if !ctx.flake_lock_problems.is_empty() {
                report_validation(&ctx.flake_lock_problems, cli.json);
                return Code::Validation;
            }
            verdict(&plan)
        }

        Cmd::Generate {
            accept_drift,
            prune,
        } => {
            // A version bump must go through `upgrade`, never a side effect of generate.
            // Drift is NOT this gate: it is handled per file below as exit 3.
            if plan.skew_needs_ack() {
                report_skew(&plan, cli.json);
                eprintln!("version skew present: run `knixl upgrade` to review and apply");
                return Code::NeedsAck;
            }
            // Precedence, not numeric order: drift (3) outranks a pending regeneration (6),
            // as in `verdict` (#135).
            let (mut drifted, mut orphaned) = (false, false);
            for f in &plan.files {
                match &f.state {
                    FileState::Clean => {}
                    FileState::Stale { .. } | FileState::Missing { .. } => write_file(ctx, f),
                    FileState::Drifted { .. } if accept_drift => write_file(ctx, f),
                    FileState::Drifted { .. } => {
                        report_taint(f, cli.json);
                        drifted = true;
                    }
                    FileState::Orphaned if prune => delete_file(ctx, f),
                    FileState::Orphaned => {
                        note_orphan(f, cli.json);
                        orphaned = true;
                    }
                }
            }
            let worst = if drifted {
                Code::Drift
            } else if orphaned {
                Code::RegenPending
            } else {
                Code::Clean
            };
            // Commit the lock ONLY on a clean apply, so it never lies about disk.
            if worst == Code::Clean {
                write_lock(ctx, &plan.lock_next);
            }
            worst
        }

        Cmd::Upgrade { yes, .. } => {
            // A pending baseline/module resolution is work to do even when every file is
            // Clean and nothing needs ack: skip the "up to date" short-circuit so it gets
            // previewed (and, on --yes, written) below instead of silently vanishing.
            if !plan.requires_ack()
                && !plan.any(FileState::is_dirty)
                && pending.is_empty()
                && pending_modules.is_none()
                && pending_host_modules.is_empty()
                && pending_module_sources.is_none()
                && pending_flake_inputs.is_none()
                && pending_project_release.is_none()
                && ctx.unvalidated.is_empty()
            {
                println!("already up to date");
                return Code::Clean;
            }
            print_migration_notes(&plan, &ctx.registry);
            print_plan(&plan, cli.json, &ctx.warnings);
            if !yes {
                for (host, b) in &pending {
                    println!(
                        "would resolve nixpkgs release \"{}\" for host \"{host}\" -> {}",
                        b.release, b.nixpkgs_rev,
                    );
                }
                for m in pending_modules.iter().flatten() {
                    println!(
                        "would resolve oracle module \"{}\" ({}) -> {}",
                        m.name, m.url, m.rev,
                    );
                }
                for (host, pins) in &pending_host_modules {
                    for m in pins {
                        println!(
                            "would resolve oracle module \"{}\" ({}) for host \"{host}\" -> {}",
                            m.name, m.url, m.rev,
                        );
                    }
                }
                if let Some(pending) = &pending_module_sources {
                    print_resolved_module_sources(pending, "would resolve");
                }
                for i in pending_flake_inputs.iter().flatten() {
                    println!(
                        "would resolve flake input \"{}\" ({}) -> {}",
                        i.name, i.url, i.rev,
                    );
                }
                if let Some((release, rev)) = &pending_project_release {
                    match release.is_empty() {
                        true => println!("would clear the project nixpkgs release"),
                        false => println!(
                            "would resolve the project nixpkgs release \"{release}\" -> {rev}"
                        ),
                    }
                }
                let missing: BTreeSet<&str> = ctx
                    .unvalidated
                    .iter()
                    .map(|(_, rev)| rev.as_str())
                    .collect();
                for rev in missing {
                    println!("would build the NixOS option set for nixpkgs {rev}");
                }
                eprintln!("re-run with --yes to apply");
                return Code::NeedsAck;
            }
            for f in &plan.files {
                if !matches!(f.state, FileState::Clean) {
                    write_generated(&ctx.root, generated_for_plan, f);
                }
            }
            for (host, b) in &pending {
                println!(
                    "resolved nixpkgs release \"{}\" for host \"{host}\" -> {}",
                    b.release, b.nixpkgs_rev,
                );
            }
            for m in pending_modules.iter().flatten() {
                println!(
                    "resolved oracle module \"{}\" ({}) -> {}",
                    m.name, m.url, m.rev,
                );
            }
            for (host, pins) in &pending_host_modules {
                for m in pins {
                    println!(
                        "resolved oracle module \"{}\" ({}) for host \"{host}\" -> {}",
                        m.name, m.url, m.rev,
                    );
                }
            }
            if let Some(pending) = &pending_module_sources {
                print_resolved_module_sources(pending, "resolved");
            }
            for i in pending_flake_inputs.iter().flatten() {
                println!(
                    "resolved flake input \"{}\" ({}) -> {}",
                    i.name, i.url, i.rev,
                );
            }
            if let Some((release, rev)) = &pending_project_release {
                match release.is_empty() {
                    true => println!("cleared the project nixpkgs release"),
                    false => {
                        println!("resolved the project nixpkgs release \"{release}\" -> {rev}")
                    }
                }
            }
            // `plan.lock_next` was built from the lock already patched with `pending`/
            // `pending_modules`/`pending_host_modules`/`pending_module_sources` (see `run`'s
            // planning step above), so this carries the baselines and module pins through; the
            // augmented-set build below (ADR 0008) may additionally set an `options-hash` on
            // top of that.
            let mut lock_next = plan.lock_next.clone();
            let mut changed_hosts: BTreeSet<String> = pending.keys().cloned().collect();
            changed_hosts.extend(pending_host_modules.keys().cloned());
            // `upgrade` has no `--strict` flag: a missing nix is always best-effort here.
            if let Err(code) = build_pending_oracle_sets(
                &ctx.root,
                &mut lock_next,
                pending_modules.is_some(),
                &changed_hosts,
                false,
                true,
            ) {
                return code;
            }
            // Cache file writes (a disk side effect) happen only here, strictly after the
            // `--yes` gate above, and before the lock write, so a lock pin is never recorded
            // for a manifest that failed to land in the cache (task 5b).
            if let Some(pending) = &pending_module_sources {
                if let Err(code) = write_module_source_caches(pending) {
                    return code;
                }
            }
            write_lock(ctx, &lock_next); // bump tool/module/formatter/oracle/baselines
            Code::Clean
        }

        Cmd::Doc { .. } => unreachable!("doc is dispatched before Ctx::load"),

        Cmd::Install {
            pkg,
            host,
            yes,
            strict,
            build,
            no_abi_check,
        } if !yes && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() => {
            install_in_tui(
                ctx,
                &pkg,
                host.as_deref(),
                strict,
                build,
                no_abi_check,
                &pending,
            )
        }
        Cmd::Install {
            pkg,
            host,
            yes,
            strict,
            build,
            no_abi_check,
        } => install(
            ctx,
            &pkg,
            host.as_deref(),
            yes,
            strict,
            build,
            no_abi_check,
            &pending,
            pending_modules.as_deref(),
            &pending_host_modules,
            pending_module_sources.as_deref(),
        ),

        Cmd::Tui => unreachable!("tui is dispatched before Ctx::load"),
        Cmd::Updates => cmd_updates(ctx, cli.json),
    }
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
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("knixl: tui needs an interactive terminal");
            return Code::Usage;
        }
        return match open_tui(tui::Entry::Hub, Default::default()) {
            Ok(outcome) => finish_tui_outcome(outcome),
            Err(e) => {
                eprintln!("knixl: {e}");
                Code::Internal
            }
        };
    }
    // `doc` only reads module schemas, so it skips generation: a broken host must not stop
    // someone reading the reference they need to fix it (#108).
    if let Cmd::Doc { node } = &cli.cmd {
        return match knixl_pipeline::gather::doc_registry(&discover_root()) {
            Ok(registry) => print_doc(&registry, node),
            Err(e) => {
                eprintln!("knixl: {e}");
                gather_error_code(&e)
            }
        };
    }
    run(cli, &Ctx::load())
}

struct Ctx {
    inputs: knixl_lock::reconcile::Inputs,
    disk: knixl_lock::reconcile::DiskState,
    lock: knixl_lock::Lock,
    running: knixl_lock::reconcile::Versions,
    registry: knixl_modules::Registry,
    root: std::path::PathBuf,
    generated: std::collections::BTreeMap<std::path::PathBuf, String>,
    warnings: Vec<String>,
    flake_lock_problems: Vec<String>,
    /// Root-relative host sources whose generated config allows unfree (see `gather`).
    unfree_hosts: BTreeSet<std::path::PathBuf>,
    /// Hosts generated without option checks for want of a cached set (see `gather`).
    unvalidated: Vec<(String, String)>,
}
/// A broken `knixl.kdl`, module manifest or host file is the user's input, so it is a
/// Validation (#131); only I/O, the lock and the formatter are Internal.
fn gather_error_code(e: &knixl_pipeline::gather::GatherError) -> Code {
    use knixl_pipeline::gather::GatherError;
    use knixl_pipeline::GenerateError;
    match e {
        GatherError::Module(_)
        | GatherError::Generate(
            GenerateError::Parse(_) | GenerateError::Validation(_) | GenerateError::Lower(_),
        ) => Code::Validation,
        GatherError::Io(_) | GatherError::Lock(_) | GatherError::Generate(_) => Code::Internal,
    }
}

impl Ctx {
    fn load() -> Ctx {
        let root = discover_root();
        let tool = env!("CARGO_PKG_VERSION")
            .parse()
            .expect("tool version parses");
        let project = knixl_pipeline::gather::gather(&root, &default_formatter(), tool)
            .unwrap_or_else(|e| {
                eprintln!("knixl: {e}");
                std::process::exit(gather_error_code(&e) as i32);
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
            flake_lock_problems: project.flake_lock_problems,
            unvalidated: project.unvalidated,
            unfree_hosts: project.unfree_hosts,
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
        assert_eq!(
            choose_formatter_bin(None, |b| b == "nixfmt-rfc-style"),
            "nixfmt-rfc-style"
        );
    }

    #[test]
    fn defaults_to_nixfmt_when_none_run() {
        assert_eq!(choose_formatter_bin(None, |_| false), "nixfmt");
    }
}
