//! `knixl install`: choose how to pin a package, preview and verify the change, and commit
//! it (KDL, lock, generated Nix) with rollback, from the CLI or the TUI.

use crate::confirm;
use crate::default_formatter;
use crate::output::report_taint;
use crate::output::report_validation;
use crate::output::write_file;
use crate::output::write_lock;
use crate::resolve::build_pending_oracle_sets;
use crate::resolve::effective_baseline_rev;
use crate::resolve::module_source_pins;
use crate::resolve::print_resolved_module_sources;
use crate::resolve::write_module_source_caches;
use crate::resolve::PendingModuleSource;
use crate::tui;
use crate::Code;
use crate::Ctx;
use knixl_lock::model::{HostBaseline, ModuleSourcePin, OracleModulePin};
use knixl_lock::{FileState, Plan};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

// One argument per `Cmd::Install` field, plus `ctx` and the pending-baseline map/pending-module
// list threaded from `run`'s pre-pass: splitting these into a struct would obscure more than
// it saves here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn install(
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

    let (name, version) = split_version(pkg);
    let (hosts, initial) = match target_hosts(ctx, host) {
        Ok(t) => t,
        Err(code) => return code,
    };
    let unfree = unfree_by_host(&hosts, &ctx.unfree_hosts, &ctx.root);
    let initial_unfree = unfree.get(&initial.name).copied().unwrap_or(false);

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
                match choose_strategy(
                    name,
                    &resolved.nixpkgs_rev,
                    &baseline_rev,
                    no_abi_check,
                    initial_unfree,
                ) {
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
        match NixEval::resolve().builds(&src, name, initial_unfree) {
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
        if let Some(pending) = &pending_module_sources {
            print_resolved_module_sources(pending, "resolved");
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
            return Code::Validation;
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
        let pins = module_source_pins(pending);
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
            false,
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

/// Mirrors `write_pin`, but for the host baseline.
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

/// Undo `write_baseline` on `commit_install`'s revert path, so a cancelled/failed install
/// never leaves a freshly resolved baseline dangling. Mirrors `remove_pin`.
fn remove_baseline(host: &str) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.baselines.remove(host);
    write_lock(&ctx, &lock);
}

/// Unlike `write_pin`/`write_baseline` (keyed per host), this is a single, project-wide list
/// resolved from `knixl.kdl`'s `oracle-modules` block.
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

/// Set `host`'s baseline `.modules` to `pins` (ADR 0008 host override), leaving the rest of
/// the baseline unchanged. Mirrors `write_oracle_modules`, but per host. A no-op if `host`
/// has no baseline yet.
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

/// Undo `write_module_sources`: restore `module_sources` to `prior`, then write the lock
/// back. Mirrors `restore_oracle_modules`. Leaves any cache file a resolve may have written
/// untouched: the cache is content-addressed, so a stray entry is harmless.
fn restore_module_sources(prior: &[ModuleSourcePin]) {
    let ctx = Ctx::load();
    let mut lock = ctx.lock.clone();
    lock.module_sources = prior.to_vec();
    write_lock(&ctx, &lock);
}

/// What the TUI's Install screen calls to build, pin and choose a strategy. Each is absent when
/// the request doesn't need it (no `--build`, no version).
pub(crate) type InstallHooks = (
    Option<tui::BuildFn>,
    Option<tui::PinFn>,
    Option<tui::StrategyFn>,
);

/// The TUI entry and hooks for `knixl install` in a terminal: the TUI opens on the package,
/// and the hooks act on whichever host is selected.
pub(crate) fn tui_session(
    ctx: &Ctx,
    pkg: &str,
    host: Option<&str>,
    strict: bool,
    build: bool,
    no_abi_check: bool,
    pending: &BTreeMap<String, HostBaseline>,
) -> Result<(tui::Entry, InstallHooks), Code> {
    let (name, version) = split_version(pkg);
    let (hosts, initial) = target_hosts(ctx, host)?;
    let unfree = unfree_by_host(&hosts, &ctx.unfree_hosts, &ctx.root);
    let entry = tui::Entry::Install {
        pkg: name.to_string(),
        strict,
        host: Some(initial.name.clone()),
        version: version.map(str::to_string),
        no_abi_check,
    };
    let build_fn = build.then(|| make_build(ctx.root.clone(), unfree.clone()));
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
            unfree.clone(),
        )
    });
    Ok((entry, (build_fn, pin_fn, strategy_fn)))
}

fn split_version(pkg: &str) -> (&str, Option<&str>) {
    match pkg.split_once('@') {
        Some((n, v)) => (n, Some(v)),
        None => (pkg, None),
    }
}

/// The project's hosts and the one `--host` (or the default) selects.
fn target_hosts(
    ctx: &Ctx,
    host: Option<&str>,
) -> Result<
    (
        Vec<knixl_pipeline::install::HostInfo>,
        knixl_pipeline::install::HostInfo,
    ),
    Code,
> {
    use knixl_pipeline::install::{list_hosts, select_host};
    let hosts = list_hosts(&ctx.root).map_err(|e| {
        eprintln!("knixl: {e}");
        Code::Internal
    })?;
    let initial = select_host(&hosts, host)
        .map_err(|e| {
            eprintln!("knixl: {e}");
            Code::Usage
        })?
        .clone();
    Ok((hosts, initial))
}

/// Turn the TUI's `Outcome::Install` pin payload into `commit_install`'s `version_pin`
/// argument, and commit `baseline_pending` alongside it. `strategy` was already chosen
/// inside the TUI's Apply-gated verify sequence, so this writes it straight to the lock
/// rather than build-testing again; it is `None` only for an unversioned install (Apply
/// gates on a chosen strategy whenever a version was requested, see
/// `InstallModel::apply_allowed`). `no_abi_check` is unused here since the strategy decision
/// is already made; kept for parameter symmetry with `Outcome::Install`. Shared by the
/// interactive `install` branch and the Hub flow.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit_tui_install(
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

/// The verify function handed to the Install screen. It closes over only `Send` data (the
/// project root) and rebuilds the registry per call, so it stays `Send + Sync` for the async
/// off-thread verify. Recomputes both `pkgs.<pkg>` existence and whether the drafted host
/// parses.
pub(crate) fn make_verify(root: std::path::PathBuf) -> tui::VerifyFn {
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
/// (ambient fallback), allowing unfree when the selected host does, and maps nix errors to a
/// coarse outcome.
fn make_build(root: std::path::PathBuf, unfree: BTreeMap<String, bool>) -> tui::BuildFn {
    use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
    Arc::new(move |host: &str, pkg: &str| {
        let allow_unfree = unfree.get(host).copied().unwrap_or(false);
        let rev = read_pinned_rev(&root);
        let src = if rev.is_empty() {
            Nixpkgs::Ambient
        } else {
            Nixpkgs::PinnedRev(rev)
        };
        match NixEval::resolve().builds(&src, pkg, allow_unfree) {
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
    allow_unfree: bool,
) -> Result<(knixl_lock::model::PinStrategy, String, bool), (String, String)> {
    use knixl_lock::model::PinStrategy;
    use knixl_nix::nixeval::NixEval;
    use knixl_pipeline::{select_strategy, SelectError};

    let nix_available = nix_build_available();
    let skipped = no_abi_check || !nix_available || rev == baseline_rev;
    let nix = NixEval::resolve();
    let build = |expr: &str| nix.builds_expr(expr).map_err(|e| e.to_string());
    match select_strategy(
        rev,
        baseline_rev,
        name,
        nix_available,
        no_abi_check,
        allow_unfree,
        &build,
    ) {
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
    knixl_nix::nixeval::NixEval::resolve().build_available()
}

/// Maps `choose_strategy`'s decision to the TUI's `StrategyOutcome`, so `make_strategy`'s
/// closure shares `choose_strategy`'s decision logic (and the plain path's error text) instead
/// of a second copy that could drift.
fn strategy_outcome(
    name: &str,
    rev: &str,
    baseline_rev: &str,
    no_abi_check: bool,
    allow_unfree: bool,
) -> tui::StrategyOutcome {
    match choose_strategy(name, rev, baseline_rev, no_abi_check, allow_unfree) {
        Ok((strategy, label, _tested)) => tui::StrategyOutcome::Chosen { strategy, label },
        Err((commit_mix, over)) => {
            tui::StrategyOutcome::Failed(format!("commit-mix: {commit_mix}; override: {over}"))
        }
    }
}

/// Builds the TUI's strategy-selection closure: given `(name, rev, host_name)`, computes
/// `host_name`'s own baseline (reproducing `effective_baseline_rev`'s logic against data
/// captured once, up front) and runs the same decision `choose_strategy` runs for the plain
/// path. Host-aware: a host switch inside the TUI re-fires this against the newly selected
/// host's baseline, not the one selected when the TUI opened.
fn make_strategy(
    hosts: Vec<knixl_pipeline::install::HostInfo>,
    baselines: BTreeMap<String, HostBaseline>,
    oracle_rev: String,
    pending: BTreeMap<String, HostBaseline>,
    no_abi_check: bool,
    unfree: BTreeMap<String, bool>,
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
        let allow_unfree = unfree.get(host_name).copied().unwrap_or(false);
        strategy_outcome(name, rev, &baseline_rev, no_abi_check, allow_unfree)
    })
}

/// Whether each host's generated config allows unfree, keyed by host name. `unfree_hosts`
/// holds root-relative sources, while `list_hosts` paths start at `root`.
fn unfree_by_host(
    hosts: &[knixl_pipeline::install::HostInfo],
    unfree_hosts: &BTreeSet<std::path::PathBuf>,
    root: &std::path::Path,
) -> BTreeMap<String, bool> {
    hosts
        .iter()
        .map(|h| {
            let rel = h.path.strip_prefix(root).unwrap_or(&h.path);
            (h.name.clone(), unfree_hosts.contains(rel))
        })
        .collect()
}

/// Human-readable name for a `PinStrategy`, for the `pinned ... via ...` status line.
fn strategy_label(strategy: knixl_lock::model::PinStrategy) -> &'static str {
    match strategy {
        knixl_lock::model::PinStrategy::CommitMix => "commit-mix",
        knixl_lock::model::PinStrategy::Override => "override",
    }
}

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

#[cfg(test)]
mod tests {
    use super::{commit_tui_install, make_strategy};
    use crate::tui;
    use crate::Code;

    /// Serializes tests that mutate `KNIXL_NIX_BUILD` (a process-global env var): cargo runs
    /// `#[test]` fns in this binary concurrently by default, so two tests each doing their own
    /// sequential set/assert/restore of the same var would otherwise race each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
            std::collections::BTreeMap::new(),
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

    /// `make_strategy` must decide against whichever host is named at call time, not a
    /// baseline fixed when the closure was built. Two hosts share a nixpkgs release but have
    /// different recorded baselines, so the same `(name, rev)` matches one host's baseline
    /// (skipped) but not the other's (build-tested).
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
            std::collections::BTreeMap::new(),
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

    /// `commit_tui_install` must use the strategy the TUI already chose, not re-derive it via
    /// a second build test. Proven with an always-failing build shim: if it still
    /// build-tested, both candidates would fail and the install would exit `Validation`
    /// instead of committing the passed-in `Override`.
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
            examples.join("../crates/knixl-modules/stdlib/web-service/knixl-module.kdl"),
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
