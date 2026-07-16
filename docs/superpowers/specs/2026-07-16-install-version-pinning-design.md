# knixl install pkg@version: per-host version pinning design

Date: 2026-07-16
Status: approved, ready for implementation plan
Issue: #15
Decision record: docs/adr/0005-package-version-pinning.md

## Problem and goal

Let a user pin a specific version of a package on a specific host
(`knixl install pkg@version`), while unpinned packages keep coming from the
host's baseline nixpkgs. One host may run several packages at different
versions. See ADR 0005 for why this is historical-commit mixing and not
`overrideAttrs`/flakes, and for the risks (external index, ABI mismatch).

Scope for this spec: the global baseline rev is unchanged; only per-package
pins are per host. Per-host baseline revs are out of scope.

## Model

- KDL declares intent: `package "htop" version="3.2.1"` under a host.
- The lock records the resolved pin per host:
  `host "laptop" { pin "htop" version="3.2.1" nixpkgs-rev="<commit>" }`.
- The generator emits a pinned package inline, as an import of its locked
  commit selecting the package straight out of it, mixed into the host
  baseline:
  ```nix
  { environment.systemPackages = [ pkgs.ripgrep (import (builtins.fetchGit { url = "https://github.com/NixOS/nixpkgs"; rev = "<commit>"; }) { system = pkgs.system; }).htop ]; }
  ```
  A full 40-char git rev is a complete pure pin on its own, so `fetchGit` needs
  no sha256. There is no `_pin_<name>` let binding; identical repeated imports
  (same url and rev) are deduped by the existing hoisting pass where eligible,
  the same as any other repeated attrset.
- Resolution (version to commit) happens only at `install`/`upgrade`; the
  result is locked, so `generate`/`check` are offline and pure. A KDL-declared
  version with no matching lock pin is a Validation error.

## Components

### 1. Lock schema: per-host pins

`knixl-lock` gains a per-host pin list. Lock KDL grows an optional `host`
block:
```
host "laptop" {
    pin "htop" version="3.2.1" nixpkgs-rev="<commit>"
}
```
Model: `Lock` gains `pins: BTreeMap<String /*host*/, Vec<Pin>>` where
`Pin { package: String, version: String, nixpkgs_rev: String }`.
Deterministic order (BTreeMap by host, pins sorted by package). Parse and
serialise round-trip (existing lock tests pattern). Absent `host` blocks mean no
pins (back-compat: existing locks parse unchanged).

Reconcile (`Plan::compute` / `knixl-lock::reconcile`): a pin is an input to
generation. If the KDL declares `package "x" version="v"` on host H and the lock
has no `pin "x" version="v"` for H, that is a validation error surfaced on the
plan (Validation exit 5), message: `htop 3.2.1 on laptop is not resolved: run
knixl install to pin it`.

### 2. Injected resolver

New `knixl-nix::pin` module:
```
pub struct PinResolver { pub bin: PathBuf }         // KNIXL_PIN_RESOLVER, default the bundled resolver
pub struct Resolved { pub nixpkgs_rev: String }
pub enum PinError { Unavailable(String), NotFound(String), Failed(String) }
impl PinResolver {
    pub fn resolve() -> PinResolver;                 // KNIXL_PIN_RESOLVER or default
    pub fn lookup(&self, name: &str, version: &str) -> Result<Resolved, PinError>;
}
```
`lookup` runs the resolver command as `<bin> <name> <version>` and expects one
line `"<commit>"` on stdout (exit 0). Non-zero with a "not found" marker maps
to `NotFound`; other non-zero to `Failed`; spawn failure to `Unavailable`. The
default `bin` is a small script (shipped under the CLI, e.g. `resolver`
invoking nixhub.io via curl); it is never called in tests, which inject a shim
via `KNIXL_PIN_RESOLVER` (mirrors the `KNIXL_NIX` shim pattern in
`nixeval.rs`). Offline/absent resolver is `Unavailable`, which blocks the pin
with a clear error, never a wrong pin.

### 3. package module: version emit

The built-in `package` module (`crates/knixl-modules/src/builtin/package.rs`)
gains an optional `version` prop in its schema. When absent, emit
`environment.systemPackages = [ pkgs.<name> ]` exactly as today (unchanged).
When present, the module needs the pin's rev; it is threaded from the lock
into the lowering context so emit stays pure and deterministic:

- `LowerCtx` gains a pin lookup for the current host: `fn pin(&self, package,
  version) -> Option<&Pin>` (the pins for `Scope.host`).
- The pipeline (`knixl-pipeline::generate` / `gather`) passes the lock's
  per-host pins into `LowerCtx`.
- The module emits the pinned import inline in `systemPackages`, as `(import
  (builtins.fetchGit { url; rev; }) { system = pkgs.system; }).<name>`. There
  is no `_pin_<name>` binding; the existing let-hoisting pass
  (`knixl-ir::hoist`) still dedups the inner `{ url; rev; }` attrset when it
  repeats, where eligible.
- If `version` is set but no pin is threaded (should not happen post-reconcile,
  but defensively), it is a `LowerError` mirroring the reconcile validation.

Determinism: pin imports emit in stable order; the `fetchGit` url/rev are
byte-stable from the lock.

### 4. install pkg@version + accept/deny

`knixl install` accepts `pkg@version` (parse the `@`; bare `pkg` unchanged).
The flow, per host (install already targets a host):

- Resolve `version` to a `commit` via the injected resolver. On
  `NotFound`/`Unavailable`/`Failed`, refuse with the mapped message (exit 5),
  never write.
- Compute the change: pinning `<pkg>` to `<version>` from nixpkgs `<commit>`
  (was: unpinned / previously pinned `<oldversion>@<oldcommit>`).
- Accept/deny (TUI-first, per the standing preference):
  - Interactive TTY without `--yes`: the TUI Install screen surfaces the pin as
    an async resolve row (spinner while resolving; then `pin: 3.2.1 -> nixpkgs
    <shortcommit>` / `pin failed: <reason>`), shown alongside verify and build.
    Apply is gated on a successful resolution when a version was requested.
    Apply writes the KDL `version` prop plus the lock pin, then regenerates
    through the existing draft -> verify -> write path (reverting on failure).
  - `--yes` or non-TTY: plain path prints the resolution and, unless `--yes`,
    a `[y/N]` confirm; on accept writes KDL + lock pin and regenerates.
- Pairs with `--build` (slice B): a pinned package from an old rev that will not
  build is refused at install rather than committed.

The resolver runs once per package (host-independent lookup keyed by
name+version); switching host in the TUI does not re-resolve (like the build).

### 5. check / generate / upgrade

- `generate` / `check`: pure over KDL + lock, no network. Emit pinned imports
  from lock pins; unresolved declared version = Validation.
- `upgrade`: when a pinned package's version is present, `upgrade` may
  re-resolve (network) and show the pin in its notes/diff before applying;
  baseline rev bumps do not disturb pins (each pin carries its own rev).

## Testing

- Lock: parse + serialise round-trip of a `host`/`pin` block; back-compat (a
  lock with no host block parses); deterministic ordering.
- Reconcile: KDL version with a matching lock pin is clean; without a pin is a
  Validation error with the stated message.
- Resolver: `lookup` against a shim via `KNIXL_PIN_RESOLVER` for found /
  not-found / unavailable / malformed-output.
- package module: golden-style emit for an unpinned package (unchanged) and a
  pinned package (inline `(import (fetchGit {...}) { system = pkgs.system;
  }).<name>`), asserting the `fetchGit` url/rev come from the threaded pin;
  determinism (byte-identical twice).
- install: `pkg@version` parsing; resolve-then-refuse on NotFound/Unavailable
  (no write, exit 5) via the CLI harness with a shim; accept writes KDL + lock
  pin and regenerates (plain path with `--yes`).
- TUI Install screen (pure reducers): a `PinResolved`/`PinFailed` message sets
  the pin status; Apply gating includes the pin (requested + resolving blocks,
  failed blocks); a host switch does not re-resolve; stale-token discard.

The async `Cmd`/resolver glue and the default nixhub resolver script stay
untested (as with the verify/build paths).

## Delivery

Implemented as phased tasks (one plan): (1) lock schema + reconcile for per-host
pins, (2) injected `PinResolver`, (3) `package` version emit + lowering-context
threading + goldens, (4) `install pkg@version` resolve + accept/deny + TUI pin
row + wiring, (5) docs.

## Out of scope (per ADR 0005)

Per-host baseline revs; `overrideAttrs`/flake resolution; GC of unreferenced
pins; showing resolver logs in the TUI (only the resolved status is surfaced).
