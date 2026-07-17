//! Automatic pin-strategy selection (#23). Given a candidate `nixpkgs-rev`, decide whether
//! the pin can be emitted as `CommitMix` (import the whole historical package) or must fall
//! back to `Override` (build the baseline package with the historical version+src). The
//! decision is made by build-testing candidate Nix expressions; `build` is injected so this
//! module stays free of a `knixl-nix` dependency and is trivially unit-testable.

use knixl_lock::model::PinStrategy;

const NIXPKGS_URL: &str = "https://github.com/NixOS/nixpkgs";

/// Self-contained: the historical nixpkgs supplies its own package + deps.
pub fn commit_mix_test_expr(rev: &str, name: &str) -> String {
    format!(
        "(import (builtins.fetchGit {{ rev = \"{rev}\"; shallow = true; url = \"{NIXPKGS_URL}\"; }}) {{ system = builtins.currentSystem; }}).{name}"
    )
}

/// Old version+src built against the builder's baseline nixpkgs (feasibility heuristic;
/// per-host baseline revs are #22).
pub fn override_test_expr(rev: &str, name: &str) -> String {
    format!(
        "let pkgs = import <nixpkgs> {{}}; _pin = (import (builtins.fetchGit {{ rev = \"{rev}\"; shallow = true; url = \"{NIXPKGS_URL}\"; }}) {{ system = pkgs.system; }}).{name}; in pkgs.{name}.overrideAttrs ({{ ... }}: {{ src = _pin.src; version = _pin.version; }})"
    )
}

/// Neither candidate expression builds; carries both failure messages so the caller can
/// report why the pin was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectError {
    NeitherBuilds { commit_mix: String, over: String },
}

/// Decide the strategy for pinning `name` at `rev`. Skips build-testing altogether (defaulting
/// to `CommitMix`) when the caller opted out (`no_abi_check`), there is no `nix` to test with,
/// or `rev` is already the baseline (nothing has moved, so there is nothing to test). Otherwise
/// build-tests `commit_mix_test_expr` first, falling back to `override_test_expr`, and fails
/// only when neither builds.
pub fn select_strategy(
    rev: &str,
    baseline_rev: &str,
    name: &str,
    nix_available: bool,
    no_abi_check: bool,
    build: &dyn Fn(&str) -> Result<(), String>,
) -> Result<PinStrategy, SelectError> {
    if no_abi_check || !nix_available || rev == baseline_rev {
        return Ok(PinStrategy::CommitMix);
    }
    match build(&commit_mix_test_expr(rev, name)) {
        Ok(()) => Ok(PinStrategy::CommitMix),
        Err(cm) => match build(&override_test_expr(rev, name)) {
            Ok(()) => Ok(PinStrategy::Override),
            Err(ov) => Err(SelectError::NeitherBuilds { commit_mix: cm, over: ov }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records every expression it was asked to build, in order, and answers per a script
    /// keyed by exact expression string. Panics on an unscripted expression so a test that
    /// expects `build` never to be called still fails loudly if it is.
    struct FakeBuilder {
        script: BTreeMapScript,
        calls: RefCell<Vec<String>>,
    }

    // A tiny alias so the struct field above reads clearly; just a map of expr -> result.
    type BTreeMapScript = std::collections::BTreeMap<String, Result<(), String>>;

    impl FakeBuilder {
        fn new(script: Vec<(String, Result<(), String>)>) -> Self {
            FakeBuilder { script: script.into_iter().collect(), calls: RefCell::new(Vec::new()) }
        }

        fn build(&self, expr: &str) -> Result<(), String> {
            self.calls.borrow_mut().push(expr.to_string());
            self.script
                .get(expr)
                .cloned()
                .unwrap_or_else(|| panic!("unscripted build call: {expr}"))
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    const REV: &str = "abc123";
    const BASELINE: &str = "def456";
    const NAME: &str = "hello";

    #[test]
    fn no_abi_check_skips_build_and_returns_commit_mix() {
        let fake = FakeBuilder::new(vec![]);
        let got = select_strategy(REV, BASELINE, NAME, true, true, &|e| fake.build(e));
        assert_eq!(got, Ok(PinStrategy::CommitMix));
        assert!(fake.calls().is_empty(), "build must not be called when no_abi_check is set");
    }

    #[test]
    fn nix_unavailable_skips_build_and_returns_commit_mix() {
        let fake = FakeBuilder::new(vec![]);
        let got = select_strategy(REV, BASELINE, NAME, false, false, &|e| fake.build(e));
        assert_eq!(got, Ok(PinStrategy::CommitMix));
        assert!(fake.calls().is_empty(), "build must not be called when nix is unavailable");
    }

    #[test]
    fn rev_matching_baseline_skips_build_and_returns_commit_mix() {
        let fake = FakeBuilder::new(vec![]);
        let got = select_strategy(BASELINE, BASELINE, NAME, true, false, &|e| fake.build(e));
        assert_eq!(got, Ok(PinStrategy::CommitMix));
        assert!(fake.calls().is_empty(), "build must not be called when rev == baseline_rev");
    }

    #[test]
    fn commit_mix_builds_selects_commit_mix() {
        let cm = commit_mix_test_expr(REV, NAME);
        let fake = FakeBuilder::new(vec![(cm.clone(), Ok(()))]);
        let got = select_strategy(REV, BASELINE, NAME, true, false, &|e| fake.build(e));
        assert_eq!(got, Ok(PinStrategy::CommitMix));
        assert_eq!(fake.calls(), vec![cm], "only the commit-mix expr should have been built");
    }

    #[test]
    fn commit_mix_fails_override_builds_selects_override() {
        let cm = commit_mix_test_expr(REV, NAME);
        let over = override_test_expr(REV, NAME);
        let fake = FakeBuilder::new(vec![
            (cm.clone(), Err("commit-mix build failed".to_string())),
            (over.clone(), Ok(())),
        ]);
        let got = select_strategy(REV, BASELINE, NAME, true, false, &|e| fake.build(e));
        assert_eq!(got, Ok(PinStrategy::Override));
        assert_eq!(fake.calls(), vec![cm, over], "both exprs built, commit-mix then override");
    }

    #[test]
    fn both_fail_returns_neither_builds_with_both_messages() {
        let cm = commit_mix_test_expr(REV, NAME);
        let over = override_test_expr(REV, NAME);
        let fake = FakeBuilder::new(vec![
            (cm.clone(), Err("commit-mix build failed".to_string())),
            (over.clone(), Err("override build failed".to_string())),
        ]);
        let got = select_strategy(REV, BASELINE, NAME, true, false, &|e| fake.build(e));
        assert_eq!(
            got,
            Err(SelectError::NeitherBuilds {
                commit_mix: "commit-mix build failed".to_string(),
                over: "override build failed".to_string(),
            })
        );
        assert_eq!(fake.calls(), vec![cm, over]);
    }
}
