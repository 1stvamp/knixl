# Automatic pin strategy selection implementation plan (#23)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give `knixl install pkg@version` a second emit strategy (`override`) chosen
automatically at pin time by build-testing candidates, recorded per pin in the lock.

**Architecture:** A `PinStrategy` enum lives on the lock `Pin` (default `CommitMix`, omitted
in render for back-compat) and mirrors onto `ResolvedPin` in knixl-modules. `package.rs`
emits per strategy. `NixEval::builds_expr` build-tests a raw expression. A pure
`select_strategy` in knixl-pipeline runs the decision table over an injected build oracle.
The CLI does the cache short-circuit, resolves, selects, and records the strategy.

**Tech Stack:** Rust workspace (knixl-lock, knixl-modules, knixl-nix, knixl-pipeline,
knixl-cli), KDL, nixfmt via `KNIXL_FORMATTER`, nix-build via `KNIXL_NIX_BUILD`.

## Global Constraints

- British spelling in prose/comments; no em/en-dashes (colons, parentheses, commas, full stops).
- Deterministic emit: `BTreeMap`/`BTreeSet`, stable order, no `HashMap` in emit paths.
- KDL is authoritative; generated Nix is derived. `Plan::compute` and generate/check stay pure
  and offline. Building happens only at pin time (install/upgrade).
- knixl-modules depends only on knixl-ir + knixl-oracle + knixl-kdl (NOT knixl-lock, NOT
  knixl-nix): its `ResolvedPin` strategy is a modules-local enum, converted in the pipeline.
- Back-compat: an ADR 0005 lock (no `strategy` attr) must parse and render byte-identically;
  `strategy` is rendered only when `Override`.
- commit-mix is the preferred default; `override` is the fallback used only when commit-mix's
  emitted-into-host build fails. Flakes are out of scope (ADR 0006).
- The fixed pin rev in fixtures is a committed literal; generate stays offline.
- nixfmt is at `~/.nix-profile/bin/nixfmt` (set `KNIXL_FORMATTER` for byte-for-byte goldens).

---

### Task 1: PinStrategy on the lock Pin (knixl-lock)

**Files:**
- Modify: `crates/knixl-lock/src/model.rs` (`PinStrategy` enum, `Pin.strategy`, parse, render)
- Test: `crates/knixl-lock/src/model.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub enum PinStrategy { CommitMix, Override }` (derive `Debug, Clone, Copy, PartialEq, Eq`);
  `Pin { package: String, version: String, nixpkgs_rev: String, strategy: PinStrategy }`.

- [ ] **Step 1: Write failing round-trip tests**

Add to the model tests: (a) a lock with `pin "htop" version="3.2.1" nixpkgs-rev="r" strategy="override"`
parses to `strategy: PinStrategy::Override` and renders back identically; (b) a lock whose pin
has no `strategy` attr parses to `PinStrategy::CommitMix` and renders WITHOUT a `strategy` attr
(byte-for-byte back-compat); (c) an unknown `strategy="bogus"` is a parse error.

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-lock strategy`
Expected: FAIL (field/enum absent).

- [ ] **Step 3: Add the enum and field**

Add `PinStrategy` and the `strategy` field to `Pin`. In the pin parser (the `host { pin ... }`
block), read `prop_str(p, "strategy")`: `None` => `CommitMix`, `Some("override")` => `Override`,
`Some(other)` => a parse error (mirror the crate's existing parse-error style). In the pin
renderer, append ` strategy="override"` only when `strategy == Override`; emit nothing for
`CommitMix`. Update the pins-sort and any `Pin { .. }` literal in existing tests to include
`strategy: PinStrategy::CommitMix`.

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p knixl-lock`
Expected: PASS (new + existing, including the existing pin round-trip which now has an implicit
`CommitMix`).

- [ ] **Step 5: Commit-free handoff.** Do not commit (controller commits).

---

### Task 2: Override emit (knixl-modules) + pipeline strategy mapping

**Files:**
- Modify: `crates/knixl-modules/src/lib.rs` (`ResolvedPin` gains a modules-local strategy enum)
- Modify: `crates/knixl-modules/src/builtin/package.rs` (emit branches on strategy)
- Modify: `crates/knixl-pipeline/src/lib.rs` (map `lock::Pin.strategy` -> `ResolvedPin.strategy`)
- Test: `crates/knixl-modules/src/builtin/package.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: Task 1's `lock::PinStrategy`.
- Produces: `modules::PinStrategy { CommitMix, Override }` (modules-local, same variants);
  `ResolvedPin { package, version, nixpkgs_rev, strategy: PinStrategy }`.

- [ ] **Step 1: Write failing emit tests**

In `package.rs` tests, add two cases exercising `lower` (or a factored emit helper) with a
`ResolvedPin` in `LowerCtx`:
- `CommitMix` => the emitted `NixExpr` renders (via `knixl_ir` emit) to
  `(import (builtins.fetchGit { rev = "<rev>"; shallow = true; url = "https://github.com/NixOS/nixpkgs"; }) { system = pkgs.system; }).htop` (the current behaviour; assert on the rendered string with `knixl_ir::emit` `Writer`, matching how existing package tests assert).
- `Override` => renders to a `let _pin = (import (builtins.fetchGit { ... }) { system = pkgs.system; }).htop; in pkgs.htop.overrideAttrs ({ ... }: { src = _pin.src; version = _pin.version; })`.

Mirror the assertion style already used by the existing versioned-package test in this file.

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-modules package`
Expected: FAIL (strategy field/enum + override arm absent).

- [ ] **Step 3: Add the modules-local enum and field**

In `crates/knixl-modules/src/lib.rs`, add `pub enum PinStrategy { CommitMix, Override }`
(derive `Debug, Clone, Copy, PartialEq, Eq`) and add `pub strategy: PinStrategy` to
`ResolvedPin`. Update any `ResolvedPin { .. }` construction in this crate's tests.

- [ ] **Step 4: Branch the emit**

In `package.rs`, factor the shared historical import into a helper, e.g.
`fn historical_pkg(rev: &str, name: &str) -> NixExpr` returning
`Select(Apply(import, [fetchGit{rev}, {system = pkgs.system}]), [name])` (exactly today's
construction). Then in the `Some(version)` arm branch on `pin.strategy`:
- `CommitMix` => `historical_pkg(rev, name)` (unchanged output).
- `Override` => build:

```rust
// let _pin = <historical_pkg>; in pkgs.<name>.overrideAttrs ({ ... }: { src = _pin.src; version = _pin.version; })
use knixl_ir::{Binding, Formals};
let bind = "_pin".to_string();
let mut attrs = std::collections::BTreeMap::new();
attrs.insert(AttrKey::Ident("src".into()),
    NixExpr::Select(Box::new(NixExpr::Ref(bind.clone())), vec!["src".into()]));
attrs.insert(AttrKey::Ident("version".into()),
    NixExpr::Select(Box::new(NixExpr::Ref(bind.clone())), vec!["version".into()]));
let lambda = NixExpr::Lambda {
    formals: Formals { args: vec![], ellipsis: true }, // `{ ... }:` ignores previous attrs
    body: Box::new(NixExpr::AttrSet(attrs)),
};
let overridden = NixExpr::Apply(
    Box::new(NixExpr::Select(
        Box::new(NixExpr::Select(Box::new(NixExpr::Ref("pkgs".into())), vec![name.clone()])),
        vec!["overrideAttrs".into()],
    )),
    vec![lambda],
);
NixExpr::Let {
    bindings: vec![Binding { name: bind, value: historical_pkg(&pin.nixpkgs_rev, &name) }],
    body: Box::new(overridden),
}
```

(Confirm the exact `Binding`/`Formals` field names against `crates/knixl-ir/src/expr.rs`; the
above matches the shapes used in `emit.rs` tests. `_pin` is safe: each package node emits its
own self-contained `let`, so there is no cross-element collision.)

- [ ] **Step 5: Map the strategy in the pipeline**

In `crates/knixl-pipeline/src/lib.rs`, where `lock::Pin` is mapped to `ResolvedPin`, convert
`lock::PinStrategy` -> `modules::PinStrategy` (a small match). Add the field to that literal.

- [ ] **Step 6: Run, watch pass**

Run: `cargo test -p knixl-modules -p knixl-pipeline`
Expected: PASS.

- [ ] **Step 7: No commit.**

---

### Task 3: builds_expr on NixEval (knixl-nix)

**Files:**
- Modify: `crates/knixl-nix/src/nixeval.rs` (`builds_expr` method + shim test)

**Interfaces:**
- Produces: `pub fn builds_expr(&self, expr: &str) -> Result<(), NixError>`.

- [ ] **Step 1: Write failing shim tests**

Mirror the existing `builds` shim tests: one where the `nix-build` shim exits 0 (=> `Ok`), one
where it exits non-zero (=> `Err(NixError::Failed(..))`), and one with a non-existent build_bin
(=> `Err(NixError::Unavailable(..))`).

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-nix builds_expr`
Expected: FAIL (method absent).

- [ ] **Step 3: Implement**

Add, mirroring `builds` but with `-E <expr>` and no `-A`:

```rust
/// Build a raw expression, proving it evaluates and its derivation builds. Used at pin time
/// to feasibility-test a candidate emit strategy. `--no-out-link` avoids a `result` symlink.
pub fn builds_expr(&self, expr: &str) -> Result<(), NixError> {
    let out = crate::output_retrying_etxtbsy(|| {
        let mut c = Command::new(&self.build_bin);
        c.args(["--no-out-link", "-E", expr]);
        c
    })
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            NixError::Unavailable(format!("{} not found", self.build_bin.display()))
        } else {
            NixError::Unavailable(e.to_string())
        }
    })?;
    if out.status.success() {
        Ok(())
    } else {
        Err(NixError::Failed(String::from_utf8_lossy(&out.stderr).trim().to_string()))
    }
}
```

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p knixl-nix`
Expected: PASS.

- [ ] **Step 5: No commit.**

---

### Task 4: Strategy selection decision (knixl-pipeline)

**Files:**
- Create: `crates/knixl-pipeline/src/strategy.rs` (candidate exprs + `select_strategy`)
- Modify: `crates/knixl-pipeline/src/lib.rs` (`mod strategy; pub use ...`)
- Test: `crates/knixl-pipeline/src/strategy.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `lock::PinStrategy`.
- Produces:
  - `pub fn commit_mix_test_expr(rev: &str, name: &str) -> String`
  - `pub fn override_test_expr(rev: &str, name: &str) -> String`
  - `pub enum SelectError { NeitherBuilds { commit_mix: String, over: String } }`
  - `pub fn select_strategy(rev: &str, baseline_rev: &str, name: &str, nix_available: bool, no_abi_check: bool, build: &dyn Fn(&str) -> Result<(), String>) -> Result<lock::PinStrategy, SelectError>`

- [ ] **Step 1: Write failing decision-table tests**

With a fake `build` closure keyed on the expression string, assert:
- `no_abi_check == true` => `CommitMix`, build never called.
- `!nix_available` => `CommitMix`, build never called.
- `rev == baseline_rev` => `CommitMix`, build never called.
- commit-mix builds => `CommitMix` (only the commit-mix expr was built).
- commit-mix fails, override builds => `Override` (both exprs built, in that order).
- both fail => `Err(SelectError::NeitherBuilds { .. })` carrying both messages.

Assert the fake saw the expected expressions (use `commit_mix_test_expr`/`override_test_expr`
to build expectations).

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-pipeline strategy`
Expected: FAIL (module absent).

- [ ] **Step 3: Implement**

```rust
use knixl_lock::model::PinStrategy;

const NIXPKGS_URL: &str = "https://github.com/NixOS/nixpkgs";

pub fn commit_mix_test_expr(rev: &str, name: &str) -> String {
    // Self-contained: the historical nixpkgs supplies its own package + deps.
    format!(
        "(import (builtins.fetchGit {{ rev = \"{rev}\"; shallow = true; url = \"{NIXPKGS_URL}\"; }}) {{ system = builtins.currentSystem; }}).{name}"
    )
}

pub fn override_test_expr(rev: &str, name: &str) -> String {
    // Old version+src built against the builder's baseline nixpkgs (feasibility heuristic;
    // per-host baseline revs are #22).
    format!(
        "let pkgs = import <nixpkgs> {{}}; _pin = (import (builtins.fetchGit {{ rev = \"{rev}\"; shallow = true; url = \"{NIXPKGS_URL}\"; }}) {{ system = pkgs.system; }}).{name}; in pkgs.{name}.overrideAttrs ({{ ... }}: {{ src = _pin.src; version = _pin.version; }})"
    )
}

pub enum SelectError { NeitherBuilds { commit_mix: String, over: String } }

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
```

Wire `mod strategy;` and re-export the public items from `lib.rs`.

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p knixl-pipeline`
Expected: PASS.

- [ ] **Step 5: No commit.**

---

### Task 5: CLI wiring (knixl-cli)

**Files:**
- Modify: `crates/knixl-cli/src/main.rs` (`install` flow, `--no-abi-check`, record strategy)
- Test: `crates/knixl-cli/tests/cli.rs`

**Interfaces:**
- Consumes: Task 1 `PinStrategy`, Task 3 `builds_expr`, Task 4 `select_strategy`.

- [ ] **Step 1: Add the flag**

Add `--no-abi-check` to `install` (default false), parsed like the existing `--build`/`--strict`
flags.

- [ ] **Step 2: Cache short-circuit before resolution**

In `install`, before calling the resolver, look up the current lock for a pin matching
`(host, package, version)`. If found and `lock.oracle.nixpkgs_rev` (the baseline) is unchanged
from what that pin was created under (for now: the baseline is the single oracle rev, so simply
"a pin exists for this (host, pkg, version)"), reuse its `nixpkgs_rev` and `strategy` verbatim,
skipping resolution and selection. (Idempotent repeat install: no network, no build.)

- [ ] **Step 3: Resolve, then select**

When not cached: resolve the commit as today, then build a `build` closure over
`NixEval::builds_expr` (constructed from the same injected nix as `make_build`; nix-absent =>
`nix_available = false`). Call `select_strategy(rev, baseline_rev, name, nix_available,
no_abi_check, &build)`. On `Ok(strategy)` proceed; on `Err(NeitherBuilds { .. })` refuse with
exit 5, printing both failures. When `--build` is also set, run the commit-mix feasibility build
once and reuse it as the `--build` check (do not build twice).

- [ ] **Step 4: Record the strategy**

Thread the chosen `PinStrategy` into `write_pin` so the lock pin records it. Under `--yes`/
non-TTY print one line naming the chosen strategy and why (e.g. `pinned htop 3.2.1 via override
(commit-mix build failed)`). In the interactive TUI, surface the chosen strategy as a status
row (mirror how `--build`/pin state already appear); this can be a follow-up row using the same
Outcome plumbing.

- [ ] **Step 5: CLI test**

Add a `cli.rs` test using the existing nix/resolver shim harness: install a `pkg@version` where
the injected build shim fails commit-mix but passes override, assert the written lock pin has
`strategy="override"`. Add a second asserting `--no-abi-check` records `commit-mix` without
invoking the build shim.

- [ ] **Step 6: Run**

Run: `cargo test -p knixl-cli`
Expected: PASS.

- [ ] **Step 7: No commit.**

---

### Task 6: Golden for the override emit path

**Files:**
- Create: `examples/hosts/pinned-override.kdl`
- Create: `examples/expected/pinned-override.nix`
- Modify: `examples/knixl.lock.kdl` (a pin block with `strategy="override"`)
- Test: `crates/knixl-pipeline/tests/golden.rs`

**Interfaces:**
- Consumes: Task 2 emit, Task 1 lock schema.

- [ ] **Step 1: Fixture host**

`examples/hosts/pinned-override.kdl`:

```kdl
host "pinned-override" {
    system "x86_64-linux"
    package "htop" version="3.2.1"
}
```

- [ ] **Step 2: Lock pin with override strategy**

Add to `examples/knixl.lock.kdl`:

```kdl
    host "pinned-override" {
        pin "htop" version="3.2.1" nixpkgs-rev="0000000000000000000000000000000000000abc" strategy="override"
    }
```

- [ ] **Step 3: Failing golden test**

Add `pinned_override_matches_golden` mirroring `pinned_matches_golden` (thread `lock.pins`,
generate `hosts/pinned-override.kdl`, compare to `expected/pinned-override.nix`), gated on
`formatter_available()`.

- [ ] **Step 4: Run, watch fail**

Run: `KNIXL_FORMATTER=/home/wes/.nix-profile/bin/nixfmt cargo test -p knixl-pipeline --test golden pinned_override_matches_golden`
Expected: FAIL (no expected file yet).

- [ ] **Step 5: Produce and sanity-check the golden**

Generate the expected bytes through the same path (temporary dump test or CLI generate), save to
`examples/expected/pinned-override.nix`, then delete the dump. READ it: it must contain the
`(let _pin = (import (builtins.fetchGit { ... }) { system = pkgs.system; }).htop; in pkgs.htop.overrideAttrs ({ ... }: { src = _pin.src; version = _pin.version; }))` shape, plus the knixl header.

- [ ] **Step 6: Verify with real nix (optional but recommended)**

`nix-instantiate --parse examples/expected/pinned-override.nix` must succeed.

- [ ] **Step 7: Run the golden + determinism**

Run: `KNIXL_FORMATTER=/home/wes/.nix-profile/bin/nixfmt cargo test -p knixl-pipeline --test golden`
Expected: all PASS.

- [ ] **Step 8: No commit.**

---

## Self-Review

- Spec coverage: Task 1 = lock schema + back-compat; Task 2 = override emit + modules/pipeline
  strategy plumbing; Task 3 = `builds_expr`; Task 4 = selection decision table + candidate exprs
  + skip conditions (no-abi-check, nix-absent, same-rev) with the cache short-circuit handled in
  Task 5; Task 5 = CLI flag, cache short-circuit, resolve+select, record, `--build` reuse; Task
  6 = override golden.
- Placeholders: the one tool-produced artifact is `examples/expected/pinned-override.nix`
  (Task 6 Step 5 gives the command); IR field names in Task 2 are "confirm against expr.rs"
  because they must mirror current code, not be invented.
- Type consistency: `PinStrategy` (lock) defined Task 1, mirrored as `modules::PinStrategy`
  Task 2, consumed by `select_strategy` (Task 4) and CLI (Task 5) under those names;
  `select_strategy`/`builds_expr` signatures match their call sites.
