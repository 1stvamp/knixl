//! knixl CLI. Every command is a thin policy over one Plan. Plan::compute is the only
//! thing that inspects the world. Exit codes are stable so CI can branch on them.
//! SPEC-GRADE SKETCH: Ctx::load and the write/report helpers are not written.

mod tui;

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use knixl_lock::model::{HostBaseline, ModuleSourcePin, OracleModulePin};
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
    Plan {
        #[arg(long)]
        detailed_exitcode: bool,
    },
    /// CI gate: succeed only if every file is Clean. Never writes, never prompts.
    Check,
    /// Apply. Silent for Stale/Missing; refuses Drifted/skew without the matching flag.
    Generate {
        #[arg(long)]
        accept_drift: bool,
        #[arg(long)]
        prune: bool,
    },
    /// Version bump: show migration notes + diff, apply on --yes, then bump the lock.
    Upgrade {
        #[arg(long)]
        yes: bool,
    },
    /// Print the typed reference for a module node (from schema()).
    Doc { node: String },
    /// Add a package to a host: draft the KDL, verify under nix, preview, then regenerate.
    Install {
        /// The nixpkgs attribute name (e.g. ripgrep) or versioned form (e.g. ripgrep@13.0.0).
        pkg: String,
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        yes: bool,
        /// Treat a skipped nix check (nix absent) as an error.
        #[arg(long)]
        strict: bool,
        /// Also build the package derivation (proves it builds, not just resolves).
        #[arg(long)]
        build: bool,
        /// Skip the build-feasibility check that picks the pin strategy: always commit-mix.
        #[arg(long)]
        no_abi_check: bool,
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
    // #22: `install` and `upgrade` are the commands that resolve a declared-but-unresolved
    // nixpkgs baseline (the validation error below names `upgrade` as the fix for every
    // other command); this must run before the validation gate, or neither could ever reach
    // its own remedy. Resolved IN MEMORY only (a network lookup is read-only, so that part is
    // safe here) and never written: writing before the `--yes`/confirm gate let a preview
    // mutate the lock and a cancelled install leave a baseline behind with no revert (review
    // finding on #22). `pending` carries what would be written; `Cmd::Upgrade` writes it on
    // `--yes`, `install`/`commit_install` write it in the confirmed, revertable commit path.
    let pending: BTreeMap<String, HostBaseline> =
        if matches!(cli.cmd, Cmd::Upgrade { .. } | Cmd::Install { .. }) {
            match resolve_pending_baselines(ctx) {
                Ok(p) => p,
                Err(code) => return code,
            }
        } else {
            BTreeMap::new()
        };

    // #35 phase 3: the project's declared `oracle-modules` (knixl.kdl) resolve the same way,
    // alongside the baseline pre-pass above (in memory only, never written here; see
    // `resolve_pending_project_modules`).
    let pending_modules: Option<Vec<OracleModulePin>> =
        if matches!(cli.cmd, Cmd::Upgrade { .. } | Cmd::Install { .. }) {
            match resolve_pending_project_modules(ctx) {
                Ok(p) => p,
                Err(code) => return code,
            }
        } else {
            None
        };

    // ADR 0008: a host's own `oracle-modules` override resolves the same way, alongside the
    // project-wide pass above (in memory only, never written here; see
    // `resolve_pending_host_modules`). Unlike the project pass this can refuse the whole run
    // (`Err(Code::Validation)`) rather than just resolving to `None`: a host that declares an
    // override with no declared `nixpkgs release=` has nowhere in the lock to carry its pins.
    let pending_host_modules: BTreeMap<String, Vec<OracleModulePin>> =
        if matches!(cli.cmd, Cmd::Upgrade { .. } | Cmd::Install { .. }) {
            match resolve_pending_host_modules(ctx) {
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
        || pending_module_sources.is_some())
    .then(|| {
        let mut lock = ctx.lock.clone();
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
            lock.module_sources = pending.iter().map(|p| p.pin.clone()).collect();
        }
        lock
    });
    let patched_inputs =
        (!pending.is_empty()).then(|| patch_inputs_for_pending(&ctx.inputs, &pending));
    let lock_for_plan = patched_lock.as_ref().unwrap_or(&ctx.lock);
    let inputs_for_plan = patched_inputs.as_ref().unwrap_or(&ctx.inputs);

    // `build_lock_next` (knixl-lock) sources the GLOBAL `oracle` pin from `running.oracle`,
    // not from the `lock` passed to `Plan::compute` (unlike per-host `baselines`, which it
    // takes from `lock` -- see `build_lock_next`): so `pending_modules` needs to land in a
    // patched `Versions`, not the patched lock above, for `plan.lock_next` to carry it through
    // to `Cmd::Upgrade`'s/`commit_install`'s write.
    let patched_running = pending_modules
        .as_ref()
        .map(|pins| knixl_lock::reconcile::Versions {
            tool: ctx.running.tool.clone(),
            formatter: ctx.running.formatter.clone(),
            oracle: knixl_lock::model::OraclePin {
                nixpkgs_rev: ctx.running.oracle.nixpkgs_rev.clone(),
                options_hash: ctx.running.oracle.options_hash.clone(),
                modules: pins.clone(),
            },
            modules: ctx.running.modules.clone(),
        });
    let running_for_plan = patched_running.as_ref().unwrap_or(&ctx.running);

    // Plan::compute is pure; validation errors ride on the plan (verdict maps them to
    // the Validation exit code), so there is no fallible generation step here.
    let plan = Plan::compute(inputs_for_plan, &ctx.disk, lock_for_plan, running_for_plan);
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
            if detailed_exitcode {
                verdict(&plan)
            } else {
                Code::Clean
            }
        }

        Cmd::Check => {
            print_plan(&plan, cli.json);
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
            let mut worst = Code::Clean;
            for f in &plan.files {
                match &f.state {
                    FileState::Clean => {}
                    FileState::Stale { .. } | FileState::Missing { .. } => write_file(ctx, f),
                    FileState::Drifted { .. } if accept_drift => write_file(ctx, f),
                    FileState::Drifted { .. } => {
                        report_taint(f, cli.json);
                        worst = worst.max(Code::Drift);
                    }
                    FileState::Orphaned if prune => delete_file(ctx, f),
                    FileState::Orphaned => {
                        note_orphan(f, cli.json);
                        worst = worst.max(Code::RegenPending);
                    }
                }
            }
            // Commit the lock ONLY on a clean apply, so it never lies about disk.
            if worst == Code::Clean {
                write_lock(ctx, &plan.lock_next);
            }
            worst
        }

        Cmd::Upgrade { yes } => {
            // A pending baseline/module resolution is work to do even when every file is
            // Clean and nothing needs ack: skip the "up to date" short-circuit so it gets
            // previewed (and, on --yes, written) below instead of silently vanishing.
            if !plan.requires_ack()
                && !plan.any(FileState::is_dirty)
                && pending.is_empty()
                && pending_modules.is_none()
                && pending_host_modules.is_empty()
                && pending_module_sources.is_none()
            {
                println!("already up to date");
                return Code::Clean;
            }
            print_migration_notes(&plan, &ctx.registry); // per (module, version delta)
            print_plan(&plan, cli.json);
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
                for p in pending_module_sources.iter().flatten() {
                    if p.fetched_text.is_some() {
                        println!(
                            "would resolve module source \"{}\" ({}) -> {}",
                            p.pin.name, p.pin.url, p.pin.rev,
                        );
                    }
                }
                eprintln!("re-run with --yes to apply");
                return Code::NeedsAck;
            }
            for f in &plan.files {
                if !matches!(f.state, FileState::Clean) {
                    write_file(ctx, f);
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
            for p in pending_module_sources.iter().flatten() {
                if p.fetched_text.is_some() {
                    println!(
                        "resolved module source \"{}\" ({}) -> {}",
                        p.pin.name, p.pin.url, p.pin.rev,
                    );
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

        Cmd::Doc { node } => {
            print_doc(ctx, &node, cli.json);
            Code::Clean
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
    }
}

// One argument per `Cmd::Install` field, plus `ctx` and the pending-baseline map/pending-module
// list threaded from `run`'s pre-pass: splitting these into a struct would obscure more than
// it saves here.
#[allow(clippy::too_many_arguments)]
fn install(
    ctx: &Ctx,
    pkg: &str,
    host: Option<&str>,
    yes: bool,
    strict: bool,
    build: bool,
    no_abi_check: bool,
    pending: &BTreeMap<String, HostBaseline>,
    pending_modules: Option<&[OracleModulePin]>,
    pending_host_modules: &BTreeMap<String, Vec<OracleModulePin>>,
    pending_module_sources: Option<&[PendingModuleSource]>,
) -> Code {
    use knixl_nix::pin::{PinError, PinResolver};
    use knixl_pipeline::install::{list_hosts, select_host};

    let (name, version) = match pkg.split_once('@') {
        Some((n, v)) => (n, Some(v)),
        None => (pkg, None),
    };

    let hosts = match list_hosts(&ctx.root) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("knixl: {e}");
            return Code::Internal;
        }
    };
    let initial = match select_host(&hosts, host) {
        Ok(t) => t.clone(),
        Err(e) => {
            eprintln!("knixl: {e}");
            return Code::Usage;
        }
    };

    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive && !yes {
        let entry = tui::Entry::Install {
            pkg: name.to_string(),
            strict,
            host: Some(initial.name.clone()),
            version: version.map(str::to_string),
            no_abi_check,
        };
        let build_fn = build.then(|| make_build(ctx.root.clone()));
        let pin_fn = version.is_some().then(make_pin);
        // #28: decide the pin strategy inside the Install screen's Apply-gated verify
        // sequence, rather than build-testing a second time at commit (`commit_tui_install`'s
        // old `choose_strategy` call). Closes over the data needed to compute a baseline for
        // WHICHEVER host is selected at call time (the host list, the lock's baselines, the
        // global oracle rev, and any pending per-host resolutions), rather than a single
        // baseline fixed to `initial` (#28 review fix: a host switch inside the TUI used to
        // leave the strategy decided against the host that was selected when the TUI opened).
        let strategy_fn = version.is_some().then(|| {
            make_strategy(
                hosts.clone(),
                ctx.lock.baselines.clone(),
                ctx.lock.oracle.nixpkgs_rev.clone(),
                pending.clone(),
                no_abi_check,
            )
        });
        return match open_tui(entry, build_fn, pin_fn, strategy_fn) {
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
                // The TUI may switch the target host from `initial`: look the pending
                // resolution up for whichever host was actually chosen.
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
        };
    }

    // The pending baseline resolution for the target host, if `run`'s pre-pass resolved one
    // (issue #22 review fix): written alongside the pin by `commit_install`, in the same
    // committed/revertable step, rather than up front by a pre-pass.
    let baseline_pending = pending.get(&initial.name).cloned();
    // As above, for the target host's own `oracle-modules` override (ADR 0008), if it has one.
    let host_modules_pending = pending_host_modules.get(&initial.name).cloned();

    // A version cannot be pinned without a resolved commit and a decided strategy: refuse
    // before anything else. A pin already on record for this exact (host, package, version)
    // is reused verbatim (its rev and strategy), skipping both resolution and selection: an
    // idempotent repeat install touches neither the network nor nix.
    let mut build_tested = false;
    let version_pin = match version {
        Some(v) => {
            let cached = ctx
                .lock
                .pins
                .get(&initial.name)
                .and_then(|pins| pins.iter().find(|p| p.package == name && p.version == v));
            let (resolved, strategy, reason) = if let Some(p) = cached {
                (
                    knixl_nix::pin::Resolved {
                        nixpkgs_rev: p.nixpkgs_rev.clone(),
                    },
                    p.strategy,
                    "cached".to_string(),
                )
            } else {
                let resolved = match PinResolver::resolve().lookup(name, v) {
                    Ok(r) => r,
                    Err(e @ (PinError::NotFound(_) | PinError::Failed(_))) => {
                        eprintln!("knixl: {e}");
                        return Code::Validation;
                    }
                    Err(e @ PinError::Unavailable(_)) => {
                        eprintln!("knixl: cannot resolve {name}@{v}: {e}");
                        return Code::Validation;
                    }
                };
                let baseline_rev = effective_baseline_rev(
                    &ctx.lock.oracle.nixpkgs_rev,
                    &ctx.lock.baselines,
                    &initial.path,
                    &initial.name,
                    baseline_pending.as_ref(),
                );
                match choose_strategy(name, &resolved.nixpkgs_rev, &baseline_rev, no_abi_check) {
                    Ok((strategy, reason, tested)) => {
                        build_tested = tested;
                        (resolved, strategy, reason)
                    }
                    Err((commit_mix, over)) => {
                        eprintln!(
                            "knixl: cannot pin {name}@{v}: commit-mix build failed: {commit_mix}"
                        );
                        eprintln!("knixl: cannot pin {name}@{v}: override build failed: {over}");
                        return Code::Validation;
                    }
                }
            };
            Some((v, resolved, strategy, reason))
        }
        None => None,
    };

    // Non-interactive / --yes: the plain path. Hard-check the package, then build it if
    // requested, then confirm unless --yes.
    match resolve_package(ctx, name) {
        tui::Resolve::No => {
            eprintln!("knixl: no nixpkgs package named `{name}`");
            return Code::Validation;
        }
        tui::Resolve::Skipped if strict => {
            eprintln!("knixl: --strict: nix unavailable, cannot verify `{name}`");
            return Code::Validation;
        }
        tui::Resolve::Skipped => {
            eprintln!("warning: nix unavailable, skipping package check for `{name}`");
        }
        tui::Resolve::Yes => {}
    }
    // A freshly build-tested version pin already proves `name` builds at the pinned rev:
    // reuse that instead of a second, separate build of the ambient package.
    if build && !build_tested {
        use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
        let rev = &ctx.lock.oracle.nixpkgs_rev;
        let src = if rev.is_empty() {
            Nixpkgs::Ambient
        } else {
            Nixpkgs::PinnedRev(rev.clone())
        };
        match NixEval::resolve().builds(&src, name) {
            Ok(()) => {}
            Err(NixError::Unavailable(_)) if strict => {
                eprintln!("knixl: --strict: nix unavailable, cannot build `{name}`");
                return Code::Validation;
            }
            Err(NixError::Unavailable(_)) => {
                eprintln!("warning: nix unavailable, skipping build of `{name}`");
            }
            Err(NixError::Failed(m)) => {
                eprintln!("knixl: `{name}` failed to build: {m}");
                return Code::Validation;
            }
        }
    }
    if !yes && !confirm(&format!("install {pkg} on {}?", initial.name)) {
        println!("cancelled");
        return Code::Clean;
    }
    let pin_args = version_pin
        .as_ref()
        .map(|(v, resolved, strategy, _)| (*v, resolved, *strategy));
    let code = commit_install(
        &initial,
        name,
        pin_args,
        baseline_pending.as_ref(),
        pending_modules,
        host_modules_pending.as_deref(),
        pending_module_sources,
        strict,
    );
    if code == Code::Clean {
        if let Some(b) = &baseline_pending {
            println!(
                "resolved nixpkgs release \"{}\" for host \"{}\" -> {}",
                b.release, initial.name, b.nixpkgs_rev,
            );
        }
        for m in pending_modules.into_iter().flatten() {
            println!(
                "resolved oracle module \"{}\" ({}) -> {}",
                m.name, m.url, m.rev
            );
        }
        for m in host_modules_pending.iter().flatten() {
            println!(
                "resolved oracle module \"{}\" ({}) for host \"{}\" -> {}",
                m.name, m.url, initial.name, m.rev
            );
        }
        for p in pending_module_sources.into_iter().flatten() {
            if p.fetched_text.is_some() {
                println!(
                    "resolved module source \"{}\" ({}) -> {}",
                    p.pin.name, p.pin.url, p.pin.rev
                );
            }
        }
        if let Some((v, _, strategy, reason)) = &version_pin {
            println!(
                "pinned {name} {v} via {} ({reason})",
                strategy_label(*strategy)
            );
        }
    }
    code
}

// `pending_modules` is `None` for the interactive/TUI install paths (#35 phase 3 only wires
// the plain, non-interactive `install`/`upgrade` paths; the TUI's own commit path does not
// resolve or write project oracle-module pins yet -- see `commit_tui_install`'s call site).
// `host_modules_pending` (ADR 0008, the host-override counterpart) and `pending_module_sources`
// (task 5b) are `None` there too, for the same reason.
#[allow(clippy::too_many_arguments)]
fn commit_install(
    chosen: &knixl_pipeline::install::HostInfo,
    pkg: &str,
    version_pin: Option<(
        &str,
        &knixl_nix::pin::Resolved,
        knixl_lock::model::PinStrategy,
    )>,
    baseline_pending: Option<&HostBaseline>,
    pending_modules: Option<&[OracleModulePin]>,
    host_modules_pending: Option<&[OracleModulePin]>,
    pending_module_sources: Option<&[PendingModuleSource]>,
    strict: bool,
) -> Code {
    use knixl_pipeline::install::add_package;

    let original = match std::fs::read_to_string(&chosen.path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("knixl: {}: {e}", chosen.path.display());
            return Code::Internal;
        }
    };
    let draft = match add_package(&original, pkg, version_pin.map(|(v, _, _)| v)) {
        Ok(Some(d)) => d,
        Ok(None) => {
            println!("{pkg} is already installed on {}", chosen.name);
            return Code::Clean;
        }
        Err(e) => {
            eprintln!("knixl: cannot edit {}: {e}", chosen.path.display());
            return Code::Internal;
        }
    };

    // `oracle.modules` is a single project-wide list (unlike the pin/baseline, which are
    // fresh per-host inserts): capture what it held before this pending resolution, so a
    // revert below can restore it exactly rather than blank it out.
    let prior_modules = pending_modules.map(|_| Ctx::load().lock.oracle.modules.clone());
    // As above, for the target host's own baseline `.modules` (ADR 0008 host override): a
    // per-host list, but still something to restore rather than blank on revert, since a host
    // may already have had a different override in place before this pending resolution.
    let prior_host_modules = host_modules_pending.map(|_| {
        Ctx::load()
            .lock
            .baselines
            .get(&chosen.name)
            .map(|b| b.modules.clone())
            .unwrap_or_default()
    });
    // As above, for the project-wide `module_sources` list (task 5b): captured before the
    // write below, so a revert restores exactly what was pinned before, rather than blanking
    // sources this install did not touch.
    let prior_module_sources =
        pending_module_sources.map(|_| Ctx::load().lock.module_sources.clone());

    // The module-source cache file write (task 5b) is a disk side effect with nothing yet on
    // record to undo, so it runs first and, on failure, simply refuses before writing anything
    // else: a lock pin must never be recorded for a manifest that failed to land in the cache.
    if let Some(pending) = pending_module_sources {
        if let Err(code) = write_module_source_caches(pending) {
            return code;
        }
    }

    // Record the pin, any pending baseline resolution, and any pending oracle-module
    // resolution before the versioned KDL hits disk (issue #22 review fix: a baseline
    // resolved for this install used to be written by a pre-pass before confirmation; it is
    // now written here, in the same committed/revertable step as the pin): `generate` treats
    // a `package` node with a `version` prop and no matching lock pin as an error, so writing
    // the KDL first would (briefly, but for real, since the next line's gather sees it) make
    // the project fail to generate even on the success path.
    if let Some((v, resolved, strategy)) = version_pin {
        write_pin(&chosen.name, pkg, v, resolved, strategy);
    }
    if let Some(b) = baseline_pending {
        write_baseline(&chosen.name, &b.release, &b.nixpkgs_rev, &b.options_hash);
    }
    if let Some(pins) = pending_modules {
        write_oracle_modules(pins);
    }
    // After `write_baseline` above, so a host whose release resolved in this same run already
    // has a baseline entry for this to set `.modules` on.
    if let Some(pins) = host_modules_pending {
        write_host_oracle_modules(&chosen.name, pins);
    }
    if let Some(pending) = pending_module_sources {
        let pins: Vec<ModuleSourcePin> = pending.iter().map(|p| p.pin.clone()).collect();
        write_module_sources(&pins);
    }

    if let Err(e) = std::fs::write(&chosen.path, &draft) {
        eprintln!("knixl: {}: {e}", chosen.path.display());
        // The pin/baseline/modules above may already be on disk even though the KDL write
        // failed: undo them so a failed install never leaves any of them dangling.
        if version_pin.is_some() {
            remove_pin(&chosen.name, pkg);
        }
        if baseline_pending.is_some() {
            remove_baseline(&chosen.name);
        }
        if let Some(prior) = &prior_modules {
            restore_oracle_modules(prior);
        }
        if let Some(prior) = &prior_host_modules {
            restore_host_oracle_modules(&chosen.name, prior);
        }
        if let Some(prior) = &prior_module_sources {
            restore_module_sources(prior);
        }
        return Code::Internal;
    }
    // Undo the pin, baseline, and modules along with the KDL: all three may have been
    // written above, before the KDL hit disk, so an abort past this point must not leave any
    // of them dangling in the lock.
    let revert = || {
        let _ = std::fs::write(&chosen.path, &original);
        if version_pin.is_some() {
            remove_pin(&chosen.name, pkg);
        }
        if baseline_pending.is_some() {
            remove_baseline(&chosen.name);
        }
        if let Some(prior) = &prior_modules {
            restore_oracle_modules(prior);
        }
        if let Some(prior) = &prior_host_modules {
            restore_host_oracle_modules(&chosen.name, prior);
        }
        if let Some(prior) = &prior_module_sources {
            restore_module_sources(prior);
        }
    };

    let drafted = Ctx::load();
    let plan = Plan::compute(
        &drafted.inputs,
        &drafted.disk,
        &drafted.lock,
        &drafted.running,
    );
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
            FileState::Drifted { .. } => {
                report_taint(f, false);
                worst = worst.max(Code::Drift);
            }
            FileState::Clean | FileState::Orphaned => {}
        }
    }
    if worst == Code::Clean {
        // ADR 0008: build + cache the augmented option set for whatever effective sets this
        // install just changed, folding each built content's hash into a local copy of
        // `plan.lock_next` before it is written; a missing nix is best-effort unless `strict`.
        let mut lock_next = plan.lock_next.clone();
        let mut changed_hosts = BTreeSet::new();
        if baseline_pending.is_some() || host_modules_pending.is_some() {
            changed_hosts.insert(chosen.name.clone());
        }
        if let Err(code) = build_pending_oracle_sets(
            &drafted.root,
            &mut lock_next,
            pending_modules.is_some(),
            &changed_hosts,
            strict,
        ) {
            revert();
            return code;
        }
        write_lock(&drafted, &lock_next);
        println!("installed {pkg} on {}", chosen.name);
    } else {
        revert();
    }
    worst
}

/// Insert or replace the resolved pin for `package` under `host` in the lock, then write it
/// back to disk. Dedups by package name, so re-pinning an already-pinned package replaces the
/// old entry rather than accumulating one per version. Loads the lock through `Ctx::load` so a
/// fresh project with no lock yet gets the same seeded default `gather` uses elsewhere.
fn write_pin(
    host: &str,
    package: &str,
    version: &str,
    resolved: &knixl_nix::pin::Resolved,
    strategy: knixl_lock::model::PinStrategy,
) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    let pins = lock.pins.entry(host.to_string()).or_default();
    pins.retain(|p| p.package != package);
    pins.push(knixl_lock::model::Pin {
        package: package.to_string(),
        version: version.to_string(),
        nixpkgs_rev: resolved.nixpkgs_rev.clone(),
        strategy,
    });
    pins.sort_by(|a, b| a.package.cmp(&b.package));
    write_lock(&ctx, &lock);
}

/// Undo `write_pin`: drop the pin for `package` under `host`, then write the lock back.
/// Mirrors `write_pin`. Called on `commit_install`'s revert path (a validation, parse, or
/// drift failure after the pin was already written), so an aborted install never leaves a
/// dangling pin behind. A no-op when no pin exists for that package on that host.
fn remove_pin(host: &str, package: &str) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    let pins = lock.pins.entry(host.to_string()).or_default();
    pins.retain(|p| p.package != package);
    write_lock(&ctx, &lock);
}

/// Insert or replace the resolved baseline for `host` in the lock, then write it back to
/// disk. Mirrors `write_pin`.
fn write_baseline(host: &str, release: &str, rev: &str, options_hash: &str) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.baselines.insert(
        host.to_string(),
        HostBaseline {
            release: release.to_string(),
            nixpkgs_rev: rev.to_string(),
            options_hash: options_hash.to_string(),
            modules: Vec::new(),
        },
    );
    write_lock(&ctx, &lock);
}

/// Undo `write_baseline`: drop the baseline for `host`, then write the lock back. Mirrors
/// `remove_pin`. Called on `commit_install`'s revert path (a validation, parse, or drift
/// failure after the baseline was already written, or a plain disk-write failure), so a
/// cancelled/failed install never leaves a freshly resolved baseline dangling. A no-op when
/// no baseline exists for that host.
fn remove_baseline(host: &str) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.baselines.remove(host);
    write_lock(&ctx, &lock);
}

/// Replace the lock's project-wide `oracle.modules` pins with `pins`, then write it back to
/// disk. Unlike `write_pin`/`write_baseline` (keyed per host) this is a single, project-wide
/// list resolved from `knixl.kdl`'s `oracle-modules` block.
fn write_oracle_modules(pins: &[OracleModulePin]) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.oracle.modules = pins.to_vec();
    write_lock(&ctx, &lock);
}

/// Undo `write_oracle_modules`: restore the lock's `oracle.modules` to `prior` (captured by
/// `commit_install` before it wrote the pending resolution), then write the lock back. Called
/// on `commit_install`'s revert path, so a cancelled/failed install never leaves a module-pin
/// change dangling. Unlike `remove_baseline` (a plain removal: a resolved baseline for a host
/// is always a fresh insert), this restores the PRIOR value rather than clearing it, since
/// `oracle.modules` may already have held a different (still-valid) set before this pending
/// resolution replaced it.
fn restore_oracle_modules(prior: &[OracleModulePin]) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.oracle.modules = prior.to_vec();
    write_lock(&ctx, &lock);
}

/// Set `host`'s baseline `.modules` to `pins`, leaving the rest of its baseline unchanged,
/// then write the lock back. Mirrors `write_oracle_modules` but per host (ADR 0008 host
/// override). A no-op if `host` has no baseline yet: the caller
/// (`resolve_pending_host_modules`) already refuses a host that declares an `oracle-modules`
/// override with no declared `nixpkgs release=` before this could ever be reached, and
/// `commit_install` always calls `write_baseline` first when a release also resolved in this
/// same run, so the baseline this sets `.modules` on already exists by the time it runs.
fn write_host_oracle_modules(host: &str, pins: &[OracleModulePin]) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    if let Some(b) = lock.baselines.get_mut(host) {
        b.modules = pins.to_vec();
    }
    write_lock(&ctx, &lock);
}

/// Undo `write_host_oracle_modules`: restore `host`'s baseline `.modules` to `prior` (captured
/// by `commit_install` before it wrote the pending resolution), then write the lock back.
/// Mirrors `restore_oracle_modules`, but per host.
fn restore_host_oracle_modules(host: &str, prior: &[OracleModulePin]) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    if let Some(b) = lock.baselines.get_mut(host) {
        b.modules = prior.to_vec();
    }
    write_lock(&ctx, &lock);
}

/// Replace the lock's `module_sources` pins with `pins`, then write it back to disk (task 5b).
/// Project-wide, like `write_oracle_modules`, not per host: a declared `modules {}` source is
/// not tied to any one host.
fn write_module_sources(pins: &[ModuleSourcePin]) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.module_sources = pins.to_vec();
    write_lock(&ctx, &lock);
}

/// Undo `write_module_sources`: restore `module_sources` to `prior` (captured by
/// `commit_install` before it wrote the pending resolution), then write the lock back. Mirrors
/// `restore_oracle_modules`. Deliberately does not touch any cache file `write_module_source_
/// caches` may already have written for a freshly resolved source: the cache is
/// content-addressed (keyed on `(url, rev, path)`), so a stray cache entry left behind by an
/// aborted install is harmless, exactly as `build_and_cache_options`'s options.json cache is
/// never deleted on any of the other revert paths here.
fn restore_module_sources(prior: &[ModuleSourcePin]) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.module_sources = prior.to_vec();
    write_lock(&ctx, &lock);
}

/// The `nixpkgs release=".."` a host's own KDL declares, if any (issue #22). Scanned
/// straight from the file rather than the gathered project, since a target host is often
/// resolved before (or independently of) a full `gather`.
fn declared_release(host_path: &std::path::Path) -> Option<String> {
    let src = std::fs::read_to_string(host_path).ok()?;
    let doc = knixl_kdl::parse(&src).ok()?;
    let node = doc.nodes().iter().find(|n| n.name().value() == "host")?;
    knixl_kdl::child_prop_str(node, "nixpkgs", "release")
}

/// The blake3 hash of the options.json cached for `rev`, or an empty string when nothing is
/// cached (best-effort, same convention as an unresolved oracle rev).
fn options_hash_for_rev(rev: &str) -> String {
    knixl_oracle::cache_path(rev)
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::read(&p).ok())
        .map(|bytes| knixl_nix::hash(&bytes))
        .unwrap_or_default()
}

/// Resolve `host`'s declared `nixpkgs release` baseline IN MEMORY ONLY: no write (issue #22
/// review fix; the pre-pass used to write straight to the lock here, before the validation
/// gate and before `--yes`/confirmation). `Ok(None)` when the host declares no release, or
/// its release already matches the lock's recorded baseline (the idempotent skip); otherwise
/// `Ok(Some(baseline))` with the freshly resolved value, for the caller to plan around and
/// decide whether to keep. `Err(Code::Validation)` on a resolver failure; the message is
/// already printed, so the caller only needs to return the code.
fn resolve_pending_baseline(
    ctx: &Ctx,
    host: &str,
    host_path: &std::path::Path,
) -> Result<Option<HostBaseline>, Code> {
    let Some(release) = declared_release(host_path) else {
        return Ok(None);
    };
    if let Some(b) = ctx.lock.baselines.get(host) {
        if b.release == release {
            return Ok(None);
        }
    }
    let rev = knixl_nix::baseline::BaselineResolver::resolve()
        .lookup(&release)
        .map_err(|e| {
            eprintln!(
                "knixl: cannot resolve nixpkgs release \"{release}\" for host \"{host}\": {e}"
            );
            Code::Validation
        })?;
    let options_hash = options_hash_for_rev(&rev);
    Ok(Some(HostBaseline {
        release,
        nixpkgs_rev: rev,
        options_hash,
        modules: Vec::new(),
    }))
}

/// For every host under `ctx.root` whose KDL declares a `nixpkgs release=".."` not already
/// resolved to that exact release in the lock, resolve it via `resolve_pending_baseline`
/// (in memory only). Called by `run`'s `Cmd::Upgrade`/`Cmd::Install` pre-pass, before the
/// shared validation gate: the "not resolved" validation error names `upgrade` as the fix,
/// and `install` needs a resolved baseline for `choose_strategy`, so neither could reach its
/// own remedy if the gate refused first. Returns the pending resolutions keyed by host name
/// (empty if every declared release already matched), or `Err(Code::Validation)` on the first
/// resolver failure (message already printed). Nothing here is written; see `Cmd::Upgrade`
/// and `commit_install` for where a caller commits (or discards) what this returns.
fn resolve_pending_baselines(ctx: &Ctx) -> Result<BTreeMap<String, HostBaseline>, Code> {
    use knixl_pipeline::install::list_hosts;
    let hosts = list_hosts(&ctx.root).unwrap_or_default();
    let mut pending = BTreeMap::new();
    for h in &hosts {
        if let Some(b) = resolve_pending_baseline(ctx, &h.name, &h.path)? {
            pending.insert(h.name.clone(), b);
        }
    }
    Ok(pending)
}

/// Resolve one declared `module "<name>" flake="<ref>" [attr="<attr>"]` (project `knixl.kdl`
/// or a host's own `oracle-modules` override) to a lock pin (#35 phase 3). `Err(Code::Validation)`
/// on a resolver failure (including an unsupported/empty flake ref -- `ModuleResolver::lookup`
/// refuses that before any resolver runs, so this never pins an empty rev); the message is
/// already printed, so the caller only needs to return the code.
fn resolve_oracle_module(
    m: &knixl_pipeline::project::OracleModule,
) -> Result<OracleModulePin, Code> {
    knixl_nix::module::ModuleResolver::resolve()
        .lookup(&m.flake)
        .map(|r| OracleModulePin {
            name: m.name.clone(),
            url: r.url,
            rev: r.rev,
            attr: m.attr.clone(),
        })
        .map_err(|e| {
            eprintln!(
                "knixl: cannot resolve oracle module \"{}\" ({}): {e}",
                m.name, m.flake
            );
            Code::Validation
        })
}

/// Resolve a whole declared module set (project default, or a host's override), in source
/// order. `Err` short-circuits on the first failing module (message already printed by
/// `resolve_oracle_module`).
fn resolve_oracle_modules(
    modules: &[knixl_pipeline::project::OracleModule],
) -> Result<Vec<OracleModulePin>, Code> {
    modules.iter().map(resolve_oracle_module).collect()
}

/// Whether `existing` (the lock's currently recorded pins) already matches `declared` (a
/// `knixl.kdl`/host `oracle-modules` block) by identity -- name, flake-derived url, and attr,
/// in the same order -- ignoring the pinned `rev`. Mirrors `resolve_pending_baseline`'s
/// release-string idempotency check: a declared module set that has not itself changed should
/// not be re-resolved (and re-hit the network) on every `install`/`upgrade`, even though the
/// upstream commit it is pinned to may have moved on since.
fn oracle_modules_up_to_date(
    existing: &[OracleModulePin],
    declared: &[knixl_pipeline::project::OracleModule],
) -> bool {
    existing.len() == declared.len()
        && existing.iter().zip(declared.iter()).all(|(pin, m)| {
            pin.name == m.name
                && pin.attr == m.attr
                && knixl_nix::module::url_from_flake_ref(&m.flake).as_deref()
                    == Some(pin.url.as_str())
        })
}

/// Resolve the project's default `oracle-modules` set (`knixl.kdl`) IN MEMORY ONLY: no write
/// (mirrors `resolve_pending_baseline`; #35 phase 3). `Ok(None)` when the project declares no
/// `oracle-modules` block AND the lock already has none pinned, or its declared set already
/// matches what is pinned (the idempotent skip, see `oracle_modules_up_to_date`); otherwise
/// `Ok(Some(pins))` for the caller to plan around and decide whether to keep. A project that
/// removes its `oracle-modules` block entirely resolves to `Ok(Some(Vec::new()))` rather than
/// `Ok(None)` when the lock still holds pins from a prior declaration: an empty declared set is
/// a real change (removal), and short-circuiting it to "nothing pending" would leave the stale
/// pins in the lock forever, un-GC'd (review finding on #35 phase 3). A malformed/unreadable
/// `knixl.kdl` is treated the same as an absent one (best-effort, matching `declared_release`'s
/// existing tolerance for a host's own KDL) rather than surfaced as an error here: the
/// project-config parser itself is not yet wired into `gather`'s validation (that is phase 5 of
/// #35), so there is no other path today that would report a genuinely malformed `knixl.kdl` to
/// the user.
fn resolve_pending_project_modules(ctx: &Ctx) -> Result<Option<Vec<OracleModulePin>>, Code> {
    let project = knixl_pipeline::project::parse_project(&ctx.root).unwrap_or_default();
    if project.oracle_modules.is_empty() {
        return Ok((!ctx.lock.oracle.modules.is_empty()).then(Vec::new));
    }
    if oracle_modules_up_to_date(&ctx.lock.oracle.modules, &project.oracle_modules) {
        return Ok(None);
    }
    Ok(Some(resolve_oracle_modules(&project.oracle_modules)?))
}

/// Resolve every host's own declared `oracle-modules` override IN MEMORY ONLY (mirrors
/// `resolve_pending_project_modules`, but per host and keyed on the host's own `nixpkgs
/// release=` declaration rather than the project file). ADR 0008's fixed decision: a host may
/// declare its own `oracle-modules` block only alongside a declared `nixpkgs release=` (that
/// baseline is the only place in the lock able to carry its pins). `Err(Code::Validation)`
/// (message already printed) the moment a host declares one with no declared release: this
/// refuses the whole run rather than silently ignoring the host's override, so `install`/
/// `upgrade` never resolve or write a pin they have nowhere to record. A host with no
/// `oracle-modules` block at all resolves to a present, empty entry when the lock still holds
/// a stale override for it (GC on removal, mirroring `resolve_pending_project_modules`'s
/// finding-1 fix), else is simply absent from the returned map.
fn resolve_pending_host_modules(ctx: &Ctx) -> Result<BTreeMap<String, Vec<OracleModulePin>>, Code> {
    use knixl_pipeline::install::list_hosts;
    let hosts = list_hosts(&ctx.root).unwrap_or_default();
    let mut pending = BTreeMap::new();
    for h in &hosts {
        let Ok(src) = std::fs::read_to_string(&h.path) else {
            continue;
        };
        let existing = ctx
            .lock
            .baselines
            .get(&h.name)
            .map(|b| b.modules.clone())
            .unwrap_or_default();
        match knixl_pipeline::project::parse_host_oracle_modules(&src) {
            None => {
                if !existing.is_empty() {
                    pending.insert(h.name.clone(), Vec::new());
                }
            }
            Some(declared) => {
                if declared_release(&h.path).is_none() {
                    eprintln!(
                        "knixl: host \"{}\": oracle-modules requires a declared nixpkgs release",
                        h.name
                    );
                    return Err(Code::Validation);
                }
                if oracle_modules_up_to_date(&existing, &declared) {
                    continue;
                }
                pending.insert(h.name.clone(), resolve_oracle_modules(&declared)?);
            }
        }
    }
    Ok(pending)
}

/// One resolved fetched-module-source pin from `run`'s pre-pass (task 5b), plus (only when
/// this run actually resolved and fetched it) the manifest text still waiting to be cached.
/// `fetched_text: None` means the existing lock pin was verified up to date and is simply
/// carried forward unchanged, needing no cache write; `Some(text)` means the pin is new or
/// changed this run and `text` must still be written to `module_cache_path(pin.url, pin.rev,
/// pin.path)` after the `--yes`/confirm gate (see `write_module_source_caches`).
#[derive(Clone)]
struct PendingModuleSource {
    pin: ModuleSourcePin,
    fetched_text: Option<String>,
}

/// Whether `pin` (the lock's currently recorded pin for a declared module source) is still
/// valid against `source` (that source's current `knixl.kdl` declaration): its `path` has not
/// moved AND its cache entry still hashes to `pin.hash`. Deliberately NOT a copy of
/// `oracle_modules_up_to_date`'s name+url-only identity check: `module_cache_path` keys the
/// cache location on `path` too, so a path edit that this check ignored would leave `gather`
/// looking in the wrong place forever, and `install`/`upgrade` re-running the naive check would
/// never notice or fix it (see task 5b brief and the task 5 report's "Half 2" analysis).
fn module_source_up_to_date(
    pin: &ModuleSourcePin,
    source: &knixl_pipeline::project::ModuleSource,
) -> bool {
    if pin.path != source.path {
        return false;
    }
    let Some(cache_path) =
        knixl_nix::module_fetch::module_cache_path(&pin.url, &pin.rev, &pin.path)
    else {
        return false;
    };
    let Ok(text) = std::fs::read_to_string(&cache_path) else {
        return false;
    };
    knixl_nix::module_fetch::hash_module(&text) == pin.hash
}

/// Resolve one declared module source that is not already up to date: look up its flake ref,
/// then fetch its manifest at the resolved rev. `Err(Code::Validation)` on a resolver or fetch
/// failure (message already printed, naming the source), never a silent skip.
fn resolve_module_source(
    source: &knixl_pipeline::project::ModuleSource,
) -> Result<PendingModuleSource, Code> {
    let resolved = knixl_nix::module::ModuleResolver::resolve()
        .lookup(&source.flake)
        .map_err(|e| {
            eprintln!(
                "knixl: cannot resolve module source \"{}\" ({}): {e}",
                source.name, source.flake
            );
            Code::Validation
        })?;
    let text = knixl_nix::module_fetch::fetch_module(&resolved.url, &resolved.rev, &source.path)
        .map_err(|e| {
            eprintln!(
                "knixl: cannot fetch module source \"{}\" ({}@{}): {e}",
                source.name, resolved.url, resolved.rev
            );
            Code::Validation
        })?;
    let pin = ModuleSourcePin {
        name: source.name.clone(),
        url: resolved.url,
        rev: resolved.rev,
        path: source.path.clone(),
        hash: knixl_nix::module_fetch::hash_module(&text),
    };
    Ok(PendingModuleSource {
        pin,
        fetched_text: Some(text),
    })
}

/// Resolve every declared `modules {}` source (`knixl.kdl`) IN MEMORY ONLY (task 5b; mirrors
/// `resolve_pending_project_modules`/`resolve_pending_baseline`): a source whose lock pin is
/// still up to date (`module_source_up_to_date`) is carried forward unchanged with no network
/// call at all; anything else is freshly resolved and fetched via `resolve_module_source`, read
/// but not yet written. `Ok(None)` when the resulting full set is identical to what the lock
/// already holds (nothing to plan or write, so `Cmd::Upgrade`'s "already up to date" short
/// circuit still fires); `Ok(Some(pins))` otherwise, always the FULL declared set (up-to-date
/// entries included), since `module_sources` is replaced wholesale on write, mirroring
/// `resolve_pending_project_modules`'s all-or-nothing GC-on-removal note: a project that drops
/// its `modules {}` block entirely resolves to `Ok(Some(Vec::new()))` when the lock still holds
/// stale pins, not `Ok(None)`, so those pins actually get GC'd rather than lingering forever.
fn resolve_pending_module_sources(ctx: &Ctx) -> Result<Option<Vec<PendingModuleSource>>, Code> {
    let project = knixl_pipeline::project::parse_project(&ctx.root).unwrap_or_default();
    let mut next = Vec::new();
    for source in &project.module_sources {
        let existing = ctx
            .lock
            .module_sources
            .iter()
            .find(|p| p.name == source.name);
        if let Some(pin) = existing {
            if module_source_up_to_date(pin, source) {
                next.push(PendingModuleSource {
                    pin: pin.clone(),
                    fetched_text: None,
                });
                continue;
            }
        }
        next.push(resolve_module_source(source)?);
    }

    let mut next_sorted: Vec<&ModuleSourcePin> = next.iter().map(|p| &p.pin).collect();
    next_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut existing_sorted: Vec<&ModuleSourcePin> = ctx.lock.module_sources.iter().collect();
    existing_sorted.sort_by(|a, b| a.name.cmp(&b.name));
    if next_sorted == existing_sorted {
        return Ok(None);
    }
    Ok(Some(next))
}

/// Write every freshly resolved module source's manifest text to its cache location (task 5b),
/// creating the cache directory if needed. Skips any entry with `fetched_text: None` (already
/// verified in place, nothing to write). A write failure is a hard `Code::Internal`: a lock pin
/// must never be recorded for a manifest that failed to land in the cache. Called only after
/// the `--yes`/confirm gate (see `Cmd::Upgrade` and `commit_install`), never from the shared
/// pending pre-pass.
fn write_module_source_caches(pending: &[PendingModuleSource]) -> Result<(), Code> {
    for p in pending {
        let Some(text) = &p.fetched_text else {
            continue;
        };
        let Some(cache_path) =
            knixl_nix::module_fetch::module_cache_path(&p.pin.url, &p.pin.rev, &p.pin.path)
        else {
            eprintln!(
                "knixl: module source \"{}\": cannot determine a cache location (no XDG_CACHE_HOME or HOME)",
                p.pin.name
            );
            return Err(Code::Internal);
        };
        if let Some(parent) = cache_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("knixl: {}: {e}", parent.display());
                return Err(Code::Internal);
            }
        }
        if let Err(e) = std::fs::write(&cache_path, text) {
            eprintln!("knixl: {}: {e}", cache_path.display());
            return Err(Code::Internal);
        }
    }
    Ok(())
}

/// Build the augmented `options.json` for one effective set (a nixpkgs rev plus its module
/// pins) via `nixosOptionsDoc`, cache it at `cache_path_for`, and return the built content's
/// blake3 hash to record as an `options-hash` (ADR 0008). `Ok(None)`: nothing to build (`rev`
/// not yet resolved, or no cache directory determinable), or nix is unavailable and `strict` is
/// false (best-effort: a warning only, since a missing nix is "not verified", not "wrong").
/// `strict` (`install --strict` only; `upgrade` has no such flag and is always best-effort here)
/// turns a missing nix into a hard `Code::Validation`. A genuine build failure (nix present, the
/// declared module set itself does not evaluate) is always a hard error regardless of `strict`:
/// that is "the declared set is broken", not merely "unverified".
fn build_and_cache_options(
    rev: &str,
    modules: &[OracleModulePin],
    strict: bool,
) -> Result<Option<String>, Code> {
    // Fetching and building the BASE (no-modules) set stays a manual step (docs/06): only the
    // AUGMENTED set (ADR 0008) is automated here. Without this guard, resolving a plain
    // per-host baseline with no module pins (ADR 0007, already working, deliberately manual)
    // would start trying to build and fetch real nixpkgs for every baseline resolution.
    if rev.is_empty() || modules.is_empty() {
        return Ok(None);
    }
    let tuples: Vec<(String, String, String)> = modules
        .iter()
        .map(|m| (m.url.clone(), m.rev.clone(), m.attr.clone()))
        .collect();
    let Some(path) = knixl_oracle::cache_path_for(rev, &tuples) else {
        return Ok(None);
    };
    let eval = knixl_nix::nixeval::NixEval::resolve();
    let json = match knixl_nix::optionsdoc::build_options_json(&eval, rev, &tuples) {
        Ok(json) => json,
        Err(knixl_nix::nixeval::NixError::Unavailable(_)) if strict => {
            eprintln!("knixl: --strict: nix unavailable, cannot build the oracle option set");
            return Err(Code::Validation);
        }
        Err(knixl_nix::nixeval::NixError::Unavailable(_)) => {
            eprintln!("warning: nix unavailable, skipping oracle option set build");
            return Ok(None);
        }
        Err(knixl_nix::nixeval::NixError::Failed(m)) => {
            eprintln!("knixl: building the oracle option set failed: {m}");
            return Err(Code::Validation);
        }
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("knixl: {}: {e}", parent.display());
            return Ok(None);
        }
    }
    if let Err(e) = std::fs::write(&path, &json) {
        eprintln!("knixl: {}: {e}", path.display());
        return Ok(None);
    }
    Ok(Some(knixl_nix::hash(json.as_bytes())))
}

/// Build + cache the augmented option set for every effective set THIS RUN changed, recording
/// each built content's hash into the matching entry of `lock` (the project's `oracle` for the
/// project-wide default set, a host's `baseline` for its own effective set) (ADR 0008). Called
/// only on confirmed apply (`upgrade --yes`, a confirmed `install`), mutating a local, not-yet-
/// written copy of the lock the caller commits: building shells out to nix and writes the cache
/// directory, so it must never run from the shared pending pre-pass (which also drives a plain
/// preview) nor from `plan`/`generate`/`check` (those stay offline).
///
/// `project_modules_changed` marks the project-wide set as changed; `changed_hosts` names hosts
/// whose own release or module override just changed. A host with no override of its own falls
/// back to the project's default set (mirroring `gather`'s `effective_modules`-equivalent
/// lookup), so its effective set also changes when the project set does, even though its own
/// entry in `changed_hosts` did not.
fn build_pending_oracle_sets(
    root: &std::path::Path,
    lock: &mut knixl_lock::Lock,
    project_modules_changed: bool,
    changed_hosts: &BTreeSet<String>,
    strict: bool,
) -> Result<(), Code> {
    if project_modules_changed {
        let rev = lock.oracle.nixpkgs_rev.clone();
        let modules = lock.oracle.modules.clone();
        if let Some(hash) = build_and_cache_options(&rev, &modules, strict)? {
            lock.oracle.options_hash = hash;
        }
    }

    use knixl_pipeline::install::list_hosts;
    let hosts = list_hosts(root).unwrap_or_default();
    let project_modules = lock.oracle.modules.clone();
    for host in &hosts {
        let Some((rev, own_modules)) = lock
            .baselines
            .get(&host.name)
            .map(|b| (b.nixpkgs_rev.clone(), b.modules.clone()))
        else {
            continue;
        };
        let src = std::fs::read_to_string(&host.path).unwrap_or_default();
        let has_override = knixl_pipeline::project::parse_host_oracle_modules(&src).is_some();
        let effective_changed = if has_override {
            changed_hosts.contains(&host.name)
        } else {
            project_modules_changed || changed_hosts.contains(&host.name)
        };
        if !effective_changed {
            continue;
        }
        let modules = if has_override {
            own_modules
        } else {
            project_modules.clone()
        };
        if let Some(hash) = build_and_cache_options(&rev, &modules, strict)? {
            lock.baselines.get_mut(&host.name).unwrap().options_hash = hash;
        }
    }
    Ok(())
}

/// The nixpkgs rev `choose_strategy` should treat as `host`'s baseline: `pending`'s rev if
/// the caller already has a pending resolution for this host in hand (issue #22 review fix:
/// no network lookup happens here any more), else the lock's already-recorded baseline for a
/// host that declares a release, else the project's global oracle rev for a host that
/// declares none. Takes `oracle_rev`/`baselines` rather than `&Ctx` (#28 review fix): the
/// interactive install's strategy closure (`make_strategy`) recomputes this per host, from
/// data captured once up front rather than a borrowed `Ctx`, since it runs off the event loop
/// and outlives any single call into `install()`.
fn effective_baseline_rev(
    oracle_rev: &str,
    baselines: &BTreeMap<String, HostBaseline>,
    host_path: &std::path::Path,
    host: &str,
    pending: Option<&HostBaseline>,
) -> String {
    if let Some(b) = pending {
        return b.nixpkgs_rev.clone();
    }
    if declared_release(host_path).is_none() {
        return oracle_rev.to_string();
    }
    baselines
        .get(host)
        .map(|b| b.nixpkgs_rev.clone())
        .unwrap_or_else(|| oracle_rev.to_string())
}

/// The `Inputs` `run` should plan `Cmd::Upgrade`/`Cmd::Install` against once `pending` has
/// resolutions in hand (issue #22 review fix): the same inputs, minus the "is not resolved:
/// run knixl upgrade" validation error for each host now pending, so those two commands can
/// reach the remedy the error names instead of being refused by it. `check`/`generate`/`plan`
/// never call this (their `pending` is always empty), so their validation error still fires.
fn patch_inputs_for_pending(
    inputs: &knixl_lock::reconcile::Inputs,
    pending: &BTreeMap<String, HostBaseline>,
) -> knixl_lock::reconcile::Inputs {
    use knixl_lock::reconcile::{ExpectedFile, Inputs};
    let validation_errors = inputs
        .validation_errors
        .iter()
        .filter(|e| {
            !pending.keys().any(|host| {
                e.starts_with(&format!("host \"{host}\": nixpkgs release \""))
                    && e.ends_with("is not resolved: run knixl upgrade")
            })
        })
        .cloned()
        .collect();
    Inputs {
        expected: inputs
            .expected
            .iter()
            .map(|e| ExpectedFile {
                path: e.path.clone(),
                hash: e.hash.clone(),
                from: e.from.clone(),
                modules: e.modules.clone(),
            })
            .collect(),
        input_hashes: inputs.input_hashes.clone(),
        validation_errors,
        referenced_pins: inputs.referenced_pins.clone(),
        declared_baselines: inputs.declared_baselines.clone(),
    }
}

/// Turn the TUI's `Outcome::Install` pin payload (the rev as a plain string, since the TUI
/// module stays decoupled from `knixl_nix`) into `commit_install`'s `version_pin` argument,
/// and commit `baseline_pending` (already resolved in memory by the caller: `run`'s pre-pass
/// for the interactive-install branch, `finish_tui_outcome`'s own resolve for the Hub flow,
/// which has no pre-pass) alongside it. The strategy for a versioned install was already
/// chosen inside the TUI's Apply-gated verify sequence (#28 Task 2), so this writes it
/// straight to the lock rather than build-testing a second time (the double-build issue #28
/// fixes). `strategy` is `None` only for an unversioned install: Apply is gated on a chosen
/// strategy whenever a version was requested (see `InstallModel::apply_allowed`), so a
/// versioned install here always carries `Some` (and `strategy_reason` alongside it, #28
/// review fix: threaded through so this prints the same `via ... (reason)` form the plain
/// path's status line does, rather than dropping the reason). `no_abi_check` is unused now
/// that the strategy decision has already been made by the time this runs; it stays a
/// parameter for symmetry with `Outcome::Install`'s other fields. Shared by the interactive
/// `install` branch and the Hub flow.
#[allow(clippy::too_many_arguments)]
fn commit_tui_install(
    host: knixl_pipeline::install::HostInfo,
    pkg: String,
    strict: bool,
    version: Option<String>,
    pin: Option<String>,
    _no_abi_check: bool,
    strategy: Option<knixl_lock::model::PinStrategy>,
    strategy_reason: Option<String>,
    baseline_pending: Option<HostBaseline>,
) -> Code {
    let version_pin = match (version.as_deref(), pin, strategy) {
        (Some(v), Some(rev), Some(strategy)) => {
            let resolved = knixl_nix::pin::Resolved { nixpkgs_rev: rev };
            Some((v, resolved, strategy))
        }
        _ => None,
    };
    let pin_args = version_pin
        .as_ref()
        .map(|(v, resolved, strategy)| (*v, resolved, *strategy));
    // The TUI/Hub commit path does not resolve project, host, or module-source pins (#35
    // phase 3 / ADR 0008 / task 5b wire only the plain `install`/`upgrade` paths; see the note
    // on `commit_install`).
    let code = commit_install(
        &host,
        &pkg,
        pin_args,
        baseline_pending.as_ref(),
        None,
        None,
        None,
        strict,
    );
    if code == Code::Clean {
        if let Some(b) = &baseline_pending {
            println!(
                "resolved nixpkgs release \"{}\" for host \"{}\" -> {}",
                b.release, host.name, b.nixpkgs_rev,
            );
        }
        if let Some((v, _, strategy)) = &version_pin {
            match &strategy_reason {
                Some(reason) => println!(
                    "pinned {pkg} {v} via {} ({reason})",
                    strategy_label(*strategy)
                ),
                None => println!("pinned {pkg} {v} via {}", strategy_label(*strategy)),
            }
        }
    }
    code
}

/// Open the TUI for the given entry: discover the project, list hosts, and inject a verify
/// function that (off the event-loop thread) drafts the host and checks it under nix.
fn open_tui(
    entry: tui::Entry,
    build: Option<tui::BuildFn>,
    pin: Option<tui::PinFn>,
    strategy: Option<tui::StrategyFn>,
) -> Result<tui::Outcome, String> {
    use knixl_pipeline::install::list_hosts;
    let root = discover_root();
    let hosts = list_hosts(&root).map_err(|e| e.to_string())?;
    let modules = browse_modules(&root);
    tui::run(
        entry,
        root.clone(),
        hosts,
        make_verify(root.clone()),
        modules,
        build,
        pin,
        strategy,
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
        if let Ok(editable) = knixl_modules::template::load_editable(&text) {
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

/// The verify function handed to the Install screen. It closes over only `Send` data (the
/// project root) and rebuilds the registry per call, so it stays `Send + Sync` for the async
/// off-thread verify. Recomputes both `pkgs.<pkg>` existence and whether the drafted host
/// parses.
fn make_verify(root: std::path::PathBuf) -> tui::VerifyFn {
    Arc::new(move |pkg: &str, host: &knixl_pipeline::install::HostInfo| {
        let formatter = default_formatter();
        let tool: semver::Version = env!("CARGO_PKG_VERSION")
            .parse()
            .expect("tool version parses");
        match knixl_pipeline::gather::gather(&root, &formatter, tool.clone()) {
            Ok(project) => {
                let (preview, parses) = preview_host(
                    &project.registry,
                    &formatter,
                    &tool,
                    host,
                    pkg,
                    &project.oracles,
                );
                let resolves = resolve_package_rev(&project.lock.oracle.nixpkgs_rev, pkg);
                tui::Verified {
                    preview,
                    resolves,
                    parses,
                }
            }
            Err(e) => tui::Verified {
                preview: format!("(preview unavailable: {e})"),
                resolves: tui::Resolve::Skipped,
                parses: tui::Parse::Skipped,
            },
        }
    })
}

/// The build function for the Install screen: builds `pkgs.<pkg>` from the lock's pinned rev
/// (ambient fallback), mapping nix errors to a coarse outcome. Closes over only `root`.
fn make_build(root: std::path::PathBuf) -> tui::BuildFn {
    use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
    Arc::new(move |pkg: &str| {
        let rev = read_pinned_rev(&root);
        let src = if rev.is_empty() {
            Nixpkgs::Ambient
        } else {
            Nixpkgs::PinnedRev(rev)
        };
        match NixEval::resolve().builds(&src, pkg) {
            Ok(()) => tui::BuildOutcome::Ok,
            Err(NixError::Unavailable(_)) => tui::BuildOutcome::Skipped,
            Err(NixError::Failed(_)) => tui::BuildOutcome::Failed,
        }
    })
}

/// The pin-resolve function for the Install screen: resolves `name@version` to a nixpkgs
/// commit via `PinResolver`, mapping resolver errors to a coarse outcome. Injected only when
/// a version was requested; takes no project state (the resolver is version-independent of
/// the current lock).
fn make_pin() -> tui::PinFn {
    use knixl_nix::pin::{PinError, PinResolver};
    Arc::new(
        move |name: &str, version: &str| match PinResolver::resolve().lookup(name, version) {
            Ok(r) => tui::PinOutcome::Resolved(r.nixpkgs_rev),
            Err(PinError::NotFound(_)) => tui::PinOutcome::NotFound,
            Err(PinError::Unavailable(_)) => tui::PinOutcome::Unavailable,
            Err(PinError::Failed(_)) => tui::PinOutcome::Failed,
        },
    )
}

/// Decide the strategy for pinning `name` at `rev`, then explain why in a short phrase for the
/// `pinned ... via ...` status line. The third element of the `Ok` tuple is whether a real
/// build attempt happened (as opposed to a skip on `no_abi_check`, absent nix, or `rev`
/// matching the baseline): callers reuse that to avoid a second, redundant build when `--build`
/// was also requested. `Err` carries both candidate build failures, verbatim from
/// `SelectError::NeitherBuilds` (which has no Display impl), for the caller to report.
fn choose_strategy(
    name: &str,
    rev: &str,
    baseline_rev: &str,
    no_abi_check: bool,
) -> Result<(knixl_lock::model::PinStrategy, String, bool), (String, String)> {
    use knixl_lock::model::PinStrategy;
    use knixl_nix::nixeval::NixEval;
    use knixl_pipeline::{select_strategy, SelectError};

    let nix_available = nix_build_available();
    let skipped = no_abi_check || !nix_available || rev == baseline_rev;
    let nix = NixEval::resolve();
    let build = |expr: &str| nix.builds_expr(expr).map_err(|e| e.to_string());
    match select_strategy(rev, baseline_rev, name, nix_available, no_abi_check, &build) {
        Ok(PinStrategy::Override) => Ok((PinStrategy::Override, "build ok".to_string(), true)),
        Ok(PinStrategy::CommitMix) if skipped => {
            let reason = if no_abi_check {
                "--no-abi-check"
            } else if !nix_available {
                "nix unavailable"
            } else {
                "matches baseline"
            };
            Ok((PinStrategy::CommitMix, reason.to_string(), false))
        }
        Ok(PinStrategy::CommitMix) => Ok((
            PinStrategy::CommitMix,
            "override build failed".to_string(),
            true,
        )),
        Err(SelectError::NeitherBuilds { commit_mix, over }) => Err((commit_mix, over)),
    }
}

/// Whether nix's build oracle (the same binary `NixEval::builds_expr` would use,
/// `KNIXL_NIX_BUILD` injectable) is present at all. Probed with `--version` rather than a real
/// evaluation, so detecting absence never touches the network. Feeds `select_strategy`'s
/// `nix_available` gate.
fn nix_build_available() -> bool {
    let nix = knixl_nix::nixeval::NixEval::resolve();
    std::process::Command::new(&nix.build_bin)
        .arg("--version")
        .output()
        .is_ok()
}

/// Maps `choose_strategy`'s decision to the TUI's `StrategyOutcome`, so `make_strategy`'s
/// closure shares `choose_strategy`'s decision logic (and the plain path's error text) instead
/// of a second copy that could drift.
fn strategy_outcome(
    name: &str,
    rev: &str,
    baseline_rev: &str,
    no_abi_check: bool,
) -> tui::StrategyOutcome {
    match choose_strategy(name, rev, baseline_rev, no_abi_check) {
        Ok((strategy, label, _tested)) => tui::StrategyOutcome::Chosen { strategy, label },
        Err((commit_mix, over)) => {
            tui::StrategyOutcome::Failed(format!("commit-mix: {commit_mix}; override: {over}"))
        }
    }
}

/// Builds the TUI's strategy-selection closure (#28): given `(name, rev, host_name)`, computes
/// `host_name`'s own baseline (reproducing `effective_baseline_rev`'s logic against data
/// captured once, up front, rather than a fixed baseline decided before the TUI ran), then runs
/// the same decision `choose_strategy` runs for the plain path. Closes over the host list (to
/// look up `host_name`'s path, for `declared_release`), the lock's recorded baselines, the
/// global oracle rev, any pending per-host baseline resolutions, and `no_abi_check`. Injected
/// into `TuiConfig` for a versioned interactive install, replacing the CLI's old second,
/// redundant build-test at commit time. Host-aware (#28 review fix) so a host switch inside the
/// TUI re-fires this against the newly selected host's baseline, not the one selected when the
/// TUI opened.
fn make_strategy(
    hosts: Vec<knixl_pipeline::install::HostInfo>,
    baselines: BTreeMap<String, HostBaseline>,
    oracle_rev: String,
    pending: BTreeMap<String, HostBaseline>,
    no_abi_check: bool,
) -> tui::StrategyFn {
    Arc::new(move |name: &str, rev: &str, host_name: &str| {
        let baseline_rev = hosts
            .iter()
            .find(|h| h.name == host_name)
            .map(|h| {
                effective_baseline_rev(
                    &oracle_rev,
                    &baselines,
                    &h.path,
                    host_name,
                    pending.get(host_name),
                )
            })
            .unwrap_or_else(|| oracle_rev.clone());
        strategy_outcome(name, rev, &baseline_rev, no_abi_check)
    })
}

/// Human-readable name for a `PinStrategy`, for the `pinned ... via ...` status line.
fn strategy_label(strategy: knixl_lock::model::PinStrategy) -> &'static str {
    match strategy {
        knixl_lock::model::PinStrategy::CommitMix => "commit-mix",
        knixl_lock::model::PinStrategy::Override => "override",
    }
}

/// The lock's pinned nixpkgs rev for `root`, or empty if unavailable.
fn read_pinned_rev(root: &std::path::Path) -> String {
    let formatter = default_formatter();
    let tool: semver::Version = env!("CARGO_PKG_VERSION")
        .parse()
        .expect("tool version parses");
    knixl_pipeline::gather::gather(root, &formatter, tool)
        .map(|p| p.lock.oracle.nixpkgs_rev)
        .unwrap_or_default()
}

/// Generate the drafted host in memory (no disk writes) and parse it, for the TUI preview.
fn preview_host(
    registry: &knixl_modules::Registry,
    formatter: &knixl_nix::Formatter,
    tool: &semver::Version,
    host: &knixl_pipeline::install::HostInfo,
    pkg: &str,
    oracles: &std::collections::BTreeMap<String, knixl_oracle::Oracle>,
) -> (String, tui::Parse) {
    use knixl_pipeline::{generate, install::add_package, HostSource};
    let src = std::fs::read_to_string(&host.path).unwrap_or_default();
    let drafted = match add_package(&src, pkg, None) {
        Ok(Some(d)) => d,
        _ => src,
    };
    let no_pins = std::collections::BTreeMap::new();
    let nix = generate(
        &[HostSource {
            path: host.path.clone(),
            src: drafted,
        }],
        registry,
        formatter,
        tool,
        oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .ok()
    .and_then(|files| {
        files
            .into_iter()
            .map(|f| f.text)
            .find(|t| t.contains("systemPackages"))
    })
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
    resolve_package_rev(&ctx.lock.oracle.nixpkgs_rev, pkg)
}

/// As `resolve_package`, but taking the pinned rev directly (for the TUI verify closure,
/// which rebuilds its own project state).
fn resolve_package_rev(rev: &str, pkg: &str) -> tui::Resolve {
    use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
    let src = if rev.is_empty() {
        Nixpkgs::Ambient
    } else {
        Nixpkgs::PinnedRev(rev.to_string())
    };
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
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("out.nix");
        let tmp = std::env::temp_dir().join(format!("knixl-parse-{}-{name}", std::process::id()));
        if std::fs::write(&tmp, text).is_err() {
            continue;
        }
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
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("knixl: tui needs an interactive terminal");
            return Code::Usage;
        }
        return match open_tui(tui::Entry::Hub, None, None, None) {
            Ok(outcome) => finish_tui_outcome(outcome),
            Err(e) => {
                eprintln!("knixl: {e}");
                Code::Internal
            }
        };
    }
    run(cli, &Ctx::load())
}

/// Act on what the hub session decided: an Install outcome commits the package; a plain quit
/// or cancel does nothing.
fn finish_tui_outcome(outcome: tui::Outcome) -> Code {
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
            let baseline_pending = match resolve_pending_baseline(&ctx, &host.name, &host.path) {
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
        let tool = env!("CARGO_PKG_VERSION")
            .parse()
            .expect("tool version parses");
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
            .map(|f| {
                format!(
                    "{{\"path\":{:?},\"state\":{:?}}}",
                    f.path.display().to_string(),
                    state_label(&f.state)
                )
            })
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
    eprintln!(
        "drift: {} was hand-edited; refusing to overwrite (use --accept-drift)",
        f.path.display()
    );
}

fn note_orphan(f: &knixl_lock::FilePlan, _json: bool) {
    eprintln!(
        "orphan: {} is no longer generated (use --prune to delete)",
        f.path.display()
    );
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
    use super::{
        choose_formatter_bin, commit_save_module, commit_tui_install, make_strategy, Code,
    };
    use crate::tui;

    /// Serializes tests that mutate `KNIXL_NIX_BUILD` (a process-global env var): cargo runs
    /// `#[test]` fns in this binary concurrently by default, so two tests each doing their own
    /// sequential set/assert/restore of the same var would otherwise race each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    /// A shim mimicking `nix-build`: exits 0 when `build_ok`, else 1. Mirrors
    /// `tests/cli.rs`'s `build_shim`; this test stubs `KNIXL_NIX_BUILD` in-process (no
    /// subprocess) since `make_strategy`'s closure runs directly, not via the `knixl` binary.
    fn build_shim(tag: &str, build_ok: bool) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("knixl-cli-mainshim-{}-{tag}", std::process::id()));
        let exit = if build_ok { 0 } else { 1 };
        let script = format!("#!/bin/sh\nexit {exit}\n");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        f.flush().unwrap();
        drop(f); // close before exec, or spawning races with ETXTBSY
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// A single-host `make_strategy` closure: `host` is not on disk, so `declared_release`
    /// finds nothing and `effective_baseline_rev` falls straight through to `oracle_rev`,
    /// mirroring what the old, single-baseline `make_strategy` test exercised.
    fn single_host_strategy_fn(oracle_rev: &str, no_abi_check: bool) -> tui::StrategyFn {
        let host = knixl_pipeline::install::HostInfo {
            name: "h".into(),
            default: false,
            path: std::path::PathBuf::from("/nonexistent/knixl-cli-make-strategy-test/h.kdl"),
        };
        make_strategy(
            vec![host],
            std::collections::BTreeMap::new(),
            oracle_rev.to_string(),
            std::collections::BTreeMap::new(),
            no_abi_check,
        )
    }

    /// `make_strategy`'s closure maps `choose_strategy`'s two outcomes: override builds ->
    /// `Chosen { Override, .. }`, neither builds -> `Failed(..)`. Both cases live in one test
    /// (rather than two `#[test]` fns) because `KNIXL_NIX_BUILD` is process-global and cargo
    /// runs tests in this binary concurrently; sharing one thread's sequential env-var swap
    /// avoids a race with itself.
    #[test]
    fn make_strategy_maps_build_outcomes() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior = std::env::var_os("KNIXL_NIX_BUILD");

        let ok_build = build_shim("override-ok", true);
        std::env::set_var("KNIXL_NIX_BUILD", &ok_build);
        let strategy_fn = single_host_strategy_fn("baseline-rev", false);
        match strategy_fn("pkg", "new-rev", "h") {
            tui::StrategyOutcome::Chosen { strategy, label } => {
                assert_eq!(strategy, knixl_lock::model::PinStrategy::Override);
                assert_eq!(label, "build ok");
            }
            tui::StrategyOutcome::Failed(msg) => panic!("expected Chosen, got Failed({msg})"),
        }
        let _ = std::fs::remove_file(&ok_build);

        let never_build = build_shim("neither-builds", false);
        std::env::set_var("KNIXL_NIX_BUILD", &never_build);
        let strategy_fn = single_host_strategy_fn("baseline-rev", false);
        match strategy_fn("pkg", "new-rev", "h") {
            tui::StrategyOutcome::Failed(msg) => {
                assert!(msg.contains("commit-mix"), "got: {msg}");
                assert!(msg.contains("override"), "got: {msg}");
            }
            tui::StrategyOutcome::Chosen { .. } => panic!("expected Failed, got Chosen"),
        }
        let _ = std::fs::remove_file(&never_build);

        match prior {
            Some(v) => std::env::set_var("KNIXL_NIX_BUILD", v),
            None => std::env::remove_var("KNIXL_NIX_BUILD"),
        }
    }

    /// #28 review fix: `make_strategy` must decide against WHICHEVER host is named at call
    /// time, not a baseline fixed once when the closure was built. Two hosts declare the same
    /// nixpkgs release but have different recorded baselines; the same `(name, rev)` pair
    /// matches host b's baseline exactly (skipped, no build) but not host a's (build-tested),
    /// proving the baseline lookup is keyed on the `host_name` argument.
    #[test]
    fn make_strategy_recomputes_the_baseline_for_the_selected_host() {
        let _guard = ENV_LOCK.lock().unwrap();
        let prior = std::env::var_os("KNIXL_NIX_BUILD");

        let root = std::env::temp_dir().join(format!(
            "knixl-cli-makestrategy-hosts-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let host_a_path = root.join("a.kdl");
        let host_b_path = root.join("b.kdl");
        std::fs::write(
            &host_a_path,
            "host \"a\" {\n    nixpkgs release=\"24.05\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            &host_b_path,
            "host \"b\" {\n    nixpkgs release=\"24.05\"\n}\n",
        )
        .unwrap();

        let mut baselines = std::collections::BTreeMap::new();
        baselines.insert(
            "a".to_string(),
            knixl_lock::model::HostBaseline {
                release: "24.05".into(),
                nixpkgs_rev: "rev-a".into(),
                options_hash: String::new(),
                modules: Vec::new(),
            },
        );
        baselines.insert(
            "b".to_string(),
            knixl_lock::model::HostBaseline {
                release: "24.05".into(),
                nixpkgs_rev: "rev-b".into(),
                options_hash: String::new(),
                modules: Vec::new(),
            },
        );
        let hosts = vec![
            knixl_pipeline::install::HostInfo {
                name: "a".into(),
                default: false,
                path: host_a_path,
            },
            knixl_pipeline::install::HostInfo {
                name: "b".into(),
                default: false,
                path: host_b_path,
            },
        ];

        // Always succeeds: a rev decided against a baseline it does NOT match runs a real
        // override build test, which this shim always passes.
        let build_ok = build_shim("host-baseline", true);
        std::env::set_var("KNIXL_NIX_BUILD", &build_ok);
        let strategy_fn = make_strategy(
            hosts,
            baselines,
            "oracle-rev".to_string(),
            std::collections::BTreeMap::new(),
            false,
        );

        // "rev-b" IS host b's own baseline: matched without a build.
        match strategy_fn("pkg", "rev-b", "b") {
            tui::StrategyOutcome::Chosen { strategy, label } => {
                assert_eq!(strategy, knixl_lock::model::PinStrategy::CommitMix);
                assert_eq!(label, "matches baseline");
            }
            tui::StrategyOutcome::Failed(msg) => panic!("expected Chosen, got Failed({msg})"),
        }

        // The SAME rev, decided against host a instead: "rev-b" != host a's baseline
        // ("rev-a"), so this one build-tests and picks Override.
        match strategy_fn("pkg", "rev-b", "a") {
            tui::StrategyOutcome::Chosen { strategy, label } => {
                assert_eq!(strategy, knixl_lock::model::PinStrategy::Override);
                assert_eq!(label, "build ok");
            }
            tui::StrategyOutcome::Failed(msg) => panic!("expected Chosen, got Failed({msg})"),
        }

        let _ = std::fs::remove_file(&build_ok);
        let _ = std::fs::remove_dir_all(&root);
        match prior {
            Some(v) => std::env::set_var("KNIXL_NIX_BUILD", v),
            None => std::env::remove_var("KNIXL_NIX_BUILD"),
        }
    }

    /// Restores process-global state (`Ctx::load`'s cwd-based root discovery, `KNIXL_FORMATTER`,
    /// `KNIXL_NIX_BUILD`) on drop, so a panicked assertion in the test below still leaves the
    /// process sane for whichever other test runs next in this binary.
    struct RestoreEnv {
        cwd: std::path::PathBuf,
        formatter: Option<std::ffi::OsString>,
        build: Option<std::ffi::OsString>,
    }
    impl Drop for RestoreEnv {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.cwd);
            match &self.formatter {
                Some(v) => std::env::set_var("KNIXL_FORMATTER", v),
                None => std::env::remove_var("KNIXL_FORMATTER"),
            }
            match &self.build {
                Some(v) => std::env::set_var("KNIXL_NIX_BUILD", v),
                None => std::env::remove_var("KNIXL_NIX_BUILD"),
            }
        }
    }

    /// #28: `commit_tui_install` used to re-derive the pin strategy itself (`choose_strategy`,
    /// deleted in Task 3), build-testing a second time even though the TUI's own verify
    /// sequence had already chosen one. This proves the fix by handing it a strategy directly
    /// with a build shim that always fails: if `commit_tui_install` still build-tested (the
    /// bug this closes), both candidates would fail and the install would exit `Validation`
    /// rather than commit `Override` (the passed-in choice, not what the shim would pick).
    #[test]
    fn commit_tui_install_reuses_the_chosen_strategy_without_a_second_build() {
        use knixl_lock::model::PinStrategy;
        use knixl_pipeline::install::HostInfo;

        let _guard = ENV_LOCK.lock().unwrap();
        let _restore = RestoreEnv {
            cwd: std::env::current_dir().unwrap(),
            formatter: std::env::var_os("KNIXL_FORMATTER"),
            build: std::env::var_os("KNIXL_NIX_BUILD"),
        };

        let root = std::env::temp_dir().join(format!("knixl-cli-committui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("hosts")).unwrap();
        std::fs::create_dir_all(root.join("modules/web-service")).unwrap();
        let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
        std::fs::copy(examples.join("hosts/web.kdl"), root.join("hosts/web.kdl")).unwrap();
        std::fs::copy(
            examples.join("../modules/web-service/knixl-module.kdl"),
            root.join("modules/web-service/knixl-module.kdl"),
        )
        .unwrap();

        std::env::set_var("KNIXL_FORMATTER", "cat");
        // Always fails: the canary that proves `commit_tui_install` never calls the build
        // oracle itself any more.
        let never_build = build_shim("commit-tui-install", false);
        std::env::set_var("KNIXL_NIX_BUILD", &never_build);
        std::env::set_current_dir(&root).unwrap(); // `Ctx::load` (inside `commit_install`) discovers the root from cwd

        let host = HostInfo {
            name: "web".into(),
            default: false,
            path: root.join("hosts/web.kdl"),
        };
        let code = commit_tui_install(
            host,
            "htop".into(),
            false,
            Some("3.2.1".into()),
            Some("abc123".into()),
            false,
            Some(PinStrategy::Override),
            Some("build ok".into()),
            None,
        );
        assert!(
            code == Code::Clean,
            "a pre-chosen strategy commits without build-testing again"
        );

        let lock = std::fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
        assert!(
            lock.contains("strategy=\"override\""),
            "the passed-in strategy is recorded verbatim: {lock}"
        );

        let _ = std::fs::remove_file(&never_build);
        let _ = std::fs::remove_dir_all(&root);
    }
}
