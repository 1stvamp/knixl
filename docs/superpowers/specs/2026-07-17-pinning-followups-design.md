# Pinning follow-ups design (#25, #24)

Date: 2026-07-17
Status: approved, ready for implementation plan
Issues: #25 (golden fixture for pinned emit), #24 (GC unreferenced pins)
Builds on: docs/adr/0005-package-version-pinning.md

Two small, additive follow-ups that harden the version-pinning work shipped in #15.
Per-host baseline revs (#22) and cross-rev resolution (#23) are handled separately.

## Grounding (current state)

- Pins live in `lock.pins: BTreeMap<host, Vec<Pin>>` (`crates/knixl-lock/src/model.rs`),
  each `Pin { package, version, nixpkgs_rev }`, keyed by host name, the vec sorted by
  package. Written only by the CLI `write_pin`/`remove_pin`, by package name.
- Emit (`crates/knixl-modules/src/builtin/package.rs`): a pinned package emits
  `(import (builtins.fetchGit { rev = <commit>; shallow = true; url = ...; })
  { system = pkgs.system; }).<name>`; an unpinned package emits bare `pkgs.<name>`.
- The golden harness (`crates/knixl-pipeline/tests/golden.rs`) generates the three
  example hosts with an empty pins map and `None` oracle, so the pinned emit path has
  no golden coverage. No example uses a versioned `package` node.
- `build_lock_next` (`crates/knixl-lock/src/reconcile.rs`) carries pins forward verbatim
  (`pins: lock.pins.clone()`); nothing correlates KDL `package` nodes with lock pins.

## #25: golden fixture for the pinned emit path

CLAUDE.md treats `examples/expected/*.nix` as the acceptance + determinism harness, so
the pinned path belongs there, not in a private test fixture.

### Fixture

New `examples/hosts/pinned.kdl`, a host exercising both emit arms:

```kdl
host "pinned" {
    system "x86_64-linux"
    package "htop" version="3.2.1"
    package "ripgrep"
}
```

`examples/knixl.lock.kdl` gains a matching pin block (the resolved commit is a fixed
40-char hex string, committed verbatim so generation stays offline and deterministic):

```kdl
host "pinned" {
    pin "htop" version="3.2.1" nixpkgs-rev="<fixed 40-char hex>"
}
```

Only the `pin` block is added, not an `input`/`output` entry for the pinned host. The new
golden reads `lock.pins` (the rev) and nothing regenerates the full-examples lock against
this file (`lock_round_trips` only round-trips the model; `gather_and_plan` uses a web+db
subset), so `input`/`output` entries would be hand-computed hashes with no verifier. If a
full-examples lock golden is added later, add them then.

New `examples/expected/pinned.nix`: the real nixfmt-formatted output for that host,
produced by the generator (nixfmt is available via `KNIXL_FORMATTER`), showing `htop`
via the pinned `fetchGit` import and `ripgrep` via ambient `pkgs`.

### Harness threading

The golden path currently passes an empty pins map. Add a golden test that threads the
pinned host's pins into `generate`. The pins come from parsing `examples/knixl.lock.kdl`
(`Lock::parse`), so the fixture lock is the single source of the rev: the test reads the
lock, pulls `lock.pins`, and calls `generate(..., &lock.pins)` for the pinned host, then
asserts the produced file equals `examples/expected/pinned.nix` byte-for-byte. Gated on
`formatter_available()`, like the other goldens.

### Ripples

- `gather_and_plan_report_missing_when_disk_is_empty` is **unaffected**: its `temp_project`
  copies only `web.kdl` + `db.kdl`, so the new host is never gathered there.
- A determinism assertion covers the pinned host too, so the pinned emit path is checked
  by the twice-generate byte-identical guarantee.

## #24: GC unreferenced pins from the lock

### Rule

A pin `(host H, package P)` is *referenced* iff host `H` exists in the current KDL **and**
has a versioned `package "P"` node (any version). Every other pin is unreferenced and is
pruned: package node removed, package left un-versioned, or the whole host gone.

Version mismatch (KDL declares `version="2.0"`, lock pins `1.0`) is **not** GC's concern:
that case is already a generate-time validation error telling the user to run
`install`/`upgrade`. GC only removes pins with no versioned node for that package at all.

### Where it hooks

The pipeline already parses each host and has `host_name`, the KDL nodes, and
`pins.get(&host_name)` in scope during `generate`. It computes the pruned pins map
(the referenced subset of `lock.pins`) and passes it into the lock-write path.
`build_lock_next` currently hard-codes `pins: lock.pins.clone()`; it takes the pruned map
instead, so `reconcile` stays pure (the KDL-to-pin correlation lives in the pipeline, the
storage decision in reconcile).

### Behaviour

- Automatic on `generate` (the only path that writes the lock). `check` stays read-only
  and never prunes.
- No separate command. The ADR mentioned "or an explicit command"; `generate` already
  reconciles the lock, so a dedicated `gc`/`--prune` is unnecessary (YAGNI).
- A host removed from the KDL drops all of its pins.

## Testing

- #25: the new byte-for-byte golden (gated on the real formatter), plus the updated
  count and determinism tests.
- #24: unit tests on the prune step: (a) a pin whose `package` node was removed is
  dropped; (b) a pin whose package is now un-versioned is dropped; (c) a whole removed
  host drops its pins; (d) a still-referenced pin survives untouched.

## Out of scope

Per-host baseline revs (#22), cross-rev resolution (#23), any change to the resolver or
the pinned emit shape itself.
