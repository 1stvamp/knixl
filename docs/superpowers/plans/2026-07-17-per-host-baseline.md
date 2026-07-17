# Per-host baseline nixpkgs rev implementation plan (#22)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a host declare `nixpkgs release="<rel>"`; resolve it to a commit at pin time,
store it per host in the lock, and validate that host (and test its pins) against that rev.

**Architecture:** A `HostBaseline` on the lock (beside `pins`), a `nixpkgs` metadata node the
host module recognises, a `BaselineResolver` (git ls-remote then GitHub API), a per-host
oracle map replacing `generate`'s single oracle, and CLI resolution/GC mirroring the pin flow.
Emit is unchanged.

**Tech Stack:** Rust workspace (knixl-lock, knixl-modules, knixl-nix, knixl-pipeline,
knixl-cli), KDL, ureq, git, nixfmt via `KNIXL_FORMATTER`.

## Global Constraints

- British spelling in prose/comments; no em/en-dashes (colons, parentheses, commas, full stops).
- Deterministic emit: BTreeMap/BTreeSet, stable order, no HashMap in emit paths.
- KDL is authoritative; the `nixpkgs` node is metadata and MUST NOT appear in emitted Nix.
- generate/check stay pure and offline; resolution (release -> rev) happens only at install/upgrade.
- Emit is baseline-independent: a host with `nixpkgs release=..` emits byte-identical Nix to
  the same host without it. The baseline drives validation and pin feasibility only.
- knixl-modules depends only on ir+oracle+kdl. Baseline resolution lives in knixl-nix; the
  per-host oracle map is built in knixl-pipeline (gather) and consumed in generate.
- Back-compat: a lock with no `baseline` line parses to empty `baselines` and renders unchanged;
  a project with no `nixpkgs` nodes behaves exactly as today (global oracle).
- Validation stays best-effort: a host rev with no cached `options.json` is skipped, not failed.
- Do NOT run `cargo fmt` (repo is not rustfmt-normalised); hand-format to match surrounding style.
- Never commit or run git/but in a task; the controller commits. nixfmt is at `~/.nix-profile/bin/nixfmt`.

---

### Task 1: HostBaseline on the lock (knixl-lock)

**Files:**
- Modify: `crates/knixl-lock/src/model.rs` (`HostBaseline`, `Lock.baselines`, host-block parse, render)
- Test: `crates/knixl-lock/src/model.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub struct HostBaseline { pub release: String, pub nixpkgs_rev: String, pub options_hash: Hash }`
  (derive Debug, Clone, PartialEq, Eq); `Lock { .., pub baselines: BTreeMap<String, HostBaseline> }`.

- [ ] **Step 1: Write failing round-trip tests**

Add tests: (a) a `host "web" { baseline release="25.05" nixpkgs-rev="abc" options-hash="blake3:x"\n pin "htop" version="3.2.1" nixpkgs-rev="abc" }` parses to a `HostBaseline` in `baselines["web"]` and the pin in `pins["web"]`, and renders back with the `baseline` line before the `pin` line; (b) a host block with only pins => empty `baselines`, renders unchanged (byte-for-byte back-compat); (c) two `baseline` nodes in one host => parse error; (d) an unknown child (not `pin`/`baseline`) => parse error.

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-lock baseline`
Expected: FAIL (type/field absent).

- [ ] **Step 3: Add the struct, field, parse, render**

Add `HostBaseline` and `baselines: BTreeMap<String, HostBaseline>` to `Lock`. In the host-block
parse loop, `match child.name().value()`: `"pin"` (as today), `"baseline"` (read props
`release`, `nixpkgs-rev`, `options-hash`; error if a second `baseline` for the same host),
anything else => the existing malformed error (update its message to "expected `pin` or
`baseline`"). Insert into `baselines` when present. In render, inside the `host "<name>"` block
emit the `baseline` line first (only if the host has one) then the `pin` lines. Update the
`Lock { .. }` literals in this crate's tests to include `baselines: BTreeMap::new()`.

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p knixl-lock`
Expected: PASS.

- [ ] **Step 5: Fix other `Lock { .. }` construction sites so the workspace compiles**

Search the workspace for `Lock {` literals (pipeline tests, cli) and add `baselines: BTreeMap::new()`.
Run `cargo build --workspace --tests`; fix every E0063. Do not change behaviour, only add the field.

- [ ] **Step 6: No commit.**

---

### Task 2: The `nixpkgs` metadata node (knixl-modules + knixl-pipeline)

**Files:**
- Modify: `crates/knixl-modules/src/builtin/host.rs` (recognise `nixpkgs` child, emit nothing)
- Modify: `crates/knixl-pipeline/src/gather.rs` (scan hosts for declared `nixpkgs release`)
- Test: host.rs `#[cfg(test)]` (no warning) and a gather test (release read)

**Interfaces:**
- Produces: `pub fn declared_baselines(hosts: &[HostSource]) -> BTreeMap<String, String>` in
  `knixl-pipeline` (host name -> declared release; only hosts with a `nixpkgs release` node).

- [ ] **Step 1: Failing test — `nixpkgs` is not an unknown-child warning**

In host.rs tests, lower a host with `system "x86_64-linux"` and `nixpkgs release="25.05"` and
assert the generated file has NO warning mentioning `nixpkgs`, and the emitted text does not
contain `nixpkgs release` / the release string (metadata is not emitted).

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-modules host`
Expected: FAIL (nixpkgs currently lints as an unknown child).

- [ ] **Step 3: Recognise `nixpkgs` in the host module**

Add `nixpkgs` to the children the host module handles directly (the `&["system"]` list in
`lower_children`, and the schema `children`/allowed set as needed) so it is claimed and NOT
dispatched or linted. It contributes no `Unit` (metadata only). Do not emit anything for it.

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p knixl-modules`
Expected: PASS.

- [ ] **Step 5: Scan for declared releases in gather**

In `crates/knixl-pipeline/src/gather.rs`, add `declared_baselines(hosts)` mirroring the
`referenced_pins` scan (#24): parse each host's KDL, for a `host` node read its name
(`first_arg_str`) and, if it has a `nixpkgs` child with a `release` prop, record
`host -> release`. Return the map. (Consumed by Task 4 for oracle selection and Task 5 for
resolution/validation.)

- [ ] **Step 6: gather test**

Add a test that `declared_baselines` returns `{"web": "25.05"}` for a host declaring it and
nothing for a host without. Run `cargo test -p knixl-pipeline`. Expected: PASS.

- [ ] **Step 7: No commit.**

---

### Task 3: BaselineResolver (knixl-nix)

**Files:**
- Create: `crates/knixl-nix/src/baseline.rs` (resolver + pure parse fns)
- Modify: `crates/knixl-nix/src/lib.rs` (`pub mod baseline;`)
- Test: `baseline.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub enum BaselineResolver { External(PathBuf), Builtin }`; `BaselineResolver::resolve()`
  reads `KNIXL_BASELINE_RESOLVER`; `fn lookup(&self, release: &str) -> Result<String, BaselineError>`
  (returns the commit); `pub fn rev_from_ls_remote(out: &str) -> Option<String>`;
  `pub fn rev_from_github_json(json: &str) -> Option<String>`. Reuse `PinError`-style variants
  or a local `BaselineError { NotFound, Unavailable, Failed }`.

- [ ] **Step 1: Write failing pure-parse tests**

`rev_from_ls_remote("<40hex>\trefs/heads/nixos-25.05\n")` => `Some("<40hex>")`; empty/garbage =>
`None`. `rev_from_github_json(sample_with_sha)` => the `sha`; missing/`bad json` => `None`.
Include a committed sample of each response shape.

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-nix baseline`
Expected: FAIL (module absent).

- [ ] **Step 3: Implement**

Mirror `crates/knixl-nix/src/pin.rs`. `lookup` for `Builtin`: run
`git ls-remote https://github.com/NixOS/nixpkgs refs/heads/nixos-<release>` (via the same
command-output helper `pin.rs` uses); on success parse with `rev_from_ls_remote`. If git is not
found or exits non-zero, fall back to `ureq::get("https://api.github.com/repos/NixOS/nixpkgs/commits/nixos-<release>")`
(a `User-Agent` header is required by GitHub; set one), parse with `rev_from_github_json`. Map
errors: transport/non-200-non-404 => `Unavailable`; 404 / no ref => `NotFound`; unparseable =>
`Failed`. `External` uses the `<cmd> <release>` single-token protocol like the pin resolver.

- [ ] **Step 4: Run, watch pass**

Run: `cargo test -p knixl-nix`
Expected: PASS (pure-parse tests; the git/ureq fetches are untested glue).

- [ ] **Step 5: No commit.**

---

### Task 4: Per-host oracle map + generate signature (knixl-pipeline + call sites)

**Files:**
- Modify: `crates/knixl-pipeline/src/lib.rs` (`generate` signature; `generate_one` per-host oracle)
- Modify: `crates/knixl-pipeline/src/gather.rs` (build the per-host oracle map)
- Modify call sites: `crates/knixl-cli/src/main.rs`, `crates/knixl-pipeline/tests/golden.rs`
- Test: pipeline test for per-host selection

**Interfaces:**
- Consumes: Task 1 `Lock.baselines`, Task 2 `declared_baselines`.
- Produces: `generate(hosts, registry, formatter, tool, oracles: &BTreeMap<String, knixl_oracle::Oracle>, pins)`
  (the `oracle: Option<&Oracle>` param becomes `oracles`); `Project` gains
  `oracles: BTreeMap<String, Oracle>`.

- [ ] **Step 1: Write the failing per-host selection test**

A pipeline test with two hosts: host A has a lock `baseline` rev whose `options.json` is
available (seed a temp cache dir or use `KNIXL_OPTIONS_JSON` with a fixture asserting a
specific option is known/unknown), host B has none. Assert A is validated against A's option
set and B against the global oracle (or skipped when neither is cached). Assert the map has an
entry per host with a resolvable oracle.

- [ ] **Step 2: Run, watch fail**

Run: `cargo test -p knixl-pipeline per_host`
Expected: FAIL (single-oracle signature).

- [ ] **Step 3: Build the per-host oracle map in gather**

In `gather`, replace the single `Oracle` build with a `BTreeMap<String, Oracle>`: for each
host in the project, choose its rev = `lock.baselines.get(host).map(|b| &b.nixpkgs_rev)` else
`&lock.oracle.nixpkgs_rev`; insert `Oracle::from_rev_cache(rev)` when it loads. Keep
`KNIXL_OPTIONS_JSON` as an override that maps every host to that one oracle (testing seam).
Store on `Project` as `oracles`.

- [ ] **Step 4: Change generate + generate_one**

Change `generate`'s `oracle: Option<&Oracle>` param to `oracles: &BTreeMap<String, Oracle>`.
In `generate_one`, select `oracles.get(&host_name)` and validate against it when `Some`, skip
when `None` (same best-effort behaviour, now per host).

- [ ] **Step 5: Update all call sites**

Update every `generate(...)` and gather-oracle caller: CLI (`check`, `generate`, `install`
preview), and `golden.rs` (which passes `None` today) to pass `&BTreeMap::new()` (empty => skip,
unchanged behaviour). `cargo build --workspace --tests` must be clean.

- [ ] **Step 6: Run**

Run: `cargo test -p knixl-pipeline -p knixl-cli` and `KNIXL_FORMATTER=/home/wes/.nix-profile/bin/nixfmt cargo test -p knixl-pipeline --test golden`
Expected: PASS (goldens unchanged: emit is baseline-independent).

- [ ] **Step 7: No commit.**

---

### Task 5: CLI resolution, write, validation error, GC (knixl-cli + reconcile)

**Files:**
- Modify: `crates/knixl-cli/src/main.rs` (resolve+write baseline; per-host baseline for choose_strategy)
- Modify: `crates/knixl-lock/src/reconcile.rs` + `crates/knixl-pipeline/src/gather.rs` (baseline GC + validation error)
- Test: `crates/knixl-cli/tests/cli.rs`, reconcile/gather tests

**Interfaces:**
- Consumes: Task 1 `HostBaseline`, Task 2 `declared_baselines`, Task 3 `BaselineResolver`.

- [ ] **Step 1: Validation error for a declared-but-unresolved release**

Where generate/check gather inputs, compare each host's `declared_baselines` release against the
lock `baselines`: a declared release with no lock baseline, or a lock baseline whose `release`
differs, is a validation error string (`"host \"web\": nixpkgs release \"25.05\" is not resolved: run knixl upgrade"`).
Thread it into `Inputs.validation_errors` (same channel unresolved pins use). Write the failing
test first (a project with a `nixpkgs release` and no lock baseline => `plan.has_validation_errors()`).

- [ ] **Step 2: Resolve + write baseline at install/upgrade**

Add a helper `write_baseline(host, release, rev, options_hash)` mirroring `write_pin`. In the
`install`/`upgrade` path, when a host declares a `nixpkgs release` not matching the lock, call
`BaselineResolver::resolve().lookup(release)`, then `write_baseline` (record the options-hash of
the rev's cached options.json if present, else empty). On resolver error, refuse (exit 5) with a
clear message. Print one line naming the release and resolved rev.

- [ ] **Step 3: choose_strategy uses the host baseline**

Change the `baseline_rev` passed to `choose_strategy`/`select_strategy` (#23) from
`ctx.lock.oracle.nixpkgs_rev` to the host's lock `baseline.nixpkgs_rev` when present, else the
global oracle rev. (Both the plain and TUI paths.)

- [ ] **Step 4: Baseline GC**

Extend the referenced-set the pipeline hands `build_lock_next` (#24) so `baselines` are pruned
too: a host absent from the KDL, or a host that no longer declares a `nixpkgs` node, drops its
lock baseline. Mirror `prune_pins` with a `prune_baselines`. Write a failing test (a lock
baseline for a host that dropped its `nixpkgs` node is gone from `lock_next`).

- [ ] **Step 5: CLI tests**

Using the shim harness, add: (a) `install`/`upgrade` with a host declaring `nixpkgs release=..`
and a shimmed baseline resolver writes the expected `baseline` line to the lock; (b) a declared
release with no lock baseline makes `check` report a validation error (exit 5).

- [ ] **Step 6: Run**

Run: `cargo test -p knixl-cli -p knixl-lock -p knixl-pipeline` and `cargo clippy --all-targets -- -D warnings`
Expected: PASS, clean.

- [ ] **Step 7: No commit.**

---

### Task 6: Golden for a host declaring a baseline

**Files:**
- Modify: `examples/hosts/*.kdl` (add `nixpkgs release=".."` to an existing example host) OR create `examples/hosts/baselined.kdl`
- Modify: `examples/knixl.lock.kdl` (that host's `baseline` line)
- Modify: `crates/knixl-pipeline/tests/golden.rs`

- [ ] **Step 1: Fixture**

Add `nixpkgs release="25.05"` to an existing example host (prefer editing one, e.g. `web.kdl`, to
prove emit is unchanged), or a new `baselined.kdl`. Add the matching `baseline` line to
`examples/knixl.lock.kdl` (fixed rev `0000000000000000000000000000000000000abc`, options-hash empty).

- [ ] **Step 2: Assert emit is unchanged**

If you edited an existing host, its `expected/*.nix` must be byte-identical to before (the golden
already covers it): run `KNIXL_FORMATTER=/home/wes/.nix-profile/bin/nixfmt cargo test -p knixl-pipeline --test golden` and confirm that host's golden still passes with NO change to the expected file. If you added a new host, produce its `expected/*.nix` (dump-test then delete) and confirm it contains no `nixpkgs`/release string.

- [ ] **Step 3: Lock round-trip stays green**

Run: `cargo test -p knixl-pipeline --test golden lock_round_trips`
Expected: PASS (the new `baseline` line round-trips).

- [ ] **Step 4: No commit.**

---

## Self-Review

- Spec coverage: T1 lock schema; T2 KDL metadata node + scan; T3 resolver; T4 per-host oracle +
  generate signature; T5 CLI resolve/write/validation-error/GC + choose_strategy baseline; T6
  golden proving emit is baseline-independent.
- Placeholders: the one tool-produced artifact is a new host's `expected/*.nix` if added (T6
  Step 2 gives the command); KDL accessor and schema APIs in T2 are "match existing host module /
  referenced_pins", to be mirrored not invented.
- Type consistency: `HostBaseline`/`Lock.baselines` (T1) consumed by gather (T4), CLI (T5);
  `declared_baselines` (T2) consumed by T4/T5; `BaselineResolver`/`lookup` (T3) consumed by T5;
  `generate(.., oracles, ..)` (T4) matches all updated call sites.
