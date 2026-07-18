# TUI strategy build design (#28)

Date: 2026-07-17
Status: approved, ready for implementation plan
Issue: #28
Builds on: #23 (ADR 0006, pin strategy selection), #22 (per-host baseline)

Move pin strategy selection into the TUI's async verify sequence for a versioned install,
so the interactive `install pkg@version [--build]` builds once (the strategy build), against
the right target (the pinned expr), and `commit_tui_install` reuses the result instead of
rebuilding after Apply.

## Grounding (current state)

- Interactive install runs an async verify sequence in `crates/knixl-cli/src/tui/install.rs`:
  `verify_cmd` (schema/eval), `pin_cmd` (resolve `name@version` -> rev via `PinFn`), and
  `build_cmd` (build via `BuildFn`), each firing a `*Done` msg carrying a `seq` token that
  discards stale results. States: `PinState`, `BuildState`.
- `BuildFn` = `Arc<dyn Fn(&str) -> BuildOutcome>`. `make_build` (main.rs) reads the pinned rev
  from the lock and builds `pkgs.<pkg>` from it; for a fresh versioned install the pin is not
  written yet, so it builds the **ambient** package (the baseline version, not the requested
  one).
- Strategy selection does NOT happen in the TUI today: `commit_tui_install` (main.rs) calls
  `choose_strategy` AFTER Apply, which build-tests the strategy expr at the resolved rev.
- Result: a versioned `--build` install builds twice (ambient pre-Apply, strategy post-Apply)
  and the pre-Apply build tests the wrong package. `TuiConfig { root, hosts, entry, verify,
  modules, build, pin }`; `Nav::Apply`/`Outcome::Install { host, pkg, strict, version, pin,
  no_abi_check }`.

## Design

### StrategyFn injected into the TUI

Add `pub strategy: Option<StrategyFn>` to `TuiConfig`, where
`StrategyFn = Arc<dyn Fn(&str, &str) -> StrategyOutcome + Send + Sync>` taking `(name, rev)`
(the resolved rev from the pin step) and returning:

```rust
pub enum StrategyOutcome {
    Chosen { strategy: PinStrategy, label: String },  // label e.g. "override" / "commit-mix (override build failed)"
    Failed(String),                                    // both candidate builds failed (NeitherBuilds), joined
}
```

The closure closes over the host `baseline_rev` and `no_abi_check` (both known when the TUI is
opened), and internally runs the same logic as `choose_strategy` (`select_strategy` +
skip conditions + `strategy_label`). `PinStrategy` here is the CLI/lock enum; the TUI module
may hold a tiny mirror if needed to avoid a dependency wart, but the CLI already depends on
knixl-lock so `TuiConfig` (in the CLI's tui module) can use `knixl_lock::model::PinStrategy`
directly.

### Sequencing in install.rs

For a **versioned** install, after `PinDone` resolves the rev (`PinState::Resolved`), fire a
`strategy_cmd(seq, pkg, rev)` that calls `config().strategy`. Introduce a `StrategyState`
(`Off | Selecting | Chosen(PinStrategy) | Failed`) shown as a status row (`strategy: override`
/ `strategy: commit-mix (override build failed)` / a failure). This step REPLACES the ambient
`build` step for versioned installs: `build_cmd`/`BuildState` still run for an **unversioned**
`--build` install (no strategy to select). Apply is blocked while `Selecting`, and blocked on
`Failed` (as a failed build already blocks it).

Strategy selection runs for a versioned install whether or not `--build` is set (it is part of
pinning, mirroring the plain path). The skip conditions (`no_abi_check`, nix absent, rev ==
baseline) live inside the closure and yield `Chosen { CommitMix, .. }` with a reason label, no
build.

### Carry the choice through Apply

`Nav::Apply` and `Outcome::Install` gain `strategy: Option<PinStrategy>` (Some for a versioned
install that ran selection, None otherwise). `commit_tui_install` takes the chosen strategy and
writes the pin with it directly, DELETING its post-Apply `choose_strategy` call and the
`TODO(#23)`. The resolved rev already flows via the existing `pin` field.

### main.rs wiring

- Build the `StrategyFn` in the install setup (where `make_build`/`make_pin` are built),
  closing over `effective_baseline_rev(..)` for the target host and `no_abi_check`. Inject it
  into `TuiConfig.strategy` only for a versioned install.
- `commit_tui_install` signature: replace the internal `choose_strategy` call with the passed
  `strategy` (write the pin with it, still inside the revertable commit path, and still writing
  any `baseline_pending`).
- The unversioned path and `--build` behaviour for unversioned installs are unchanged.

## Testability

- **StrategyOutcome mapping**: a small pure/uni test that the closure maps `select_strategy`
  results to `Chosen`/`Failed` with the right label (reuse the fake-build-oracle pattern from
  the `select_strategy` tests, or a `StrategyFn` stub in the TUI model tests).
- **TUI model**: extend the install-model tests (they already drive the async states with
  stub `PinFn`/`BuildFn`) with a `StrategyFn` stub: assert a versioned install transitions
  `PinState::Resolved -> StrategyState::Chosen(..)` and does NOT enter `BuildState::Building`,
  that Apply is gated while `Selecting` and on `Failed`, and that `Nav::Apply` carries the
  chosen strategy.
- **CLI**: an integration test (shim harness) that an interactive-style versioned `--build`
  install invokes the build shim ONCE (not twice) and records the chosen strategy. If the
  interactive path is hard to drive headlessly, assert via a unit on `commit_tui_install` that
  it writes the passed strategy without calling the resolver/build again.
- Existing `--yes`/non-TTY plain-path tests and unversioned `--build` tests stay green.

## Out of scope

Changing the plain-path strategy flow (already single-build via `build_tested`); showing a
build progress bar; strategy selection for unversioned installs (there is none).
