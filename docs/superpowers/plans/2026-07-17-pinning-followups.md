# Pinning follow-ups implementation plan (#25, #24)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Put the pinned-package emit path under the byte-for-byte golden suite (#25), and
prune unreferenced pins from the lock on generate (#24).

**Architecture:** #25 adds an `examples/` fixture host that declares a pinned + an unpinned
package, its expected nixfmt output, and a golden test that threads the lock's pins into
`generate`. #24 adds a `referenced_pins` set to `Inputs`, populated by the pipeline from the
parsed host KDL, and has `build_lock_next` retain only pins that still have a versioned
`package` node.

**Tech Stack:** Rust workspace (knixl-lock, knixl-pipeline, knixl-modules), nixfmt via
`KNIXL_FORMATTER`, KDL, blake3 hashes.

## Global Constraints

- British spelling in prose/comments; no em/en-dashes (colons, parentheses, commas, full stops).
- Deterministic emit: no `HashMap` in emit paths; `BTreeMap`/`BTreeSet` and stable order.
- KDL is authoritative; generated Nix is derived. No Nix-to-KDL round-trip.
- `Plan::compute` stays pure (no I/O). The KDL-to-pin correlation lives in the pipeline layer;
  reconcile only stores the result.
- knixl-modules depends only on knixl-ir + knixl-oracle (not knixl-lock). Do not add crate deps.
- The byte-for-byte goldens require a real formatter and are gated on `formatter_available()`;
  they skip (not fail) without `KNIXL_FORMATTER`. nixfmt is at `~/.nix-profile/bin/nixfmt`.
- Fixed pin rev in the fixture is a committed literal; generation stays offline.

---

### Task 1: Golden fixture for the pinned emit path (#25)

**Files:**
- Create: `examples/hosts/pinned.kdl`
- Create: `examples/expected/pinned.nix` (produced by the generator, then committed)
- Modify: `examples/knixl.lock.kdl` (add the `host "pinned"` pin block)
- Test: `crates/knixl-pipeline/tests/golden.rs` (new golden + determinism coverage)

**Interfaces:**
- Consumes: `generate(hosts, registry, formatter, tool, oracle, pins)` where
  `pins: &BTreeMap<String, Vec<knixl_lock::model::Pin>>`; `Lock::parse(&str) -> Result<Lock, _>`
  with `lock.pins: BTreeMap<String, Vec<Pin>>`; the golden helpers `examples_dir()`,
  `build_registry()`, `formatter()`, `formatter_available()`, `HostSource { path, src }`.
- Produces: nothing consumed by later tasks.

- [ ] **Step 1: Write the fixture host**

Create `examples/hosts/pinned.kdl` exercising both emit arms (one pinned, one ambient):

```kdl
host "pinned" {
    system "x86_64-linux"
    package "htop" version="3.2.1"
    package "ripgrep"
}
```

- [ ] **Step 2: Add the pin to the example lock**

In `examples/knixl.lock.kdl`, add a pin block for the host (place it with the other host
blocks, or after `oracle` if none exist). Use a fixed, obviously-synthetic 40-char hex rev:

```kdl
    host "pinned" {
        pin "htop" version="3.2.1" nixpkgs-rev="0000000000000000000000000000000000000abc"
    }
```

Do NOT add `input`/`output` entries for the pinned host (see the spec: nothing regenerates
the full-examples lock against this file).

- [ ] **Step 3: Write the failing golden test**

Add to `crates/knixl-pipeline/tests/golden.rs`:

```rust
#[test]
fn pinned_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping pinned_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    let examples = examples_dir();
    let path = std::path::PathBuf::from("hosts/pinned.kdl");
    let src = fs::read_to_string(examples.join(&path)).expect("read pinned host kdl");

    // The rev comes from the committed example lock: the pin path stays offline.
    let lock_src = fs::read_to_string(examples.join("knixl.lock.kdl")).expect("read lock");
    let pins = Lock::parse(&lock_src).expect("parse lock").pins;

    let tool = "0.3.1".parse().unwrap();
    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &formatter(),
        &tool,
        None,
        &pins,
    )
    .expect("generate");

    assert_eq!(files.len(), 1, "pinned host has no side-files");
    let expected = fs::read_to_string(examples.join("expected/pinned.nix"))
        .expect("no expected output at examples/expected/pinned.nix");
    assert_eq!(files[0].text, expected, "pinned.nix does not match golden");
}
```

- [ ] **Step 4: Run it, watch it fail**

Run: `KNIXL_FORMATTER=nixfmt cargo test -p knixl-pipeline --test golden pinned_matches_golden -- --nocapture`
Expected: FAIL at reading `examples/expected/pinned.nix` (the golden does not exist yet).

- [ ] **Step 5: Produce the golden output**

Generate the expected bytes through the same code path and save them. Quickest: a throwaway
that mirrors the test but writes the output, e.g. add a temporary `#[test] fn dump()` that
runs the same `generate(...)` and does
`fs::write(examples.join("expected/pinned.nix"), &files[0].text).unwrap();`, run it once with
`KNIXL_FORMATTER=nixfmt`, then delete the dump test. (Or run the real CLI `knixl generate` in
a temp project containing `hosts/pinned.kdl` + the lock, and copy `generated/pinned.nix`.)

Read the produced `examples/expected/pinned.nix` and sanity-check it: it must contain
`htop` via `import (builtins.fetchGit { ... rev = "0000...abc"; ... }) { system = pkgs.system; }`
and `ripgrep` via ambient `pkgs.ripgrep`, with the knixl header comment and nixfmt spacing.

- [ ] **Step 6: Run the golden, watch it pass**

Run: `KNIXL_FORMATTER=nixfmt cargo test -p knixl-pipeline --test golden pinned_matches_golden`
Expected: PASS.

- [ ] **Step 7: Cover the pinned path in the determinism check**

Add a determinism assertion for the pinned host (either extend
`generate_is_byte_identical_across_runs` to also run the pinned host, or add a sibling test)
that generates `hosts/pinned.kdl` twice with the lock's pins and asserts byte-identical output.

Run: `KNIXL_FORMATTER=nixfmt cargo test -p knixl-pipeline --test golden`
Expected: all golden tests PASS (or skip cleanly without the formatter).

- [ ] **Step 8: Confirm nothing else regressed**

Run: `cargo test -p knixl-pipeline` and `cargo test -p knixl-lock`
Expected: green (the golden tests skip without `KNIXL_FORMATTER`, everything else passes).

- [ ] **Step 9: Commit**

Commit `examples/hosts/pinned.kdl`, `examples/expected/pinned.nix`,
`examples/knixl.lock.kdl`, and the test changes with a message like
`test(pipeline): golden fixture for the pinned package emit path (#25)`.

---

### Task 2: GC unreferenced pins from the lock on generate (#24)

**Files:**
- Modify: `crates/knixl-lock/src/reconcile.rs` (add `Inputs.referenced_pins`, prune in `build_lock_next`)
- Modify: `crates/knixl-pipeline/src/gather.rs` (populate `referenced_pins` from parsed hosts)
- Test: `crates/knixl-lock/src/reconcile.rs` `#[cfg(test)]` (prune unit tests)

**Interfaces:**
- Consumes: `Inputs`, `Lock { pins: BTreeMap<String, Vec<Pin>> }`, `Pin { package, version, nixpkgs_rev }`,
  `build_lock_next(inputs, lock, running) -> Lock`.
- Produces: `Inputs.referenced_pins: BTreeMap<String, BTreeSet<String>>` (host name -> set of
  package names that have a versioned `package` node), consumed only within reconcile.

- [ ] **Step 1: Write the failing prune test**

In the `#[cfg(test)] mod tests` of `crates/knixl-lock/src/reconcile.rs`, add a test that
builds a `Lock` with pins for two hosts and an `Inputs` whose `referenced_pins` references
only some of them, then asserts `build_lock_next(...).pins` keeps referenced pins and drops:
(a) a pin whose package is absent from its host's referenced set, (b) all pins of a host
absent from `referenced_pins` entirely, while (c) a referenced pin survives.

```rust
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
    let lock = Lock { pins, ..base_lock() };

    let mut referenced = BTreeMap::new();
    referenced.insert("web".to_string(), BTreeSet::from(["htop".to_string()]));
    // "db" absent entirely -> all its pins pruned.
    let inputs = Inputs {
        expected: vec![],
        input_hashes: BTreeMap::new(),
        validation_errors: vec![],
        referenced_pins: referenced,
    };

    let next = build_lock_next(&inputs, &lock, &running_versions());
    assert_eq!(next.pins.get("web").map(Vec::len), Some(1));
    assert_eq!(next.pins["web"][0].package, "htop");
    assert!(next.pins.get("db").is_none(), "removed host drops its pins");
}
```

Reuse or add small `base_lock()` / `running_versions()` helpers matching the existing tests in
this module (mirror whatever the current tests construct).

- [ ] **Step 2: Add the field and run to see it fail to compile**

Add `pub referenced_pins: BTreeMap<String, BTreeSet<String>>,` to `Inputs`
(`crates/knixl-lock/src/reconcile.rs`, the struct at lines ~65-69). `BTreeSet` is already
imported at the top of the file.

Run: `cargo test -p knixl-lock build_lock_next_prunes_unreferenced_pins`
Expected: FAIL — every other `Inputs { .. }` constructor now misses the field (compile error),
and the prune is not implemented yet.

- [ ] **Step 3: Prune in `build_lock_next`**

Replace `pins: lock.pins.clone(),` in `build_lock_next` with a pruned map:

```rust
        pins: prune_pins(&lock.pins, &inputs.referenced_pins),
```

and add the helper below `build_lock_next`:

```rust
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
```

Fix the other `Inputs { .. }` literals in this module's tests to include
`referenced_pins: BTreeMap::new()` (an empty map prunes all pins, which matches the old
behaviour only for locks with no pins; existing pin-free tests are unaffected).

- [ ] **Step 4: Run the unit tests, watch them pass**

Run: `cargo test -p knixl-lock`
Expected: PASS (the new prune test and all existing reconcile tests).

- [ ] **Step 5: Populate `referenced_pins` in gather**

In `crates/knixl-pipeline/src/gather.rs`, where `Inputs` is constructed, populate
`referenced_pins` by scanning each gathered host's KDL for `package` nodes that carry a
`version` prop, keyed by the host's name (the `host "<name>"` positional arg). Add an entry
for EVERY host present (even with an empty set) so a host that exists but dropped a package
still prunes that pin, while a host absent from the KDL drops all its pins.

Use the already-parsed host documents if available; otherwise parse each `HostSource.src`
with `knixl_kdl::parse`. Sketch:

```rust
let mut referenced_pins: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
for host in &hosts {                       // the gathered host sources
    let doc = knixl_kdl::parse(&host.src)?;// or reuse an already-parsed doc
    for node in doc.nodes() {
        if node.name().value() != "host" { continue; }
        let name = /* positional arg 0 as String */;
        let set = referenced_pins.entry(name).or_default();
        for child in node.iter_children() {
            if child.name().value() == "package" && child.get("version").is_some() {
                if let Some(pkg) = /* positional arg 0 as String */ {
                    set.insert(pkg);
                }
            }
        }
    }
}
```

Match the exact KDL accessor API already used elsewhere in gather/lowering for reading a
node's positional string arg and a prop (mirror how the package module reads `version`). Set
`referenced_pins` on the `Inputs` returned by gather.

- [ ] **Step 6: Add a gather-level test**

Add a test (in `gather.rs` tests, or `golden.rs`) that gathers a temp project whose lock has
a pin for a package no longer declared in the host KDL, computes the plan, and asserts
`plan.lock_next.pins` no longer contains that pin, while a still-declared pinned package
survives. Reuse the `temp_project`/`gather` pattern from `golden.rs`.

Run: `cargo test -p knixl-pipeline`
Expected: PASS.

- [ ] **Step 7: Full workspace check**

Run: `cargo test` then `cargo clippy --all-targets -- -D warnings` then `cargo fmt --all --check`
Expected: green.

- [ ] **Step 8: Commit**

Commit `reconcile.rs` and `gather.rs` with a message like
`feat(pipeline): prune unreferenced package pins from the lock on generate (#24)`.

---

## Self-Review

- Spec coverage: Task 1 = #25 (fixture host, expected .nix, lock pin, threaded golden,
  determinism). Task 2 = #24 (referenced set, prune in build_lock_next, gather population,
  automatic on generate, tests for removed-package / removed-host / survivor).
- No placeholders: the one deliberately tool-produced artifact is `examples/expected/pinned.nix`
  (Step 1.5 gives the exact command); the gather KDL accessors are "match existing API" because
  the precise accessor names must mirror current code, not be invented.
- Type consistency: `referenced_pins: BTreeMap<String, BTreeSet<String>>` is defined in Task 2
  Step 2 and consumed in Step 3 and Step 5 under the same name; `prune_pins` signature matches
  its call site.
