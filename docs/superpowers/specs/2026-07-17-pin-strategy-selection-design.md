# Automatic pin strategy selection design (#23)

Date: 2026-07-17
Status: approved, ready for implementation plan
Issue: #23
Builds on: ADR 0005, ADR 0006, docs/superpowers/specs/2026-07-17-cross-rev-resolution-spike.md

Give `knixl install pkg@version` a second emit strategy (`override`), preferred when its
build succeeds and falling back to commit-mix otherwise, chosen at pin time by build-testing
the candidates and recording the winner in the lock. "Prepare for the worst": a cross-rev
pin that commit-mix cannot integrate is served by `override` without a broken host.

## Grounding (current state)

- `Pin { package, version, nixpkgs_rev }` in `crates/knixl-lock/src/model.rs`, keyed by host.
- `ResolvedPin { package, version, nixpkgs_rev }` in `crates/knixl-modules/src/lib.rs`;
  `LowerCtx::pin(name, version)` returns it; `package.rs` emits the pinned select.
- Pinned emit today (`crates/knixl-modules/src/builtin/package.rs`):
  `(import (builtins.fetchGit { shallow = true; url = ...; rev = <rev>; }) { system = pkgs.system; }).<name>`.
- `NixEval::builds(&self, src: &Nixpkgs, name: &str)` (`crates/knixl-nix/src/nixeval.rs`)
  runs `nix-build --no-out-link -A <name> -E <nixpkgs-expr>`. Nix work is injected behind
  `KNIXL_NIX*` env vars and shim-tested.
- Pin resolution + writing lives in the CLI (`install`, `commit_install`, `write_pin`,
  `make_build`/`make_pin` injected closures) in `crates/knixl-cli/src/main.rs`.

## The two strategies

Both derive from the same resolved commit (`rev`). Only the emit differs.

### commit-mix (robust fallback + safe default when selection cannot run, unchanged emit)

```nix
(import (builtins.fetchGit { rev = "<rev>"; shallow = true; url = "https://github.com/NixOS/nixpkgs"; }) { system = pkgs.system; }).<name>
```

### override (preferred when it builds)

`version` and `src` are pulled from the historical commit's package, so no separate source
hash is resolved. Emitted as a let-bound historical package, overridden onto the baseline:

```nix
(
  let
    _pin_<name> = (import (builtins.fetchGit { rev = "<rev>"; shallow = true; url = "https://github.com/NixOS/nixpkgs"; }) { system = pkgs.system; }).<name>;
  in
  pkgs.<name>.overrideAttrs ({ ... }: {
    version = _pin_<name>.version;
    src = _pin_<name>.src;
  })
)
```

Built from existing IR nodes: `Let { bindings, body }`, `Lambda { formals, body }` (an
`{ ... }:` formals that ignores the previous attrs), `Apply`, `Select`, `AttrSet`. No IR
change is needed: explicit `version =`/`src =` assignments avoid needing an `inherit (expr)`
construct. As a list element the whole `let .. in ..` is parenthesised by the emitter's
`emit_atom` (already in place). The let binding name `_pin_<name>` follows the existing
`_knixl<N>` hoist convention closely enough; keep it distinct and deterministic.

## Lock schema

`Pin` gains a strategy:

```rust
pub enum PinStrategy { CommitMix, Override }   // in knixl-lock
pub struct Pin { pub package: String, pub version: String, pub nixpkgs_rev: String, pub strategy: PinStrategy }
```

Render: `pin "<pkg>" version="<v>" nixpkgs-rev="<rev>" strategy="override"`. The `strategy`
attr is **omitted when `CommitMix`** (the default), so ADR 0005 locks and the #25 golden
lock parse and render unchanged. Parse: absent `strategy` => `CommitMix`; `"override"` =>
`Override`; any other value is a parse error.

`ResolvedPin` (knixl-modules) gains the same `strategy` field (a knixl-modules-local enum,
so knixl-modules keeps depending only on knixl-ir + knixl-oracle, not knixl-lock). The
pipeline maps `lock::Pin.strategy` to `modules::ResolvedPin.strategy` where it already maps
the other fields.

## Emit

`package.rs` branches on `pin.strategy`:
- `CommitMix` => the current select (unchanged).
- `Override` => the let/overrideAttrs expression above.

Factor the shared historical import (`import (fetchGit {rev}) { system = pkgs.system; }`)
into a helper so both arms build it once.

## Strategy selection (pin time only)

Selection runs where the commit is resolved (`install`/`upgrade`), never at generate/check.
It is injected as a closure (mirroring `make_build`/`make_pin`) so it is shim-testable and
degrades cleanly.

Choose the strategy. The cache check comes **before resolving the commit**, so a repeat
install neither hits the resolver nor builds:

0. **Cache short-circuit (before resolution):** if a lock pin for `(host, package, version)`
   already exists and the baseline rev is unchanged, reuse it verbatim (its `nixpkgs_rev` and
   `strategy`), skipping resolution, selection, and build entirely.

Then, given a freshly resolved `(name, rev)` and the host baseline:

1. **Skip conditions (no build), result = `CommitMix`** (the safe default when we cannot test):
   - `rev == baseline_rev` (no cross-rev);
   - nix is unavailable (cannot build-test): `CommitMix` with a warning (as `--build` does);
   - `--no-abi-check` was passed: `CommitMix`.
2. Otherwise **build-test override first**. If it builds => `Override` (the lean result).
3. Else **build-test commit-mix**. If it builds => `CommitMix` (the robust fallback).
4. Else the pin cannot be satisfied: refuse (exit 5), reporting both build failures.

Override is tried first on purpose: commit-mix is a self-contained historical closure that
builds by construction, so trying it first would make it the perpetual winner and leave
override as dead code. Override (old source, baseline deps) is the one that can genuinely
fail, so testing it first is what lets the picker choose between them.

When `--build` is set, the feasibility build and the `--build` package build are the same
build: run it once and reuse the result.

### The feasibility build

Add `NixEval::builds_expr(&self, expr: &str) -> Result<(), NixError>` running
`nix-build --no-out-link -E <expr>` (no `-A`). The pipeline constructs the candidate
expressions:

- commit-mix test: `(import (builtins.fetchGit { rev = "<rev>"; shallow = true; url = ...; }) { system = builtins.currentSystem; }).<name>` (self-contained; `currentSystem` since there is no host `pkgs` in the test harness).
- override test: `let pkgs = <baseline>; _pin = (import (builtins.fetchGit { rev = "<rev>"; ... }) { system = pkgs.system; }).<name>; in pkgs.<name>.overrideAttrs ({ ... }: { version = _pin.version; src = _pin.src; })`, where `<baseline>` is `import (builtins.fetchGit { rev = "<baseline_rev>"; ... }) {}` when the oracle baseline rev is recorded, else `import <nixpkgs> {}`.

The baseline `pkgs` for the override test is the oracle baseline rev when recorded, else the
builder's `<nixpkgs>`, while per-host baseline revs (#22) do not exist; this is a feasibility heuristic, documented
as such. It is good enough to catch the "old src will not build against current deps" break
that `override` risks.

## CLI and TUI

- `install` gains `--no-abi-check` (skip selection, force commit-mix).
- Interactive/TUI: selection is part of the resolve/verify step already shown; surface the
  chosen strategy as a row (e.g. `strategy: override (commit-mix failed to build)`),
  consistent with the standing TUI-first preference. Under `--yes`/non-TTY the plain path
  runs selection silently and prints one line naming the chosen strategy.
- `write_pin` records the chosen strategy.

## Testability

- **Selection logic**: pure given a build oracle. Inject a `fn(&Candidate) -> BuildResult`
  fake and unit-test the decision table: same-rev short-circuit, cached-pin reuse,
  nix-absent, `--no-abi-check`, commit-mix-passes, commit-mix-fails-override-passes,
  both-fail-refuses.
- **Emit**: unit tests for both arms in `package.rs` (structure), plus extend the #25 golden
  or add a sibling golden host with an `override`-strategy pin so the override emit is under
  the byte-for-byte + determinism goldens.
- **builds_expr**: untested glue like the other nix shell-outs, exercised via the existing
  shim pattern for success/failure exit codes.
- **Lock**: round-trip tests for `strategy="override"` and for the omitted (commit-mix)
  default, including a back-compat lock with no strategy attr.

## Out of scope

Flake-input strategy (ADR 0006: redundant with commit-mix, no different ABI outcome);
per-host baseline revs (#22, so the override test uses the builder nixpkgs); resolving a
version that no commit ships (both strategies need such a commit; unresolvable still
refuses); caching build results across invocations beyond the lock's recorded strategy.
