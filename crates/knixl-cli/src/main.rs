//! knixl CLI. Every command is a thin policy over one Plan. Plan::compute is the only
//! thing that inspects the world. Exit codes are stable so CI can branch on them.
//! SPEC-GRADE SKETCH: Ctx::load and the write/report helpers are not written.

mod tui;

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use knixl_lock::model::HostBaseline;
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
    if plan.has_validation_errors() { return Code::Validation; }
    if plan.any(FileState::is_drifted) { return Code::Drift; }
    if plan.requires_ack() { return Code::NeedsAck; }
    if plan.any(FileState::is_dirty) { return Code::RegenPending; }
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

    // Plan off a lock/inputs patched with `pending` merged in, so the "not resolved: run
    // knixl upgrade" validation error does not block the very commands that would resolve it,
    // and `lock_next` (built from this patched lock) carries the pending baselines for
    // `Cmd::Upgrade` to write verbatim. `check`/`generate`/`plan` never populate `pending`, so
    // they always plan off the on-disk lock and inputs unchanged.
    let patched_lock = (!pending.is_empty()).then(|| {
        let mut lock = ctx.lock.clone();
        lock.baselines.extend(pending.iter().map(|(host, b)| (host.clone(), b.clone())));
        lock
    });
    let patched_inputs = (!pending.is_empty()).then(|| patch_inputs_for_pending(&ctx.inputs, &pending));
    let lock_for_plan = patched_lock.as_ref().unwrap_or(&ctx.lock);
    let inputs_for_plan = patched_inputs.as_ref().unwrap_or(&ctx.inputs);

    // Plan::compute is pure; validation errors ride on the plan (verdict maps them to
    // the Validation exit code), so there is no fallible generation step here.
    let plan = Plan::compute(inputs_for_plan, &ctx.disk, lock_for_plan, &ctx.running);
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
            // A pending baseline resolution is work to do even when every file is Clean and
            // nothing needs ack: skip the "up to date" short-circuit so it gets previewed
            // (and, on --yes, written) below instead of silently vanishing.
            if !plan.requires_ack() && !plan.any(FileState::is_dirty) && pending.is_empty() {
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
                eprintln!("re-run with --yes to apply");
                return Code::NeedsAck;
            }
            for f in &plan.files {
                if !matches!(f.state, FileState::Clean) { write_file(ctx, f); }
            }
            for (host, b) in &pending {
                println!(
                    "resolved nixpkgs release \"{}\" for host \"{host}\" -> {}",
                    b.release, b.nixpkgs_rev,
                );
            }
            // `plan.lock_next` was built from the lock already patched with `pending` (see
            // `run`'s planning step above), so this one write commits the baselines too.
            write_lock(ctx, &plan.lock_next); // bump tool/module/formatter/oracle/baselines
            Code::Clean
        }

        Cmd::Doc { node } => { print_doc(ctx, &node, cli.json); Code::Clean }

        Cmd::Install { pkg, host, yes, strict, build, no_abi_check } => {
            install(ctx, &pkg, host.as_deref(), yes, strict, build, no_abi_check, &pending)
        }

        Cmd::Tui => unreachable!("tui is dispatched before Ctx::load"),
    }
}

// One argument per `Cmd::Install` field, plus `ctx` and the pending-baseline map threaded
// from `run`'s pre-pass: splitting these into a struct would obscure more than it saves here.
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
) -> Code {
    use knixl_pipeline::install::{list_hosts, select_host};
    use knixl_nix::pin::{PinError, PinResolver};

    let (name, version) = match pkg.split_once('@') {
        Some((n, v)) => (n, Some(v)),
        None => (pkg, None),
    };

    let hosts = match list_hosts(&ctx.root) {
        Ok(h) => h,
        Err(e) => { eprintln!("knixl: {e}"); return Code::Internal; }
    };
    let initial = match select_host(&hosts, host) {
        Ok(t) => t.clone(),
        Err(e) => { eprintln!("knixl: {e}"); return Code::Usage; }
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
        // #28: decide the pin strategy once, up front, and inject it into the Install
        // screen's Apply-gated verify sequence, rather than build-testing a second time at
        // commit (`commit_tui_install`'s old `choose_strategy` call). `initial` is the target
        // host before the TUI runs; a host switch inside the TUI does not re-derive this
        // (mirrors `make_build`/`make_pin`, which are also fixed at `initial`/host-independent).
        let strategy_fn = version.is_some().then(|| {
            let baseline_rev =
                effective_baseline_rev(ctx, &initial.path, &initial.name, pending.get(&initial.name));
            make_strategy(baseline_rev, no_abi_check)
        });
        return match open_tui(entry, build_fn, pin_fn, strategy_fn) {
            Ok(tui::Outcome::Install { host, pkg, strict, version, pin, no_abi_check, strategy }) => {
                // The TUI may switch the target host from `initial`: look the pending
                // resolution up for whichever host was actually chosen.
                let baseline_pending = pending.get(&host.name).cloned();
                commit_tui_install(host, pkg, strict, version, pin, no_abi_check, strategy, baseline_pending)
            }
            Ok(_) => { println!("cancelled"); Code::Clean }
            Err(e) => { eprintln!("knixl: tui: {e}"); Code::Internal }
        };
    }

    // The pending baseline resolution for the target host, if `run`'s pre-pass resolved one
    // (issue #22 review fix): written alongside the pin by `commit_install`, in the same
    // committed/revertable step, rather than up front by a pre-pass.
    let baseline_pending = pending.get(&initial.name).cloned();

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
                (knixl_nix::pin::Resolved { nixpkgs_rev: p.nixpkgs_rev.clone() }, p.strategy, "cached".to_string())
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
                let baseline_rev =
                    effective_baseline_rev(ctx, &initial.path, &initial.name, baseline_pending.as_ref());
                match choose_strategy(name, &resolved.nixpkgs_rev, &baseline_rev, no_abi_check) {
                    Ok((strategy, reason, tested)) => {
                        build_tested = tested;
                        (resolved, strategy, reason)
                    }
                    Err((commit_mix, over)) => {
                        eprintln!("knixl: cannot pin {name}@{v}: commit-mix build failed: {commit_mix}");
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
        let src = if rev.is_empty() { Nixpkgs::Ambient } else { Nixpkgs::PinnedRev(rev.clone()) };
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
    let pin_args = version_pin.as_ref().map(|(v, resolved, strategy, _)| (*v, resolved, *strategy));
    let code = commit_install(&initial, name, pin_args, baseline_pending.as_ref(), strict);
    if code == Code::Clean {
        if let Some(b) = &baseline_pending {
            println!(
                "resolved nixpkgs release \"{}\" for host \"{}\" -> {}",
                b.release, initial.name, b.nixpkgs_rev,
            );
        }
        if let Some((v, _, strategy, reason)) = &version_pin {
            println!("pinned {name} {v} via {} ({reason})", strategy_label(*strategy));
        }
    }
    code
}

fn commit_install(
    chosen: &knixl_pipeline::install::HostInfo,
    pkg: &str,
    version_pin: Option<(&str, &knixl_nix::pin::Resolved, knixl_lock::model::PinStrategy)>,
    baseline_pending: Option<&HostBaseline>,
    strict: bool,
) -> Code {
    use knixl_pipeline::install::add_package;

    let original = match std::fs::read_to_string(&chosen.path) {
        Ok(s) => s,
        Err(e) => { eprintln!("knixl: {}: {e}", chosen.path.display()); return Code::Internal; }
    };
    let draft = match add_package(&original, pkg, version_pin.map(|(v, _, _)| v)) {
        Ok(Some(d)) => d,
        Ok(None) => { println!("{pkg} is already installed on {}", chosen.name); return Code::Clean; }
        Err(e) => { eprintln!("knixl: cannot edit {}: {e}", chosen.path.display()); return Code::Internal; }
    };

    // Record the pin and any pending baseline resolution before the versioned KDL hits disk
    // (issue #22 review fix: a baseline resolved for this install used to be written by a
    // pre-pass before confirmation; it is now written here, in the same committed/revertable
    // step as the pin): `generate` treats a `package` node with a `version` prop and no
    // matching lock pin as an error, so writing the KDL first would (briefly, but for real,
    // since the next line's gather sees it) make the project fail to generate even on the
    // success path.
    if let Some((v, resolved, strategy)) = version_pin {
        write_pin(&chosen.name, pkg, v, resolved, strategy);
    }
    if let Some(b) = baseline_pending {
        write_baseline(&chosen.name, &b.release, &b.nixpkgs_rev, &b.options_hash);
    }

    if let Err(e) = std::fs::write(&chosen.path, &draft) {
        eprintln!("knixl: {}: {e}", chosen.path.display());
        // The pin/baseline above may already be on disk even though the KDL write failed:
        // undo them so a failed install never leaves either dangling.
        if version_pin.is_some() {
            remove_pin(&chosen.name, pkg);
        }
        if baseline_pending.is_some() {
            remove_baseline(&chosen.name);
        }
        return Code::Internal;
    }
    // Undo the pin and baseline along with the KDL: both may have been written above, before
    // the KDL hit disk, so an abort past this point must not leave either dangling in the lock.
    let revert = || {
        let _ = std::fs::write(&chosen.path, &original);
        if version_pin.is_some() {
            remove_pin(&chosen.name, pkg);
        }
        if baseline_pending.is_some() {
            remove_baseline(&chosen.name);
        }
    };

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
    let rev = knixl_nix::baseline::BaselineResolver::resolve().lookup(&release).map_err(|e| {
        eprintln!("knixl: cannot resolve nixpkgs release \"{release}\" for host \"{host}\": {e}");
        Code::Validation
    })?;
    let options_hash = options_hash_for_rev(&rev);
    Ok(Some(HostBaseline { release, nixpkgs_rev: rev, options_hash }))
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

/// The nixpkgs rev `choose_strategy` should treat as `host`'s baseline: `pending`'s rev if
/// the caller already has a pending resolution for this host in hand (issue #22 review fix:
/// no network lookup happens here any more), else the lock's already-recorded baseline for a
/// host that declares a release, else the project's global oracle rev for a host that
/// declares none.
fn effective_baseline_rev(
    ctx: &Ctx,
    host_path: &std::path::Path,
    host: &str,
    pending: Option<&HostBaseline>,
) -> String {
    if let Some(b) = pending {
        return b.nixpkgs_rev.clone();
    }
    if declared_release(host_path).is_none() {
        return ctx.lock.oracle.nixpkgs_rev.clone();
    }
    ctx.lock
        .baselines
        .get(host)
        .map(|b| b.nixpkgs_rev.clone())
        .unwrap_or_else(|| ctx.lock.oracle.nixpkgs_rev.clone())
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
/// versioned install here always carries `Some`. `no_abi_check` is unused now that the
/// strategy decision has already been made by the time this runs; it stays a parameter for
/// symmetry with `Outcome::Install`'s other fields. Shared by the interactive `install`
/// branch and the Hub flow.
#[allow(clippy::too_many_arguments)]
fn commit_tui_install(
    host: knixl_pipeline::install::HostInfo,
    pkg: String,
    strict: bool,
    version: Option<String>,
    pin: Option<String>,
    _no_abi_check: bool,
    strategy: Option<knixl_lock::model::PinStrategy>,
    baseline_pending: Option<HostBaseline>,
) -> Code {
    let version_pin = match (version.as_deref(), pin, strategy) {
        (Some(v), Some(rev), Some(strategy)) => {
            let resolved = knixl_nix::pin::Resolved { nixpkgs_rev: rev };
            Some((v, resolved, strategy))
        }
        _ => None,
    };
    let pin_args = version_pin.as_ref().map(|(v, resolved, strategy)| (*v, resolved, *strategy));
    let code = commit_install(&host, &pkg, pin_args, baseline_pending.as_ref(), strict);
    if code == Code::Clean {
        if let Some(b) = &baseline_pending {
            println!(
                "resolved nixpkgs release \"{}\" for host \"{}\" -> {}",
                b.release, host.name, b.nixpkgs_rev,
            );
        }
        if let Some((v, _, strategy)) = &version_pin {
            println!("pinned {pkg} {v} via {}", strategy_label(*strategy));
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
    tui::run(entry, root.clone(), hosts, make_verify(root.clone()), modules, build, pin, strategy)
}

/// Enumerate registered modules for the Browse screen: node name, kind tag, rendered schema
/// doc, and a host-insertion skeleton. Built here (not in the TUI) since the registry is not
/// `Send`. An unreadable project yields an empty list rather than failing the whole TUI.
fn browse_modules(root: &std::path::Path) -> Vec<tui::BrowseModule> {
    use knixl_modules::ModuleKind;
    let Ok(registry) = knixl_pipeline::gather::registry(root) else {
        return Vec::new();
    };
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
            }
        })
        .collect()
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
            ValueTy::Enum(opts) => {
                opts.first().map(|o| format!("\"{o}\"")).unwrap_or_else(|| "\"\"".to_string())
            }
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
        let tool: semver::Version =
            env!("CARGO_PKG_VERSION").parse().expect("tool version parses");
        match knixl_pipeline::gather::gather(&root, &formatter, tool.clone()) {
            Ok(project) => {
                let (preview, parses) =
                    preview_host(&project.registry, &formatter, &tool, host, pkg, &project.oracles);
                let resolves = resolve_package_rev(&project.lock.oracle.nixpkgs_rev, pkg);
                tui::Verified { preview, resolves, parses }
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
        let src = if rev.is_empty() { Nixpkgs::Ambient } else { Nixpkgs::PinnedRev(rev) };
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
    Arc::new(move |name: &str, version: &str| match PinResolver::resolve().lookup(name, version) {
        Ok(r) => tui::PinOutcome::Resolved(r.nixpkgs_rev),
        Err(PinError::NotFound(_)) => tui::PinOutcome::NotFound,
        Err(PinError::Unavailable(_)) => tui::PinOutcome::Unavailable,
        Err(PinError::Failed(_)) => tui::PinOutcome::Failed,
    })
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
        Ok(PinStrategy::CommitMix) => {
            Ok((PinStrategy::CommitMix, "override build failed".to_string(), true))
        }
        Err(SelectError::NeitherBuilds { commit_mix, over }) => Err((commit_mix, over)),
    }
}

/// Whether nix's build oracle (the same binary `NixEval::builds_expr` would use,
/// `KNIXL_NIX_BUILD` injectable) is present at all. Probed with `--version` rather than a real
/// evaluation, so detecting absence never touches the network. Feeds `select_strategy`'s
/// `nix_available` gate.
fn nix_build_available() -> bool {
    let nix = knixl_nix::nixeval::NixEval::resolve();
    std::process::Command::new(&nix.build_bin).arg("--version").output().is_ok()
}

/// Maps `choose_strategy`'s decision to the TUI's `StrategyOutcome`, so `make_strategy`'s
/// closure shares `choose_strategy`'s decision logic (and the plain path's error text) instead
/// of a second copy that could drift.
fn strategy_outcome(name: &str, rev: &str, baseline_rev: &str, no_abi_check: bool) -> tui::StrategyOutcome {
    match choose_strategy(name, rev, baseline_rev, no_abi_check) {
        Ok((strategy, label, _tested)) => tui::StrategyOutcome::Chosen { strategy, label },
        Err((commit_mix, over)) => {
            tui::StrategyOutcome::Failed(format!("commit-mix: {commit_mix}; override: {over}"))
        }
    }
}

/// Builds the TUI's strategy-selection closure (#28): given `(name, rev)`, runs the same
/// decision `choose_strategy` runs for the plain path, closing over `baseline_rev` and
/// `no_abi_check`. Injected into `TuiConfig` for a versioned interactive install, replacing
/// the CLI's old second, redundant build-test at commit time.
fn make_strategy(baseline_rev: String, no_abi_check: bool) -> tui::StrategyFn {
    Arc::new(move |name: &str, rev: &str| strategy_outcome(name, rev, &baseline_rev, no_abi_check))
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
    let tool: semver::Version = env!("CARGO_PKG_VERSION").parse().expect("tool version parses");
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
        &[HostSource { path: host.path.clone(), src: drafted }],
        registry,
        formatter,
        tool,
        oracles,
        &no_pins,
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
    resolve_package_rev(&ctx.lock.oracle.nixpkgs_rev, pkg)
}

/// As `resolve_package`, but taking the pinned rev directly (for the TUI verify closure,
/// which rebuilds its own project state).
fn resolve_package_rev(rev: &str, pkg: &str) -> tui::Resolve {
    use knixl_nix::nixeval::{NixError, NixEval, Nixpkgs};
    let src = if rev.is_empty() { Nixpkgs::Ambient } else { Nixpkgs::PinnedRev(rev.to_string()) };
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
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            eprintln!("knixl: tui needs an interactive terminal");
            return Code::Usage;
        }
        return match open_tui(tui::Entry::Hub, None, None, None) {
            Ok(outcome) => finish_tui_outcome(outcome),
            Err(e) => { eprintln!("knixl: {e}"); Code::Internal }
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
        // requests a version (`Entry::Hub` seeds `version: None`), so `strategy` is always
        // `None` here too.
        tui::Outcome::Install { host, pkg, strict, version, pin, no_abi_check, strategy } => {
            let ctx = Ctx::load();
            let baseline_pending = match resolve_pending_baseline(&ctx, &host.name, &host.path) {
                Ok(b) => b,
                Err(code) => return code,
            };
            commit_tui_install(host, pkg, strict, version, pin, no_abi_check, strategy, baseline_pending)
        }
        tui::Outcome::Insert { host, node, skeleton } => commit_insert(&host, &node, &skeleton),
        tui::Outcome::Scaffold { name, manifest } => commit_scaffold(&name, &manifest),
        tui::Outcome::Cancelled | tui::Outcome::Quit => Code::Clean,
    }
}

/// Write a scaffolded module manifest to `modules/<name>/knixl-module.kdl`, refusing to
/// overwrite an existing module.
fn commit_scaffold(name: &str, manifest: &str) -> Code {
    let dir = discover_root().join("modules").join(name);
    let path = dir.join("knixl-module.kdl");
    if path.exists() {
        eprintln!("knixl: module `{name}` already exists at {}", path.display());
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
    println!("created module `{name}`: edit {} then declare it on a host", path.display());
    Code::Clean
}

/// Scaffold a module node into a host's KDL: splice the skeleton and write the file. Unlike
/// install this does not regenerate, since the skeleton is a starting point the user edits
/// before running `knixl generate`.
fn commit_insert(host: &knixl_pipeline::install::HostInfo, node: &str, skeleton: &str) -> Code {
    use knixl_pipeline::install::add_node;
    let original = match std::fs::read_to_string(&host.path) {
        Ok(s) => s,
        Err(e) => { eprintln!("knixl: {}: {e}", host.path.display()); return Code::Internal; }
    };
    match add_node(&original, node, skeleton) {
        Ok(Some(draft)) => {
            if let Err(e) = std::fs::write(&host.path, &draft) {
                eprintln!("knixl: {}: {e}", host.path.display());
                return Code::Internal;
            }
            println!("added {node} to {}: edit {} then run `knixl generate`", host.name, host.path.display());
            Code::Clean
        }
        Ok(None) => { println!("{node} is already declared on {}", host.name); Code::Clean }
        Err(e) => { eprintln!("knixl: cannot edit {}: {e}", host.path.display()); Code::Internal }
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
    use super::{choose_formatter_bin, commit_tui_install, make_strategy, Code};
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
        assert_eq!(choose_formatter_bin(None, |b| b == "nixfmt-rfc-style"), "nixfmt-rfc-style");
    }

    #[test]
    fn defaults_to_nixfmt_when_none_run() {
        assert_eq!(choose_formatter_bin(None, |_| false), "nixfmt");
    }

    /// A shim mimicking `nix-build`: exits 0 when `build_ok`, else 1. Mirrors
    /// `tests/cli.rs`'s `build_shim`; this test stubs `KNIXL_NIX_BUILD` in-process (no
    /// subprocess) since `make_strategy`'s closure runs directly, not via the `knixl` binary.
    fn build_shim(tag: &str, build_ok: bool) -> std::path::PathBuf {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("knixl-cli-mainshim-{}-{tag}", std::process::id()));
        let exit = if build_ok { 0 } else { 1 };
        let script = format!("#!/bin/sh\nexit {exit}\n");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(script.as_bytes()).unwrap();
        f.flush().unwrap();
        drop(f); // close before exec, or spawning races with ETXTBSY
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
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
        let strategy_fn = make_strategy("baseline-rev".to_string(), false);
        match strategy_fn("pkg", "new-rev") {
            tui::StrategyOutcome::Chosen { strategy, label } => {
                assert_eq!(strategy, knixl_lock::model::PinStrategy::Override);
                assert_eq!(label, "build ok");
            }
            tui::StrategyOutcome::Failed(msg) => panic!("expected Chosen, got Failed({msg})"),
        }
        let _ = std::fs::remove_file(&ok_build);

        let never_build = build_shim("neither-builds", false);
        std::env::set_var("KNIXL_NIX_BUILD", &never_build);
        let strategy_fn = make_strategy("baseline-rev".to_string(), false);
        match strategy_fn("pkg", "new-rev") {
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

        let host = HostInfo { name: "web".into(), default: false, path: root.join("hosts/web.kdl") };
        let code = commit_tui_install(
            host,
            "htop".into(),
            false,
            Some("3.2.1".into()),
            Some("abc123".into()),
            false,
            Some(PinStrategy::Override),
            None,
        );
        assert!(code == Code::Clean, "a pre-chosen strategy commits without build-testing again");

        let lock = std::fs::read_to_string(root.join("knixl.lock.kdl")).unwrap();
        assert!(lock.contains("strategy=\"override\""), "the passed-in strategy is recorded verbatim: {lock}");

        let _ = std::fs::remove_file(&never_build);
        let _ = std::fs::remove_dir_all(&root);
    }
}
