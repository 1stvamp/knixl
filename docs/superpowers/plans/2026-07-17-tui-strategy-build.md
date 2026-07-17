# TUI strategy build implementation plan (#28)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** For an interactive versioned install, run pin strategy selection inside the TUI
verify sequence (once, correct target) and reuse it at commit, ending the double-build.

**Architecture:** A `StrategyFn` on `TuiConfig` (closes over baseline rev + no_abi_check); a
`StrategyState` step in `install.rs` that fires after the pin resolves and replaces the ambient
build for versioned installs; the chosen strategy carried through `Nav::Apply` /
`Outcome::Install` so `commit_tui_install` writes the pin without rebuilding.

**Tech Stack:** Rust, bubbletea-rs async state machine (tui/install.rs), knixl-cli.

## Global Constraints

- British spelling; no em/en-dashes. Do NOT run `cargo fmt` (hand-format to surrounding style).
- Never commit or run git/but in a task; the controller commits.
- The plain `--yes`/non-TTY path and unversioned `--build` behaviour must stay unchanged.
- Strategy selection for a versioned install runs whether or not `--build` is set (it is part of
  pinning); the skip conditions (no_abi_check, nix absent, rev == baseline) live in the closure.
- `PinStrategy` is `knixl_lock::model::PinStrategy` (the CLI already depends on knixl-lock).
- Async commands use the existing `seq`-token pattern so stale results are discarded.

---

### Task 1: StrategyFn, StrategyOutcome, TuiConfig field, and the closure (tui/mod.rs + main.rs)

**Files:**
- Modify: `crates/knixl-cli/src/tui/mod.rs` (`StrategyFn`, `StrategyOutcome`, `TuiConfig.strategy`)
- Modify: `crates/knixl-cli/src/main.rs` (build the `StrategyFn`; factor `choose_strategy` logic so both the closure and any remaining caller share it)
- Test: main.rs (the closure maps outcomes correctly)

**Interfaces:**
- Produces: `pub type StrategyFn = Arc<dyn Fn(&str, &str) -> StrategyOutcome + Send + Sync>`;
  `pub enum StrategyOutcome { Chosen { strategy: knixl_lock::model::PinStrategy, label: String }, Failed(String) }`;
  `TuiConfig { .., pub strategy: Option<StrategyFn> }`.

- [ ] **Step 1: Add the types and config field**

In `tui/mod.rs` add `StrategyOutcome` and `StrategyFn` (next to `BuildFn`/`PinFn`) and
`pub strategy: Option<StrategyFn>` on `TuiConfig`. Update every `TuiConfig { .. }` construction
(main.rs) to set `strategy: None` for now so it compiles.

- [ ] **Step 2: Failing test for the closure**

In main.rs tests, write a test that a `make_strategy(baseline_rev, no_abi_check)` closure, given
a stubbed build oracle, returns `Chosen { strategy: Override, .. }` when override builds and
`Failed(..)` when neither does. (Reuse the `select_strategy` fake-oracle pattern; if the closure
calls real nix, inject the oracle behind the same seam `choose_strategy` uses.)

- [ ] **Step 3: Build the closure**

Add `make_strategy(root_or_baseline_rev, no_abi_check) -> tui::StrategyFn` in main.rs that runs
the same decision as `choose_strategy` (call `select_strategy` + `strategy_label`), mapping
`Ok((strategy, label, _tested))` -> `StrategyOutcome::Chosen { strategy, label }` and
`Err((commit_mix, over))` -> `StrategyOutcome::Failed(format!("commit-mix: {commit_mix}; override: {over}"))`.
Factor shared logic so `choose_strategy` (still used by the plain path) and the closure do not
duplicate the mapping.

- [ ] **Step 4: Run**

Run: `cargo test -p knixl-cli` and `cargo build --workspace`. Expected: PASS (closure unused by
the TUI yet; injected in Task 3).

- [ ] **Step 5: No commit.**

---

### Task 2: Strategy step in the TUI state machine (tui/install.rs)

**Files:**
- Modify: `crates/knixl-cli/src/tui/install.rs` (`StrategyState`, `StrategyDone`, `strategy_cmd`,
  sequencing, Apply gating, carry in `Nav::Apply`)
- Modify: `crates/knixl-cli/src/tui/mod.rs` (`Nav::Apply` gains `strategy: Option<PinStrategy>`)
- Test: install.rs model tests

**Interfaces:**
- Consumes: Task 1 `StrategyFn`/`StrategyOutcome`; `config().strategy`.
- Produces: `Nav::Apply { .., strategy: Option<knixl_lock::model::PinStrategy> }`.

- [ ] **Step 1: Failing model test**

Extend the install-model tests (which drive the async states with stub `PinFn`/`BuildFn`) with a
stub `StrategyFn`: for a versioned install, assert the model transitions `PinState::Resolved` ->
`StrategyState::Selecting` -> `StrategyState::Chosen(strategy)`, that `BuildState` does NOT enter
`Building` (the ambient build is replaced), that Apply is blocked while `Selecting` and on
`Failed`, and that the emitted `Nav::Apply` carries the chosen strategy. Watch it fail.

- [ ] **Step 2: Add StrategyState + StrategyDone + strategy_cmd**

Add `enum StrategyState { Off, Selecting, Chosen(PinStrategy), Failed }`, `struct StrategyDone
{ seq, outcome: StrategyOutcome }`, and `fn strategy_cmd(seq, pkg, rev) -> Cmd` mirroring
`build_cmd`/`pin_cmd` (spawn_blocking over `config().strategy`). Store a `strategy: StrategyState`
(and the chosen `PinStrategy` + label for the view) on `InstallModel`.

- [ ] **Step 3: Sequence it after the pin, replacing the build for versioned installs**

When `PinDone` resolves the rev for a versioned install, kick `strategy_cmd` (set
`StrategyState::Selecting`) instead of the ambient `build_cmd`. For an unversioned install, the
existing `build_cmd`/`BuildState` path is unchanged. On `StrategyDone`, set `Chosen`/`Failed`
(guard on `seq`). Render a status row: `strategy: <label>` when `Chosen`, the error when `Failed`.

- [ ] **Step 4: Gate Apply and carry the strategy**

Apply is blocked while `StrategyState::Selecting` and refused on `StrategyState::Failed` (mirror
how a failed/building build gates Apply). When Apply fires for a versioned install, put the
chosen `PinStrategy` into `Nav::Apply.strategy`; `None` for unversioned.

- [ ] **Step 5: Run**

Run: `cargo test -p knixl-cli` and `cargo build --workspace`. Expected: PASS.

- [ ] **Step 6: No commit.**

---

### Task 3: Reuse at commit; inject the closure; drop the post-Apply build (mod.rs + main.rs)

**Files:**
- Modify: `crates/knixl-cli/src/tui/mod.rs` (`Outcome::Install` gains `strategy`; `Nav::Apply` ->
  `Outcome::Install` mapping threads it)
- Modify: `crates/knixl-cli/src/main.rs` (inject `StrategyFn` into `TuiConfig` for versioned
  installs; `commit_tui_install` uses the passed strategy, deletes its `choose_strategy` call and
  the `TODO(#23)`)
- Test: `crates/knixl-cli/tests/cli.rs`

**Interfaces:**
- Consumes: Task 1 closure, Task 2 `Nav::Apply.strategy`.
- Produces: `Outcome::Install { .., strategy: Option<PinStrategy> }`.

- [ ] **Step 1: Thread strategy through Outcome**

Add `strategy: Option<PinStrategy>` to `Outcome::Install` and set it from `Nav::Apply.strategy`
in the `Nav::Apply => Outcome::Install` mapping (`tui/mod.rs`).

- [ ] **Step 2: Inject the closure for versioned installs**

In main.rs where `TuiConfig` is built for `install`, set `strategy: Some(make_strategy(baseline_rev,
no_abi_check))` when a version is requested (else `None`). Compute `baseline_rev` via
`effective_baseline_rev(..)` for the target host (as `commit_tui_install` already does).

- [ ] **Step 3: commit_tui_install reuses the strategy**

Change `commit_tui_install` to take the chosen `strategy: Option<PinStrategy>` and, for a
versioned install, write the pin with it directly, DELETING the `choose_strategy` call and the
`TODO(#23)` comment. Keep the revertable baseline write. Update the `Outcome::Install` handler
call site to pass `strategy`. (If `strategy` is `None` for a versioned install because nix was
absent etc., the closure already returned `Chosen { CommitMix, .. }`, so `None` only occurs for
unversioned installs.)

- [ ] **Step 4: Failing CLI test then green**

Add a `cli.rs` test (shim harness) proving a versioned `--build` interactive-style install
invokes the build shim exactly ONCE and records the chosen strategy. If the interactive path
cannot be driven headlessly, assert on `commit_tui_install` that, given a chosen strategy, it
writes that strategy to the lock without invoking the build/resolver shims again. Run
`cargo test -p knixl-cli`; watch fail then pass.

- [ ] **Step 5: Full check**

Run: `cargo test -p knixl-cli -p knixl-pipeline`, `cargo build --workspace --tests`, and
`cargo clippy --all-targets -- -D warnings`. Also `KNIXL_FORMATTER=/home/wes/.nix-profile/bin/nixfmt cargo test -p knixl-pipeline --test golden` (unchanged). Expected: all PASS.

- [ ] **Step 6: No commit.**

---

## Self-Review

- Spec coverage: T1 types + closure; T2 the async strategy step replacing the versioned build +
  Apply gating + carry; T3 Outcome threading + commit reuse + closure injection + drop the
  post-Apply build. Together they end the double-build and fix the wrong-target build.
- Placeholders: none; the async command mirrors `build_cmd`/`pin_cmd`, and the decision mirrors
  `choose_strategy` (factored, not duplicated).
- Type consistency: `StrategyFn`/`StrategyOutcome` (T1) consumed by `strategy_cmd` (T2) and
  injected in T3; `PinStrategy` (knixl-lock) threads Nav::Apply (T2) -> Outcome::Install (T3) ->
  `commit_tui_install` (T3) under one type.
