//! Reconcile: three concerns kept as separate axes (per-file drift, version skew, orphans).
//! Plan is a PURE function of (inputs, on-disk files, lock, running versions). No writes.
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use semver::Version;
use crate::model::{FormatterPin, Hash, Lock, OraclePin, OutputEntry};

/// Per-output-file state, derived from three hashes:
/// lock_hash (recorded), disk_hash (current), expected_hash (freshly generated).
pub enum FileState {
    /// disk == lock == expected.
    Clean,
    /// disk == lock, expected != lock. Inputs or module logic changed the output. Silent path.
    Stale { expected_hash: Hash },
    /// disk != lock. The generated file was hand-edited. TAINTED. No silent overwrite.
    Drifted { disk_hash: Hash, expected_hash: Hash },
    /// In lock, absent on disk.
    Missing { expected_hash: Hash },
    /// On disk (knixl header present) but not in lock.
    Orphaned,
}

impl FileState {
    pub fn is_drifted(&self) -> bool { matches!(self, FileState::Drifted { .. }) }
    pub fn is_dirty(&self) -> bool {
        matches!(self, FileState::Stale { .. } | FileState::Missing { .. } | FileState::Orphaned)
    }
}

/// Version skew is ORTHOGONAL to file state: it is about WHY expected differs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionSkew {
    pub tool: Option<Delta<Version>>,
    pub formatter: Option<Delta<String>>,
    pub modules: Vec<(String, Delta<Version>)>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta<T> { pub locked: T, pub running: T }

pub struct FilePlan {
    pub path: PathBuf,
    pub state: FileState,
    pub skew: Option<VersionSkew>, // Some => diff must be shown + acked before apply
}

pub struct Plan {
    pub files: Vec<FilePlan>,
    pub lock_next: Lock,
    /// Schema/oracle errors from generating expected output. Non-empty => the plan cannot
    /// be trusted; the verdict maps this to the Validation exit code.
    pub validation_errors: Vec<String>,
}

// ---- inputs to planning, gathered by the CLI layer (compute itself does no I/O) ----

/// One freshly generated output file: the bytes' hash plus the provenance the lock records.
pub struct ExpectedFile {
    pub path: PathBuf,
    pub hash: Hash,
    pub from: PathBuf,
    pub modules: Vec<String>,
}

pub struct Inputs {
    pub expected: Vec<ExpectedFile>,
    pub input_hashes: BTreeMap<PathBuf, Hash>,
    pub validation_errors: Vec<String>,
    /// Host name -> set of package names with a versioned `package` node in that host's
    /// KDL. Drives pin GC in `build_lock_next`: a pin whose package is not in its host's
    /// set (or whose host is absent entirely) is dropped.
    pub referenced_pins: BTreeMap<String, BTreeSet<String>>,
}

/// The knixl-generated files currently on disk (header present) and their hashes.
pub struct DiskState {
    pub files: BTreeMap<PathBuf, Hash>,
}

/// Versions of the running tool and its pinned boundary, for skew detection and lock_next.
pub struct Versions {
    pub tool: Version,
    pub formatter: FormatterPin,
    pub oracle: OraclePin,
    pub modules: BTreeMap<String, Version>,
}

impl Plan {
    /// Pure: derive each file's state from the three hashes, attach version skew, and build
    /// the lock a clean apply would write. No I/O, no writes.
    pub fn compute(inputs: &Inputs, disk: &DiskState, lock: &Lock, running: &Versions) -> Plan {
        let skew = compute_skew(lock, running);

        let expected: BTreeMap<PathBuf, &ExpectedFile> =
            inputs.expected.iter().map(|e| (e.path.clone(), e)).collect();
        let locked: BTreeMap<PathBuf, &Hash> =
            lock.outputs.iter().map(|o| (o.path.clone(), &o.hash)).collect();

        let mut paths: BTreeSet<PathBuf> = BTreeSet::new();
        paths.extend(expected.keys().cloned());
        paths.extend(locked.keys().cloned());
        paths.extend(disk.files.keys().cloned());

        let mut files = Vec::new();
        for path in &paths {
            let exp = expected.get(path).map(|e| e.hash.clone());
            let dsk = disk.files.get(path).cloned();
            let lck = locked.get(path).map(|h| (*h).clone());

            let state = match (exp, dsk, lck) {
                (Some(e), Some(d), Some(l)) => {
                    if d != l {
                        FileState::Drifted { disk_hash: d, expected_hash: e }
                    } else if e != l {
                        FileState::Stale { expected_hash: e }
                    } else {
                        FileState::Clean
                    }
                }
                // On disk and generated but never locked: no baseline proves a human edit,
                // so trust it when it matches expected, protect it when it does not.
                (Some(e), Some(d), None) => {
                    if d == e {
                        FileState::Stale { expected_hash: e }
                    } else {
                        FileState::Drifted { disk_hash: d, expected_hash: e }
                    }
                }
                (Some(e), None, _) => FileState::Missing { expected_hash: e },
                (None, Some(_), _) => FileState::Orphaned,
                // In the lock only, already gone from disk and no longer generated: skip.
                (None, None, _) => continue,
            };

            files.push(FilePlan { path: path.clone(), state, skew: skew.clone() });
        }

        Plan {
            files,
            lock_next: build_lock_next(inputs, lock, running),
            validation_errors: inputs.validation_errors.clone(),
        }
    }

    pub fn has_validation_errors(&self) -> bool { !self.validation_errors.is_empty() }
    pub fn any(&self, pred: fn(&FileState) -> bool) -> bool { self.files.iter().any(|f| pred(&f.state)) }

    /// Needs human ack iff Drifted, OR Stale/Missing UNDER a VersionSkew (potential regression).
    pub fn requires_ack(&self) -> bool {
        self.files.iter().any(|f| matches!(f.state, FileState::Drifted { .. })
            || (f.skew.is_some()
                && matches!(f.state, FileState::Stale { .. } | FileState::Missing { .. })))
    }

    /// A version skew that would change a still-generated file. `generate` refuses this
    /// (points at `upgrade`); it is distinct from drift, which is handled per file as exit 3.
    pub fn skew_needs_ack(&self) -> bool {
        self.files.iter().any(|f| {
            f.skew.is_some() && matches!(f.state, FileState::Stale { .. } | FileState::Missing { .. })
        })
    }
}

fn compute_skew(lock: &Lock, running: &Versions) -> Option<VersionSkew> {
    let tool = (lock.tool != running.tool)
        .then(|| Delta { locked: lock.tool.clone(), running: running.tool.clone() });
    let formatter = (lock.formatter.version != running.formatter.version).then(|| Delta {
        locked: lock.formatter.version.clone(),
        running: running.formatter.version.clone(),
    });
    let mut modules = Vec::new();
    for (name, running_v) in &running.modules {
        if let Some(locked_v) = lock.modules.get(name) {
            if locked_v != running_v {
                modules.push((
                    name.clone(),
                    Delta { locked: locked_v.clone(), running: running_v.clone() },
                ));
            }
        }
    }
    if tool.is_none() && formatter.is_none() && modules.is_empty() {
        None
    } else {
        Some(VersionSkew { tool, formatter, modules })
    }
}

fn build_lock_next(inputs: &Inputs, lock: &Lock, running: &Versions) -> Lock {
    let mut outputs: Vec<OutputEntry> = inputs
        .expected
        .iter()
        .map(|e| OutputEntry {
            path: e.path.clone(),
            hash: e.hash.clone(),
            from: e.from.clone(),
            modules: e.modules.clone(),
        })
        .collect();
    outputs.sort_by(|a, b| a.path.cmp(&b.path));
    Lock {
        version: lock.version,
        tool: running.tool.clone(),
        formatter: running.formatter.clone(),
        oracle: running.oracle.clone(),
        inputs: inputs.input_hashes.clone(),
        modules: running.modules.clone(),
        outputs,
        pins: prune_pins(&lock.pins, &inputs.referenced_pins),
    }
}

/// Drop pins with no referencing versioned `package` node: a host absent from
/// `referenced` loses all its pins; within a host, a pin whose package is not in the
/// referenced set is dropped. Hosts left with no pins are removed. Version mismatch is
/// not GC's concern (that is a generate-time validation error).
fn prune_pins(
    pins: &BTreeMap<String, Vec<crate::model::Pin>>,
    referenced: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, Vec<crate::model::Pin>> {
    let mut out = BTreeMap::new();
    for (host, list) in pins {
        let Some(refs) = referenced.get(host) else { continue };
        let kept: Vec<_> = list.iter().filter(|p| refs.contains(&p.package)).cloned().collect();
        if !kept.is_empty() {
            out.insert(host.clone(), kept);
        }
    }
    out
}

/// What the command layer did to a file.
pub enum Apply {
    Wrote(PathBuf),
    RefusedDrift(PathBuf),
    NeedsAck(PathBuf),
    DeletedOrphan(PathBuf),
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OutputEntry;

    fn ver(s: &str) -> Version { Version::parse(s).unwrap() }
    fn fmt_pin() -> FormatterPin { FormatterPin { name: "nixfmt-rfc-style".into(), version: "0.6.0".into() } }
    fn oracle_pin() -> OraclePin { OraclePin { nixpkgs_rev: "rev".into(), options_hash: "blake3:opts".into() } }
    fn versions() -> Versions {
        Versions { tool: ver("0.3.1"), formatter: fmt_pin(), oracle: oracle_pin(), modules: BTreeMap::new() }
    }
    fn empty_lock() -> Lock {
        Lock {
            version: 1,
            tool: ver("0.3.1"),
            formatter: fmt_pin(),
            oracle: oracle_pin(),
            inputs: BTreeMap::new(),
            modules: BTreeMap::new(),
            outputs: vec![],
            pins: BTreeMap::new(),
        }
    }
    fn p(s: &str) -> PathBuf { PathBuf::from(s) }
    fn expected(path: &str, hash: &str) -> ExpectedFile {
        ExpectedFile { path: p(path), hash: hash.into(), from: p("hosts/x.kdl"), modules: vec!["host".into()] }
    }
    fn inputs_with(exp: Vec<ExpectedFile>) -> Inputs {
        Inputs {
            expected: exp,
            input_hashes: BTreeMap::new(),
            validation_errors: vec![],
            referenced_pins: BTreeMap::new(),
        }
    }
    fn disk_with(entries: &[(&str, &str)]) -> DiskState {
        DiskState { files: entries.iter().map(|(pa, h)| (p(pa), h.to_string())).collect() }
    }
    fn lock_with_output(path: &str, hash: &str) -> Lock {
        let mut l = empty_lock();
        l.outputs = vec![OutputEntry { path: p(path), hash: hash.into(), from: p("hosts/x.kdl"), modules: vec!["host".into()] }];
        l
    }
    fn state_of<'a>(plan: &'a Plan, path: &str) -> &'a FileState {
        &plan.files.iter().find(|f| f.path == p(path)).expect("file present in plan").state
    }

    #[test]
    fn clean_when_all_three_hashes_match() {
        let plan = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:1")]),
            &disk_with(&[("g/a.nix", "blake3:1")]),
            &lock_with_output("g/a.nix", "blake3:1"),
            &versions(),
        );
        assert!(matches!(state_of(&plan, "g/a.nix"), FileState::Clean));
    }

    #[test]
    fn stale_when_expected_differs_but_disk_matches_lock() {
        let plan = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:2")]),
            &disk_with(&[("g/a.nix", "blake3:1")]),
            &lock_with_output("g/a.nix", "blake3:1"),
            &versions(),
        );
        assert!(matches!(state_of(&plan, "g/a.nix"), FileState::Stale { .. }));
    }

    #[test]
    fn drifted_when_disk_differs_from_lock() {
        let plan = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:2")]),
            &disk_with(&[("g/a.nix", "blake3:hand-edited")]),
            &lock_with_output("g/a.nix", "blake3:1"),
            &versions(),
        );
        assert!(matches!(state_of(&plan, "g/a.nix"), FileState::Drifted { .. }));
    }

    #[test]
    fn missing_when_in_lock_but_absent_on_disk() {
        let plan = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:1")]),
            &disk_with(&[]),
            &lock_with_output("g/a.nix", "blake3:1"),
            &versions(),
        );
        assert!(matches!(state_of(&plan, "g/a.nix"), FileState::Missing { .. }));
    }

    #[test]
    fn orphaned_when_on_disk_but_not_generated() {
        let plan = Plan::compute(
            &inputs_with(vec![]),
            &disk_with(&[("g/old.nix", "blake3:1")]),
            &empty_lock(),
            &versions(),
        );
        assert!(matches!(state_of(&plan, "g/old.nix"), FileState::Orphaned));
    }

    #[test]
    fn unlocked_on_disk_is_stale_if_it_matches_expected_else_drifted() {
        let matching = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:1")]),
            &disk_with(&[("g/a.nix", "blake3:1")]),
            &empty_lock(),
            &versions(),
        );
        assert!(matches!(state_of(&matching, "g/a.nix"), FileState::Stale { .. }));

        let differing = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:2")]),
            &disk_with(&[("g/a.nix", "blake3:1")]),
            &empty_lock(),
            &versions(),
        );
        assert!(matches!(state_of(&differing, "g/a.nix"), FileState::Drifted { .. }));
    }

    #[test]
    fn skew_is_detected_and_stale_under_skew_requires_ack() {
        let mut lock = lock_with_output("g/a.nix", "blake3:1");
        lock.modules.insert("host".into(), ver("1.0.0"));
        let mut running = versions();
        running.modules.insert("host".into(), ver("1.1.0"));

        let plan = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:2")]), // changed => Stale
            &disk_with(&[("g/a.nix", "blake3:1")]),
            &lock,
            &running,
        );
        assert!(matches!(state_of(&plan, "g/a.nix"), FileState::Stale { .. }));
        assert!(plan.files[0].skew.is_some());
        assert!(plan.requires_ack(), "stale under version skew must need ack");
    }

    #[test]
    fn no_skew_and_plain_stale_does_not_require_ack() {
        let plan = Plan::compute(
            &inputs_with(vec![expected("g/a.nix", "blake3:2")]),
            &disk_with(&[("g/a.nix", "blake3:1")]),
            &lock_with_output("g/a.nix", "blake3:1"),
            &versions(),
        );
        assert!(plan.files[0].skew.is_none());
        assert!(!plan.requires_ack());
    }

    #[test]
    fn lock_next_uses_running_versions_and_sorts_outputs() {
        let mut running = versions();
        running.tool = ver("0.9.0");
        let plan = Plan::compute(
            &inputs_with(vec![expected("g/b.nix", "blake3:b"), expected("g/a.nix", "blake3:a")]),
            &disk_with(&[]),
            &empty_lock(),
            &running,
        );
        assert_eq!(plan.lock_next.tool, ver("0.9.0"));
        let paths: Vec<_> = plan.lock_next.outputs.iter().map(|o| o.path.clone()).collect();
        assert_eq!(paths, vec![p("g/a.nix"), p("g/b.nix")]);
    }

    #[test]
    fn validation_errors_surface_on_the_plan() {
        let mut inputs = inputs_with(vec![]);
        inputs.validation_errors = vec!["services.nginx.bogus: unknown option".into()];
        let plan = Plan::compute(&inputs, &disk_with(&[]), &empty_lock(), &versions());
        assert!(plan.has_validation_errors());
        assert_eq!(plan.validation_errors.len(), 1);
    }


    #[test]
    fn build_lock_next_prunes_unreferenced_pins() {
        use crate::model::Pin;
        let mut pins = BTreeMap::new();
        pins.insert("web".to_string(), vec![
            Pin { package: "htop".into(), version: "3.2.1".into(), nixpkgs_rev: "r1".into() },
            Pin { package: "jq".into(), version: "1.7".into(), nixpkgs_rev: "r2".into() }, // unreferenced
        ]);
        pins.insert("db".to_string(), vec![ // whole host gone from KDL
            Pin { package: "ripgrep".into(), version: "14".into(), nixpkgs_rev: "r3".into() },
        ]);
        let lock = Lock { pins, ..empty_lock() };

        let mut referenced = BTreeMap::new();
        referenced.insert("web".to_string(), BTreeSet::from(["htop".to_string()]));
        // "db" absent entirely -> all its pins pruned.
        let inputs = Inputs {
            expected: vec![],
            input_hashes: BTreeMap::new(),
            validation_errors: vec![],
            referenced_pins: referenced,
        };

        let next = build_lock_next(&inputs, &lock, &versions());
        assert_eq!(next.pins.get("web").map(Vec::len), Some(1));
        assert_eq!(next.pins["web"][0].package, "htop");
        assert!(!next.pins.contains_key("db"), "removed host drops its pins");
    }
}
