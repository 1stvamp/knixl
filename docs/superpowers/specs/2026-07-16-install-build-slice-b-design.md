# knixl install --build (slice B) design

Date: 2026-07-16
Status: approved, ready for implementation plan
Issue: #14

## Problem and goal

`knixl install` today verifies a package two ways: `pkgs.<pkg>` resolves against the
lock's pinned nixpkgs rev, and the drafted host file parses as Nix. Neither proves the
package actually builds. Slice B adds an opt-in `--build` that builds the package
derivation, so a package that resolves but fails to build is caught before it is written
into a host.

The build is the package derivation only (`pkgs.<pkg>`), never the host's system
toplevel. Building the toplevel needs full NixOS-module evaluation, which `knixl-nix`
deliberately avoids: a host with a `lib.mkIf config.*` block forces `config`, which a
standalone stub cannot satisfy, so it would report false failures. Host-toplevel build is
out of scope (a later issue if the module-eval story is solved).

## Build mechanism (knixl-nix)

Add to `NixEval`:

```
pub fn builds(&self, src: &Nixpkgs, name: &str) -> Result<(), NixError>
```

It runs `nix-build --no-out-link -A <name> -E '<src.expr()>'`, where `src.expr()` is the
same pinned-rev (or ambient) nixpkgs expression the existing checks use. `--no-out-link`
avoids leaving a `result` symlink. Cache-backed builds return quickly; a source build
compiles.

The build binary is separate from the eval binary: `NixEval` gains a `build_bin`,
resolved from `KNIXL_NIX_BUILD` (default `nix-build`), parallel to the existing
`KNIXL_NIX` (default `nix-instantiate`). Tests inject a shim through `KNIXL_NIX_BUILD`.

Error mapping matches the existing checks: a missing/unspawnable binary is
`NixError::Unavailable`; a non-zero exit is `NixError::Failed(stderr)`. So nix-absent
stays a skip, not a failure.

## Verification model

The package build is host-independent and potentially slow, so it is tied to the package,
not the host:

- It runs once when the screen opens (or, on the plain path, once before the confirm).
- It re-runs when the package name changes (Enter in the package field on the TUI path).
- It does NOT re-run on a host switch.

A build status has four states: building, ok, failed, and skipped (nix unavailable). When
`--build` is not passed there is no build step and nothing new gates apply.

Apply/commit is allowed only when, in addition to the existing resolve and parse gating:

- the build is not in flight,
- the build did not fail,
- and, under `--strict`, the build was not skipped.

## Interactive (TUI) path

`knixl install <pkg> --build` opens the Install screen with one extra row, shown only when
`--build` is set:

```
build  <spinner> building        (in flight)
build  ✓ builds                  (ok)
build  ✗ build failed            (failed)
build  · build skipped           (nix unavailable)
```

The build runs as its own async `Cmd`, resolving to a `BuildDone { seq, result }` message,
with a sequence token so a superseded build (package edited again before this one returns)
is discarded. A Bubbles `spinner` animates it, driven the same way as the verify spinner.
The build function is injected into `TuiConfig` (an `Option<BuildFn>`, present only when
`--build` was requested), mirroring the injected verify function so tests supply their own
and the screen never touches nix directly.

Switching host re-runs the existing verify (preview + parse) but leaves the build result
untouched. Editing the package and pressing Enter re-runs both verify and build.

## Plain path

Non-interactive or `--yes`: after the existing `pkgs.<pkg>` resolve check and before the
`[y/N]` confirm, build the package when `--build` is set. A build failure refuses with the
Validation exit code (5); nix-absent skips with a warning unless `--strict` makes it an
error, matching the resolve check's semantics.

## Wiring

- `Cmd::Install` gains `--build`.
- `Entry::Install` carries `build: bool`.
- `TuiConfig` gains `build: Option<BuildFn>` where
  `BuildFn = Arc<dyn Fn(&str) -> BuildOutcome + Send + Sync>` and `BuildOutcome` carries
  the resulting status. Built once per package, off the event-loop thread via
  `spawn_blocking`, like the verify function.
- The CLI builds the `BuildFn` from the project root (rebuilding the lock's pinned rev
  inside the closure, as `make_verify` does), so it closes over only `Send` data.

With `--build` absent, every path is byte-for-byte unchanged: no build binary is resolved,
no build row is shown, and apply gating is exactly as today.

## Testing

- `knixl-nix`: `builds()` against a shim via `KNIXL_NIX_BUILD` for ok / failed /
  unavailable, alongside the existing eval tests.
- Install screen (pure reducers): a `BuildDone` message flips the build status; apply
  gating across building / failed / skipped, each with and without `--strict`; a stale
  build token is discarded; a host switch does not retrigger the build; a package-edit
  Enter does.
- CLI integration: `knixl install <pkg> --build` on the plain path with a failing build
  shim refuses (exit 5); with an ok shim it proceeds; nix-absent skips unless `--strict`.

The async `Cmd` glue (spawning the build, real key reads) stays untested, as with the
existing verify path.

## Out of scope

Host-toplevel build; caching build results across invocations; showing build log output in
the TUI (only the pass/fail status is surfaced). Version pinning is slice D (#15).
