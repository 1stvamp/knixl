//! Resolving what the KDL declares but the lock does not yet record: nixpkgs baselines and
//! releases, flake inputs, oracle modules, fetched module sources and their option sets.
//! Resolution happens in memory; callers write it to the lock once confirmed.

use crate::updates::constrained_refresh;
use crate::Code;
use crate::Ctx;
use knixl_lock::model::{FlakeInputPin, HostBaseline, ModuleSourcePin, OracleModulePin};
use std::collections::{BTreeMap, BTreeSet};

/// The `nixpkgs release=".."` a host's own KDL declares, if any (issue #22). Scanned
/// straight from the file rather than the gathered project, since a target host is often
/// resolved before (or independently of) a full `gather`.
fn declared_release(host_path: &std::path::Path) -> Option<String> {
    let src = std::fs::read_to_string(host_path).ok()?;
    let doc = knixl_kdl::parse(&src).ok()?;
    let node = doc.nodes().iter().find(|n| n.name().value() == "host")?;
    knixl_kdl::child_prop_str(node, "nixpkgs", "release")
}

/// A host's declared `system` (e.g. `x86_64-linux`).
fn declared_system(host_path: &std::path::Path) -> Option<String> {
    let src = std::fs::read_to_string(host_path).ok()?;
    let doc = knixl_kdl::parse(&src).ok()?;
    let node = doc.nodes().iter().find(|n| n.name().value() == "host")?;
    knixl_kdl::child_arg_str(node, "system")
}

/// A host's declared `nixpkgs rev=".."` exact-commit pin (ADR 0014), if any.
fn declared_rev(host_path: &std::path::Path) -> Option<String> {
    let src = std::fs::read_to_string(host_path).ok()?;
    let doc = knixl_kdl::parse(&src).ok()?;
    let node = doc.nodes().iter().find(|n| n.name().value() == "host")?;
    knixl_kdl::child_prop_str(node, "nixpkgs", "rev")
}

/// The blake3 hash of the options.json cached for `rev`, or an empty string when nothing is
/// cached (best-effort, same convention as an unresolved oracle rev).
pub(crate) fn options_hash_for_rev(rev: &str) -> String {
    knixl_oracle::cache_path(rev)
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::read(&p).ok())
        .map(|bytes| knixl_nix::hash(&bytes))
        .unwrap_or_default()
}

/// The project's declared `nixpkgs release` (knixl.kdl), resolved in memory against the lock's
/// `oracle` line (#134): `Ok(None)` when they already agree, `Some(("", ""))` to clear a
/// release no longer declared, else the freshly resolved `(release, rev)`.
pub(crate) fn resolve_pending_project_release(
    ctx: &Ctx,
    refresh: bool,
    keeps: &[(String, String)],
) -> Result<Option<(String, String)>, Code> {
    let declared = knixl_pipeline::project::parse_project(&ctx.root)
        .ok()
        .and_then(|p| p.default_release);
    let recorded = &ctx.lock.oracle;
    match declared {
        None if recorded.release.is_empty() => Ok(None),
        None => Ok(Some((String::new(), String::new()))),
        Some(release)
            if release == recorded.release && !recorded.nixpkgs_rev.is_empty() && !refresh =>
        {
            Ok(None)
        }
        Some(release) => {
            let rev = if refresh && !keeps.is_empty() && release == recorded.release {
                constrained_refresh(&release, "x86_64-linux", &recorded.nixpkgs_rev, keeps)
                    .map_err(|e| {
                        eprintln!("knixl: the project nixpkgs release: {e}");
                        Code::Validation
                    })?
            } else {
                knixl_nix::baseline::BaselineResolver::resolve()
                    .lookup(&release)
                    .map_err(|e| {
                        eprintln!(
                            "knixl: cannot resolve the project nixpkgs release \"{release}\": {e}"
                        );
                        Code::Validation
                    })?
            };
            if release == recorded.release && rev == recorded.nixpkgs_rev {
                return Ok(None);
            }
            Ok(Some((release, rev)))
        }
    }
}

/// Resolve `host`'s declared `nixpkgs release` baseline IN MEMORY ONLY: no write (issue #22
/// review fix; the pre-pass used to write straight to the lock here, before the validation
/// gate and before `--yes`/confirmation). `Ok(None)` when the host declares no release, or
/// its release already matches the lock's recorded baseline (the idempotent skip); otherwise
/// `Ok(Some(baseline))` with the freshly resolved value, for the caller to plan around and
/// decide whether to keep. `Err(Code::Validation)` on a resolver failure; the message is
/// already printed, so the caller only needs to return the code.
pub(crate) fn resolve_pending_baseline(
    ctx: &Ctx,
    host: &str,
    host_path: &std::path::Path,
    refresh: bool,
    keeps: &[(String, String)],
) -> Result<Option<HostBaseline>, Code> {
    let Some(release) = declared_release(host_path) else {
        return Ok(None);
    };
    let declared = declared_rev(host_path);
    let recorded = ctx.lock.baselines.get(host);
    if let Some(b) = recorded {
        // `--refresh` re-resolves a release-tracking baseline; an exact `rev=` stays put.
        let refreshing = refresh && declared.is_none();
        if b.release == release
            && declared.as_ref().is_none_or(|r| &b.nixpkgs_rev == r)
            && !refreshing
        {
            return Ok(None);
        }
    }
    // An exact-commit pin is recorded as declared, never resolved from the release branch.
    if let Some(rev) = declared {
        if !knixl_pipeline::project::is_full_rev(&rev) {
            eprintln!(
                "knixl: host \"{host}\": nixpkgs rev \"{rev}\" is not a full 40-character commit"
            );
            return Err(Code::Validation);
        }
        let options_hash = options_hash_for_rev(&rev);
        return Ok(Some(HostBaseline {
            release,
            nixpkgs_rev: rev,
            options_hash,
            modules: Vec::new(),
        }));
    }
    let rev = match recorded {
        // `--keep`: only as far along the channel as the kept versions allow.
        Some(b) if refresh && !keeps.is_empty() && b.release == release => {
            let system = declared_system(host_path).unwrap_or_else(|| "x86_64-linux".into());
            constrained_refresh(&release, &system, &b.nixpkgs_rev, keeps).map_err(|e| {
                eprintln!("knixl: host \"{host}\": {e}");
                Code::Validation
            })?
        }
        _ => knixl_nix::baseline::BaselineResolver::resolve()
            .lookup(&release)
            .map_err(|e| {
                eprintln!(
                    "knixl: cannot resolve nixpkgs release \"{release}\" for host \"{host}\": {e}"
                );
                Code::Validation
            })?,
    };
    // A refresh that finds the branch where it was is no change; one that moves keeps the
    // host's own module pins, which only a release or override change re-resolves.
    let modules = match recorded {
        Some(b) if b.release == release && b.nixpkgs_rev == rev => return Ok(None),
        Some(b) if b.release == release => b.modules.clone(),
        _ => Vec::new(),
    };
    let options_hash = options_hash_for_rev(&rev);
    Ok(Some(HostBaseline {
        release,
        nixpkgs_rev: rev,
        options_hash,
        modules,
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
pub(crate) fn resolve_pending_baselines(
    ctx: &Ctx,
    refresh: bool,
    keeps: &[(String, String)],
) -> Result<BTreeMap<String, HostBaseline>, Code> {
    use knixl_pipeline::install::list_hosts;
    let hosts = list_hosts(&ctx.root).unwrap_or_default();
    let mut pending = BTreeMap::new();
    for h in &hosts {
        if let Some(b) = resolve_pending_baseline(ctx, &h.name, &h.path, refresh, keeps)? {
            pending.insert(h.name.clone(), b);
        }
    }
    Ok(pending)
}

/// Resolve the project's declared flake inputs other than `nixpkgs` IN MEMORY ONLY (ADR 0014;
/// mirrors `resolve_pending_project_modules`). A declared `rev=` is recorded as-is, anything
/// else resolves through the module resolver. `Ok(None)` when every input already matches its
/// pin by name, url and any declared rev (the idempotent skip), or there are none and none are
/// pinned; `Ok(Some(vec![]))` when inputs were removed and the lock still pins some (GC).
pub(crate) fn resolve_pending_flake_inputs(ctx: &Ctx) -> Result<Option<Vec<FlakeInputPin>>, Code> {
    let project = knixl_pipeline::project::parse_project(&ctx.root).unwrap_or_default();
    let declared: Vec<knixl_pipeline::project::FlakeInput> = project
        .system
        .map(|s| s.inputs)
        .unwrap_or_default()
        .into_iter()
        .filter(|i| i.name != "nixpkgs")
        .collect();
    let mut pinned: Vec<&FlakeInputPin> = ctx.lock.flake_inputs.iter().collect();
    pinned.sort_by(|a, b| a.name.cmp(&b.name));
    let up_to_date = pinned.len() == declared.len()
        && pinned.iter().zip(&declared).all(|(p, d)| {
            p.name == d.name && p.url == d.url && d.rev.as_ref().is_none_or(|r| &p.rev == r)
        });
    if up_to_date {
        return Ok(None);
    }
    declared
        .iter()
        .map(|d| {
            let rev = match &d.rev {
                Some(rev) => rev.clone(),
                None => knixl_nix::module::ModuleResolver::resolve()
                    .lookup(&d.url)
                    .map(|r| r.rev)
                    .map_err(|e| {
                        eprintln!(
                            "knixl: cannot resolve flake input \"{}\" ({}): {e}",
                            d.name, d.url
                        );
                        Code::Validation
                    })?,
            };
            if !knixl_pipeline::project::is_full_rev(&rev) {
                eprintln!(
                    "knixl: flake input \"{}\" resolved to \"{rev}\", not a full 40-character commit",
                    d.name
                );
                return Err(Code::Validation);
            }
            Ok(FlakeInputPin {
                name: d.name.clone(),
                url: d.url.clone(),
                rev,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// Resolve one declared `module "<name>" flake="<ref>" [attr="<attr>"]` (project `knixl.kdl`
/// or a host's own `oracle-modules` override) to a lock pin (#35 phase 3). `Err(Code::Validation)`
/// on a resolver failure (including an unsupported/empty flake ref -- `ModuleResolver::lookup`
/// refuses that before any resolver runs, so this never pins an empty rev); the message is
/// already printed, so the caller only needs to return the code.
fn resolve_oracle_module(
    m: &knixl_pipeline::project::OracleModule,
    input_pins: &InputPins,
) -> Result<OracleModulePin, Code> {
    if let Some(input) = &m.input {
        return match input_pins.get(input) {
            Some(Some((url, rev))) => Ok(OracleModulePin {
                name: m.name.clone(),
                url: url.clone(),
                rev: rev.clone(),
                attr: m.attr.clone(),
            }),
            Some(None) => {
                eprintln!(
                    "knixl: oracle module \"{}\": input \"{input}\" needs a github: url for the oracle to build its options",
                    m.name
                );
                Err(Code::Validation)
            }
            None => {
                eprintln!(
                    "knixl: oracle module \"{}\": flake input \"{input}\" is not resolved: run knixl upgrade",
                    m.name
                );
                Err(Code::Validation)
            }
        };
    }
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
    input_pins: &InputPins,
) -> Result<Vec<OracleModulePin>, Code> {
    modules
        .iter()
        .map(|m| resolve_oracle_module(m, input_pins))
        .collect()
}

/// Each pinned flake input's oracle source, keyed by input name: the `https://github.com/..`
/// url the options build fetches with, and the pinned rev. `None` for a non-`github:` input,
/// which the options build cannot fetch yet.
type InputPins = BTreeMap<String, Option<(String, String)>>;

pub(crate) fn flake_input_module_pins(root: &std::path::Path, pins: &[FlakeInputPin]) -> InputPins {
    let declared = knixl_pipeline::project::parse_project(root)
        .unwrap_or_default()
        .system
        .map(|s| s.inputs)
        .unwrap_or_default();
    pins.iter()
        .filter(|p| declared.iter().any(|d| d.name == p.name && d.url == p.url))
        .map(|p| {
            let source =
                knixl_nix::module::url_from_flake_ref(&p.url).map(|url| (url, p.rev.clone()));
            (p.name.clone(), source)
        })
        .collect()
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
    input_pins: &InputPins,
) -> bool {
    existing.len() == declared.len()
        && existing.iter().zip(declared.iter()).all(|(pin, m)| {
            pin.name == m.name
                && pin.attr == m.attr
                && match &m.input {
                    // An input-backed module follows its input, so it moves when the input does.
                    Some(input) => {
                        input_pins.get(input).cloned().flatten()
                            == Some((pin.url.clone(), pin.rev.clone()))
                    }
                    None => {
                        knixl_nix::module::url_from_flake_ref(&m.flake).as_deref()
                            == Some(pin.url.as_str())
                    }
                }
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
pub(crate) fn resolve_pending_project_modules(
    ctx: &Ctx,
    input_pins: &InputPins,
) -> Result<Option<Vec<OracleModulePin>>, Code> {
    let project = knixl_pipeline::project::parse_project(&ctx.root).unwrap_or_default();
    if project.oracle_modules.is_empty() {
        return Ok((!ctx.lock.oracle.modules.is_empty()).then(Vec::new));
    }
    if oracle_modules_up_to_date(
        &ctx.lock.oracle.modules,
        &project.oracle_modules,
        input_pins,
    ) {
        return Ok(None);
    }
    Ok(Some(resolve_oracle_modules(
        &project.oracle_modules,
        input_pins,
    )?))
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
pub(crate) fn resolve_pending_host_modules(
    ctx: &Ctx,
    input_pins: &InputPins,
) -> Result<BTreeMap<String, Vec<OracleModulePin>>, Code> {
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
                if oracle_modules_up_to_date(&existing, &declared, input_pins) {
                    continue;
                }
                pending.insert(
                    h.name.clone(),
                    resolve_oracle_modules(&declared, input_pins)?,
                );
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
pub(crate) struct PendingModuleSource {
    pin: ModuleSourcePin,
    fetched_text: Option<String>,
}

pub(crate) fn module_source_pins(pending: &[PendingModuleSource]) -> Vec<ModuleSourcePin> {
    pending.iter().map(|p| p.pin.clone()).collect()
}

/// Report the module sources this run resolved (not those carried forward unchanged), each
/// line starting with `verb`, e.g. "resolved" or "would resolve".
pub(crate) fn print_resolved_module_sources(pending: &[PendingModuleSource], verb: &str) {
    for p in pending.iter().filter(|p| p.fetched_text.is_some()) {
        println!(
            "{verb} module source \"{}\" ({}) -> {}",
            p.pin.name, p.pin.url, p.pin.rev
        );
    }
}

/// Whether `pin` (the lock's currently recorded pin for a declared module source) is still
/// valid against `source` (that source's current `knixl.kdl` declaration): its `path` has not
/// moved AND its cache entry still hashes to `pin.hash`. Deliberately NOT a copy of
/// `oracle_modules_up_to_date`'s name+url-only identity check: `module_cache_path` keys the
/// cache location on `path` too, so a path edit that this check ignored would leave `gather`
/// looking in the wrong place forever, and `install`/`upgrade` re-running the naive check would
/// never notice or fix it (see task 5b brief and the task 5 report's "Half 2" analysis).
///
/// Limit: this checks `path` and the cached content hash only, never the declared `flake=`
/// ref itself. Changing `flake=` while `path=` and the cache entry it resolves to stay the
/// same is not detected here, since re-checking a flake ref means a network lookup, and this
/// check exists precisely so `install`/`upgrade` can skip that lookup when nothing changed.
/// Bump the source's rev, or drop its lock pin, to force re-resolution.
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
/// A malformed `knixl.kdl` is `Err(Code::Validation)` (message already printed), never folded
/// into an empty config: unlike `resolve_pending_project_modules`, this pre-pass drives what
/// gets fetched and written, so silently treating a parse failure as "nothing declared" would
/// resolve and pin nothing while looking successful.
pub(crate) fn resolve_pending_module_sources(
    ctx: &Ctx,
) -> Result<Option<Vec<PendingModuleSource>>, Code> {
    let project = knixl_pipeline::project::parse_project(&ctx.root).map_err(|e| {
        eprintln!("knixl: {e}");
        Code::Validation
    })?;
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
pub(crate) fn write_module_source_caches(pending: &[PendingModuleSource]) -> Result<(), Code> {
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
    if rev.is_empty() {
        return Ok(None);
    }
    let tuples: Vec<(String, String, String)> = modules
        .iter()
        .map(|m| (m.url.clone(), m.rev.clone(), m.attr.clone()))
        .collect();
    let Some(path) = knixl_oracle::cache_path_for(rev, &tuples) else {
        return Ok(None);
    };
    // A set for a given rev and module pins never changes, so a cached one is reused.
    if let Ok(bytes) = std::fs::read(&path) {
        return Ok(Some(knixl_nix::hash(&bytes)));
    }
    let eval = knixl_nix::nixeval::NixEval::resolve();
    println!("building the NixOS option set for nixpkgs {rev} (a minute or so)");
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
        // The base set (no modules) failing is "unverified" (offline, say), not a broken
        // declaration, so it only warns (#128).
        Err(knixl_nix::nixeval::NixError::Failed(m)) if modules.is_empty() => {
            eprintln!("warning: building the NixOS option set for nixpkgs {rev} failed, so it is not validated: {m}");
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
pub(crate) fn build_pending_oracle_sets(
    root: &std::path::Path,
    lock: &mut knixl_lock::Lock,
    project_modules_changed: bool,
    changed_hosts: &BTreeSet<String>,
    strict: bool,
    fill_missing: bool,
) -> Result<(), Code> {
    if project_modules_changed || fill_missing {
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
        if !effective_changed && !fill_missing {
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
pub(crate) fn effective_baseline_rev(
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
pub(crate) fn patch_inputs_for_pending(
    inputs: &knixl_lock::reconcile::Inputs,
    pending: &BTreeMap<String, HostBaseline>,
    flake_inputs_pending: bool,
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
        .filter(|e| {
            !(flake_inputs_pending
                && e.starts_with("flake input \"")
                && e.ends_with("is not resolved: run knixl upgrade"))
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
