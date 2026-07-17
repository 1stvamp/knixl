# Per-host baseline nixpkgs rev design (#22)

Date: 2026-07-17
Status: approved, ready for implementation plan
Issue: #22
Builds on: ADR 0003, ADR 0005, ADR 0006, ADR 0007

Let each host declare its own baseline nixpkgs release; validate that host against that
release's option set and use that rev as the host's pin baseline. Mirrors the package-pin
model: KDL declares intent, resolution happens at pin time, the lock stores the resolved
rev, generate/check stay offline. Emit is unchanged (validation + pin baseline only).

## Grounding (current state)

- One global `lock.oracle: OraclePin { nixpkgs_rev, options_hash }` (`knixl-lock/src/model.rs`).
- `gather` builds a single `Oracle` from `lock.oracle.nixpkgs_rev` (best-effort:
  `Oracle::from_rev_cache(rev)`, or `KNIXL_OPTIONS_JSON`, else `None` => validation skipped).
- `generate(hosts, .., oracle: Option<&Oracle>, pins)` validates every host against that one
  oracle. `Oracle::cache_path(rev)` already keys the options cache by rev.
- The `host` block in the lock accepts only `pin` children (`model.rs`, host-block parse).
- `host` module has `open_children: true`; an unknown child (e.g. `nixpkgs`) currently lints
  as a warning. `lower_children(node, &["system"])` lists the children host handles directly.
- Several call sites read `lock.oracle.nixpkgs_rev` as "the baseline", including #23's
  `choose_strategy` (`main.rs`).

## KDL surface

An optional `nixpkgs` node on a host declares its baseline release:

```kdl
host "web" {
    system "x86_64-linux"
    nixpkgs release="25.05"
    web-service "example.com" { ... }
}
```

`release` is a NixOS release like `25.05` (the `nixos-25.05` branch). A host without a
`nixpkgs` node uses the global `oracle` rev (back-compat). The node is metadata: it is
recognised by the `host` module (added to the directly-handled children, so no
unknown-child warning) and lowers to nothing (it never appears in the emitted Nix).

## Lock schema

Add a per-host baseline record:

```rust
pub struct HostBaseline { pub release: String, pub nixpkgs_rev: String, pub options_hash: Hash }
// on Lock:
pub baselines: BTreeMap<String, HostBaseline>,   // keyed by host name
```

Render inside the existing `host "<name>"` block, before its `pin` lines:

```kdl
    host "web" {
        baseline release="25.05" nixpkgs-rev="<commit>" options-hash="<hash>"
        pin "htop" version="3.2.1" nixpkgs-rev="<commit>"
    }
```

Parse: the host-block loop accepts `baseline` (at most one per host) and `pin`; any other
child is malformed. `options-hash` may be empty (best-effort, like the global oracle). The
global `oracle` line is unchanged and remains the default baseline for undeclared hosts. A
lock with no `baseline` lines parses to empty `baselines` (back-compat).

## Resolution (pin time only)

A new `BaselineResolver` in `knixl-nix` (mirror `PinResolver`, ADR 0005 / #21):
`External(PathBuf)` when `KNIXL_BASELINE_RESOLVER` is set, else `Builtin`.

- `External`: `<cmd> <release>` prints the resolved commit (single token), same protocol
  shape as the pin resolver.
- `Builtin`: run `git ls-remote https://github.com/NixOS/nixpkgs refs/heads/nixos-<release>`
  and take the SHA of the first line; if git is absent or errors, fall back to the GitHub
  commits API over `ureq` (`GET https://api.github.com/repos/NixOS/nixpkgs/commits/nixos-<release>`,
  read the top-level `sha`). Errors map like the pin resolver: transport/unknown status =>
  `Unavailable`, 404/missing branch => `NotFound`, unparseable => `Failed`.

Resolution runs only at `install`/`upgrade`. The resolved rev (and the options-hash of that
rev's `options.json` when it is available in the cache, else empty) is written to the lock's
`baselines`. `generate`/`check` never resolve.

A host whose KDL declares a `nixpkgs release` with no matching lock `baseline` (or a
release that differs from the locked one) is a validation error naming the host and telling
the user to run `upgrade`, exactly as an unresolved package pin is (ADR 0005). Offline or
without a resolver, a baseline cannot be created (a clear error), never a wrong rev.

## Validation wiring (per host)

- `gather` builds a per-host oracle map instead of one oracle: for each host, if the lock has
  a `baseline` for it, `Oracle::from_rev_cache(baseline.nixpkgs_rev)`, else
  `Oracle::from_rev_cache(oracle.nixpkgs_rev)` (the global default). Best-effort as today: a
  missing cache => that host's validation is skipped, not failed. `KNIXL_OPTIONS_JSON`, if
  set, still overrides for all hosts (a testing seam).
- `generate` takes the per-host oracles (e.g. `oracles: &BTreeMap<String, Oracle>` plus an
  optional global default) instead of a single `Option<&Oracle>`; `generate_one` selects by
  host name. This is the one signature change; update all call sites (CLI, golden tests).
- The `baseline_rev` that `choose_strategy` (#23) passes to `select_strategy` becomes the
  host's baseline rev (from the lock) instead of the global `oracle` rev, retiring ADR 0006's
  "builds against the builder channel" note for hosts that declare a release.

## Reconcile

- Emit is baseline-independent, so a baseline change causes no output-file drift or skew:
  `Plan::compute` and the `FileState` triple are unchanged.
- `build_lock_next` carries `baselines` forward like `pins`, and prunes a host's baseline
  when the host no longer declares a `nixpkgs` node (mirror #24's pin GC: the pipeline's
  referenced set gains, per host, whether a `nixpkgs release` node is present; an absent host
  or a host without the node drops its baseline).

## CLI

- `upgrade` (or `install` when a host declares a new/changed release) resolves the release
  and writes the baseline to the lock, mirroring how `install` writes pins. Print one line
  naming the resolved release and rev.
- The validation-error path (a declared release with no lock baseline, or one that differs
  from the locked release) is surfaced by `check`/`generate`/`plan` as a validation error
  (exit 5), consistent with unresolved pins.

## Testability

- **Lock**: round-trip tests for a `baseline` line (present / absent / with pins / bad
  child), and back-compat (no baseline line => empty `baselines`).
- **Resolver**: the git and ureq fetches are untested glue (shim pattern like the pin
  resolver); the response-to-rev parse steps are pure functions, unit-tested against
  committed `git ls-remote` output and a GitHub API sample.
- **Per-host oracle selection**: a gather/pipeline test with two hosts, one declaring a
  baseline (with a cached options.json fixture via `KNIXL_OPTIONS_JSON` or a temp cache) and
  one not, asserting each is validated against the right option set.
- **Baseline GC**: a lock baseline for a host that dropped its `nixpkgs` node is pruned on
  generate (mirror #24's test).
- **Validation error**: a host declaring a release with no lock baseline yields a validation
  error naming the host and `upgrade`.
- **Golden**: an example host with `nixpkgs release=".."`; since emit is unchanged, its
  golden `.nix` is identical to the same host without the node (asserting emit really is
  baseline-independent), and the example lock carries its `baseline` line.

## Out of scope

Driving unpinned package emit from the baseline rev (ADR 0007); a raw `nixpkgs rev="..."`
escape hatch; automatically building `options.json` for a rev (validation stays best-effort
on the cache); per-host formatter or tool versions.
