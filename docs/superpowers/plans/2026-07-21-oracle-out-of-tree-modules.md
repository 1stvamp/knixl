# Oracle out-of-tree module option sets implementation plan (#35)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the oracle option set from `nixpkgs` plus a declared, pinned set of out-of-tree
modules, so emitting `disko.*` / `sops.*` is validated. A `knixl.kdl` project file declares the
default baseline release and `oracle-modules` (per-host replace-override); knixl builds the
augmented `options.json` via `nixosOptionsDoc` (also automating the base build).

**Architecture:** Five layers, each independently shippable: (1) `knixl.kdl` parsing +
effective-set computation; (2) lock schema for module pins; (3) a module-source resolver +
install/upgrade pinning; (4) the `nixosOptionsDoc` build expression + `NixEval` + effective-set
caching; (5) gather/plan wiring + ADR 0008 + docs.

**Tech Stack:** Rust; knixl-kdl/knixl-pipeline (parsing, wiring), knixl-lock (schema), knixl-nix
(resolver, eval), knixl-oracle (cache key), knixl (CLI).

## Global Constraints

- British spelling in prose and comments. No em-dashes or en-dashes.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless,
  delve, and the AI-smell set.
- **Run `cargo fmt --all` before finishing each task and keep it clean** (CI runs
  `cargo fmt --all --check`; the repo is rustfmt-normalised). Do NOT hand-format.
- Never run git/but or commit in a task; the controller commits. Do NOT run `git stash`/`git
  status`/any git command.
- Determinism: the augmented `options.json` is a pure function of `(nixpkgs rev, ordered module
  pins)`; module pins render in lock source order; no `HashMap` on emit/lock paths.
- `plan`/`generate` stay offline and pure: no nix shell-outs mid-plan. Resolution and the eval
  build happen at `install`/`upgrade` (online); a missing cache at plan time falls back to
  no-check (best-effort), as today.
- Oracle stays best-effort (ADR 0003): `Oracle::check` semantics are unchanged.

---

### Task 1: `knixl.kdl` parsing and effective-set computation

**Files:**
- Create: `crates/knixl-pipeline/src/project.rs` (parse `knixl.kdl`; effective-set helpers)
- Modify: `crates/knixl-pipeline/src/lib.rs` or `gather.rs` (expose the module; wire nothing yet)
- Test: `crates/knixl-pipeline/src/project.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct OracleModule { pub name: String, pub flake: String, pub attr: String } // attr default "default"

  #[derive(Clone, PartialEq, Eq, Debug, Default)]
  pub struct ProjectConfig {
      pub default_release: Option<String>,   // `nixpkgs release="..."` at project root
      pub oracle_modules: Vec<OracleModule>, // project default set (source order)
  }

  pub fn parse_project(root: &std::path::Path) -> Result<ProjectConfig, ProjectError>; // absent file => Default
  /// The effective module set for a host: its own `oracle-modules` block (replace) if present,
  /// else the project default. `host_modules` is None when the host declares no block.
  pub fn effective_modules<'a>(project: &'a [OracleModule], host_modules: Option<&'a [OracleModule]>) -> &'a [OracleModule];
  pub fn parse_host_oracle_modules(host_src: &str) -> Option<Vec<OracleModule>>; // None if no block
  ```

- [ ] **Step 1: Failing tests**

```rust
    #[test]
    fn parses_project_default_release_and_modules() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"),
            "nixpkgs release=\"25.05\"\noracle-modules {\n    module \"disko\" flake=\"github:nix-community/disko\"\n    module \"sops-nix\" flake=\"github:Mic92/sops-nix\" attr=\"default\"\n}\n").unwrap();
        let p = parse_project(dir.path()).unwrap();
        assert_eq!(p.default_release.as_deref(), Some("25.05"));
        assert_eq!(p.oracle_modules.len(), 2);
        assert_eq!(p.oracle_modules[0].name, "disko");
        assert_eq!(p.oracle_modules[0].flake, "github:nix-community/disko");
        assert_eq!(p.oracle_modules[0].attr, "default"); // defaulted
    }

    #[test]
    fn absent_project_file_is_default() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(parse_project(dir.path()).unwrap(), ProjectConfig::default());
    }

    #[test]
    fn host_oracle_modules_replace_the_project_default() {
        let project = vec![OracleModule { name: "disko".into(), flake: "a".into(), attr: "default".into() }];
        let host = vec![OracleModule { name: "sops-nix".into(), flake: "b".into(), attr: "default".into() }];
        // host present => host wins (replace)
        assert_eq!(effective_modules(&project, Some(&host)), host.as_slice());
        // host absent => project default
        assert_eq!(effective_modules(&project, None), project.as_slice());
    }

    #[test]
    fn parse_host_oracle_modules_reads_a_block_or_none() {
        let with = "host \"nas\" {\n    oracle-modules { module \"disko\" flake=\"x\" }\n}";
        let got = parse_host_oracle_modules(with).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "disko");
        assert!(parse_host_oracle_modules("host \"web\" { }").is_none());
    }
```

Run: `cargo test -p knixl-pipeline project`. Expected: FAIL (module absent).

- [ ] **Step 2: Implement `project.rs`**

Parse `root/knixl.kdl` (absent -> `ProjectConfig::default()`). Read a top-level `nixpkgs`
node's `release` prop (reuse `knixl_kdl::child_prop_str`-style access, but at document top level).
Read an `oracle-modules` node's `module` children: `name` = first arg, `flake` = `flake` prop,
`attr` = `attr` prop or `"default"`. `parse_host_oracle_modules` finds the `host` node's
`oracle-modules` child and reads the same. `effective_modules` returns host slice if `Some`, else
project slice. Add `thiserror` `ProjectError` (parse/io). Add `tempfile` as a dev-dependency if
not present.

- [ ] **Step 3: Run + fmt**

Run: `cargo test -p knixl-pipeline`, `cargo fmt --all`, `cargo clippy -p knixl-pipeline --all-targets -- -D warnings`. Expected: PASS, clean.

- [ ] **Step 4: No commit.**

---

### Task 2: Lock schema for module pins

**Files:**
- Modify: `crates/knixl-lock/src/model.rs` (`OracleModulePin`; add `modules` to `OraclePin` and
  `HostBaseline`; render + parse; keep byte-stable when empty)
- Modify: `crates/knixl-lock/src/reconcile.rs` (carry module pins through reconciliation)
- Test: `crates/knixl-lock/src/model.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct OracleModulePin { pub name: String, pub url: String, pub rev: String, pub attr: String }
  // OraclePin  { nixpkgs_rev, options_hash, modules: Vec<OracleModulePin> }
  // HostBaseline { release, nixpkgs_rev, options_hash, modules: Vec<OracleModulePin> }
  ```

- [ ] **Step 1: Failing tests**

Extend the existing round-trip tests. A lock with `oracle-module` child lines under `oracle` and
under a host `baseline` parses back to the same `Lock`; a lock without them renders byte-identical
to today (regression guard). Example rendered shape:
```
    oracle nixpkgs-rev="deadbeef" options-hash="blake3:x"
        oracle-module name="disko" url="https://github.com/nix-community/disko" rev="abc" attr="default"
```

```rust
    #[test]
    fn oracle_module_pins_round_trip() {
        let mut lock = /* build a Lock with oracle.modules = [OracleModulePin{..}] and one host baseline with modules */;
        let text = lock.render();
        let back = Lock::parse(&text).unwrap();
        assert_eq!(back, lock);
        assert!(text.contains("oracle-module name=\"disko\""));
    }

    #[test]
    fn a_lock_without_module_pins_renders_unchanged() {
        // an OraclePin/HostBaseline with empty `modules` renders exactly as before (no extra lines)
        let text = /* existing fixture lock */.render();
        assert!(!text.contains("oracle-module"));
    }
```

Run: `cargo test -p knixl-lock`. Expected: FAIL to compile (`modules` field / `OracleModulePin`
absent).

- [ ] **Step 2: Add the field and type; update all construction sites**

Add `OracleModulePin`; add `pub modules: Vec<OracleModulePin>` to `OraclePin` and `HostBaseline`;
set `modules: vec![]` at every existing construction site (model.rs tests, gather.rs, main.rs,
reconcile.rs) so the workspace compiles.

- [ ] **Step 3: Render**

In `render`, after the `oracle nixpkgs-rev=... options-hash=...` line, emit one indented
`oracle-module name=".." url=".." rev=".." attr=".."` line per pin, in `modules` order; none when
empty (byte-stable). Same for each host `baseline` block. Escape strings with the existing `esc`.

- [ ] **Step 4: Parse**

In the `oracle` and `baseline` parse arms, read `oracle-module` child nodes into `modules`
(name = first arg or `name` prop, then `url`/`rev`/`attr` props). Absent children -> empty vec.

- [ ] **Step 5: Reconcile**

`reconcile.rs`: carry `oracle`/`baseline` `modules` through `build_lock_next` unchanged (they are
inputs recorded at install/upgrade; reconciliation preserves them, like `options_hash`). No new
pruning logic beyond keeping them attached to their oracle/baseline entry.

- [ ] **Step 6: Run + fmt**

Run: `cargo test -p knixl-lock`, `cargo fmt --all`, `cargo build --workspace --tests`,
`cargo clippy -p knixl-lock --all-targets -- -D warnings`. Expected: PASS, clean.

- [ ] **Step 7: No commit.**

---

### Task 3: Module-source resolver + install/upgrade pinning

**Files:**
- Create: `crates/knixl-nix/src/module.rs` (`ModuleResolver`: flake ref -> `{ url, rev }`)
- Modify: `crates/knixl-nix/src/lib.rs` (export it)
- Modify: `crates/knixl/src/main.rs` (resolve declared modules at install/upgrade; write pins to
  the lock revertably, mirroring the baseline pre-pass)
- Test: `crates/knixl-nix/src/module.rs` (pure `rev_from_*`); `crates/knixl/tests/cli.rs` (shim)

**Interfaces:**
- Produces:
  ```rust
  pub enum ModuleResolver { External(String), Builtin }
  pub struct ResolvedModule { pub url: String, pub rev: String }
  impl ModuleResolver {
      pub fn resolve() -> ModuleResolver;                 // KNIXL_MODULE_RESOLVER or builtin
      pub fn lookup(&self, flake_ref: &str) -> Result<ResolvedModule, ModuleError>;
  }
  pub fn url_from_flake_ref(flake_ref: &str) -> Option<String>;   // github:o/r -> https://github.com/o/r
  pub fn rev_from_ls_remote(out: &str) -> Option<String>;         // reuse baseline's helper shape
  ```

- [ ] **Step 1: Failing tests (pure helpers)**

```rust
    #[test]
    fn github_flake_ref_to_url() {
        assert_eq!(url_from_flake_ref("github:nix-community/disko").as_deref(),
                   Some("https://github.com/nix-community/disko"));
        assert_eq!(url_from_flake_ref("github:Mic92/sops-nix").as_deref(),
                   Some("https://github.com/Mic92/sops-nix"));
    }
    #[test]
    fn ls_remote_head_to_rev() {
        let out = "abc123\tHEAD\ndef\trefs/heads/main\n";
        assert_eq!(rev_from_ls_remote(out).as_deref(), Some("abc123"));
    }
```

Run: `cargo test -p knixl-nix module`. Expected: FAIL (module absent).

- [ ] **Step 2: Implement the resolver**

Mirror `baseline.rs`: `Builtin` runs `git ls-remote <url> HEAD` (default branch head) and parses
the rev; `External(cmd)` runs `<cmd> <flake-ref>` and reads a rev on stdout. `url_from_flake_ref`
maps `github:owner/repo` -> `https://github.com/owner/repo` (return None for unsupported forms,
so the caller can error clearly). `ModuleError` via `thiserror`.

- [ ] **Step 3: Wire into install/upgrade (main.rs)**

Where baselines are resolved (the #22 pre-pass), also resolve each declared module (project set,
or a host's override set) to `OracleModulePin { name, url, rev, attr }`, IN MEMORY for the plan,
and write them to the lock's `oracle.modules` (project) / host `baseline.modules` (override) only
on confirmed apply (revertable), exactly as baselines are written. `plan`/`generate` do not
resolve (offline). An unresolvable module ref refuses (exit 5), like an unresolved baseline.

- [ ] **Step 4: Failing CLI test then green**

Add a `cli.rs` test (resolver shim via `KNIXL_MODULE_RESOLVER`) proving an `install` on a project
with `knixl.kdl` `oracle-modules` records the resolved `oracle-module` pins in the lock, and a
cancelled/`plan` run does not. Run `cargo test -p knixl-nix -p knixl`; watch fail then pass.

- [ ] **Step 5: Run + fmt**

Run: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`. Expected: clean.

- [ ] **Step 6: No commit.**

---

### Task 4: `nixosOptionsDoc` build expression + eval + caching

**Files:**
- Create: `crates/knixl-nix/src/optionsdoc.rs` (the expression + `NixEval` build; return the
  `options.json` text)
- Modify: `crates/knixl-oracle/src/lib.rs` (effective-set cache key: `cache_path_for(rev, &pins)`)
- Modify: `crates/knixl-nix/src/lib.rs` (export)
- Test: `crates/knixl-oracle` (cache-key), `crates/knixl-nix` (nix-gated integration)

**Interfaces:**
- Produces:
  ```rust
  // knixl-nix
  /// Build options.json for a nixpkgs rev plus module pins. Empty pins => base options.
  pub fn build_options_json(eval: &NixEval, nixpkgs_rev: &str, modules: &[(String /*url*/, String /*rev*/, String /*attr*/)]) -> Result<String, NixError>;
  // knixl-oracle
  /// Cache path for an effective set: rev-only file when `modules` is empty (base compat),
  /// else `options-<effective-hash>.json`.
  pub fn cache_path_for(rev: &str, modules: &[(String, String, String)]) -> Option<std::path::PathBuf>;
  pub fn effective_hash(rev: &str, modules: &[(String, String, String)]) -> String;
  ```

- [ ] **Step 1: Cache-key tests (pure, no nix)**

```rust
    #[test]
    fn empty_modules_keeps_rev_only_cache_path() {
        let p = cache_path_for("deadbeef", &[]).unwrap();
        assert!(p.ends_with("options-deadbeef.json"));
    }
    #[test]
    fn module_set_changes_the_cache_key_and_is_order_sensitive() {
        let a = effective_hash("r", &[("u1".into(),"v1".into(),"default".into()), ("u2".into(),"v2".into(),"default".into())]);
        let b = effective_hash("r", &[("u2".into(),"v2".into(),"default".into()), ("u1".into(),"v1".into(),"default".into())]);
        let c = effective_hash("r", &[]);
        assert_ne!(a, b, "order matters");
        assert_ne!(a, c, "modules change the hash");
        assert_eq!(a, effective_hash("r", &[("u1".into(),"v1".into(),"default".into()), ("u2".into(),"v2".into(),"default".into())]));
    }
```

Run: `cargo test -p knixl-oracle cache`. Expected: FAIL.

- [ ] **Step 2: Implement cache key**

`effective_hash` = `knixl_nix::hash` over a canonical string `rev + "\n" + join("\n", "url@rev#attr")`
in slice order. `cache_path_for` returns the existing rev-only path for empty modules, else
`options-<effective_hash>.json` under the same cache dir. Keep `cache_path(rev)` for base compat.

- [ ] **Step 3: The build expression**

Write `optionsdoc.rs` with a Nix expression string that, given `nixpkgs` (fetched at `rev`) and a
list of module sources, evaluates the NixOS module system with the base modules plus each declared
module and runs `nixosOptionsDoc`, printing the `options.json` to stdout. Approach: fetch nixpkgs
via `builtins.fetchGit`; import each module as `(builtins.getFlake "<url>?rev=<rev>").nixosModules.<attr>`
(or `fetchGit` + the module entrypoint if getFlake is unavailable); build a NixOS eval
(`nixos/lib/eval-config.nix` with a minimal `hostPlatform`), then
`pkgs.nixosOptionsDoc { inherit (eval) options; }` and read `optionsJSON`. Run it through
`NixEval` (nix-build / nix eval) and return the resulting `options.json` text. This expression is
nix-version sensitive and will need iteration against a real nix; keep the base (no-modules) path
working first, then add modules.

- [ ] **Step 4: nix-gated integration test**

A test (skipped when nix is absent, like the golden/formatter tests) that `build_options_json`
for a small pinned `nixpkgs` rev with no modules yields JSON parseable by
`Oracle::from_options_json`, and with one real module (e.g. a tiny pinned module) contains that
module's option prefix. Guard on nix presence; do not fail CI when nix is unavailable.

- [ ] **Step 5: Run + fmt**

Run: `cargo test -p knixl-oracle -p knixl-nix`, `cargo fmt --all`,
`cargo clippy --all-targets -- -D warnings`. Expected: PASS (nix test may skip), clean.

- [ ] **Step 6: No commit.**

---

### Task 5: gather/plan wiring + ADR 0008 + docs

**Files:**
- Modify: `crates/knixl-pipeline/src/gather.rs` (per-host effective set -> augmented oracle load)
- Modify: `crates/knixl/src/main.rs` (build+cache the augmented `options.json` at install/upgrade)
- Create: `docs/adr/0008-out-of-tree-oracle-modules.md`
- Modify: `docs/03-module-system.md` / `docs/06-oracle.md` (knixl.kdl, oracle-modules, the build)
- Test: `crates/knixl-pipeline` (effective-set -> oracle selection), `crates/knixl/tests/cli.rs`

**Interfaces:**
- Consumes: Task 1 `parse_project`/`effective_modules`, Task 2 lock pins, Task 4 `cache_path_for`
  / `build_options_json`.

- [ ] **Step 1: Failing tests**

`gather` builds each host's oracle from `cache_path_for(effective rev, effective module pins)`
(env `KNIXL_OPTIONS_JSON` still wins; absent cache -> no check). A CLI test: a host emitting a
`disko.*` path validates clean when an augmented `options.json` (containing `disko.*`) is cached
for its effective set, and fails `UnknownOption` when only the base set is present. Write these,
watch fail.

- [ ] **Step 2: gather wiring**

In `gather`, parse `knixl.kdl` once; for each host compute effective (rev, module pins) from the
lock (`oracle`/`baseline` + module pins), load `cache_path_for(rev, pins)` as its oracle. Keep the
`KNIXL_OPTIONS_JSON` override and the absent -> no-check fallback.

- [ ] **Step 3: install/upgrade build**

At install/upgrade (after resolving module pins, Task 3), call `build_options_json` for each
distinct effective set and write it to `cache_path_for(...)`; record the `options-hash` (blake3 of
the built content) in the lock's `oracle`/`baseline` on confirmed apply. nix absent -> skip the
build with a warning (best-effort), unless `--strict`.

- [ ] **Step 4: ADR 0008 + docs**

Write `docs/adr/0008-out-of-tree-oracle-modules.md` (the oracle set spans declared, pinned
out-of-tree modules; `knixl.kdl` declares the default release + oracle-modules with per-host
replace-override; module pins join the reproducibility boundary; refines ADR 0003 and 0007).
Update docs/06 (the build is now automated; knixl.kdl; the augmented set) and docs/03 (a
declarative module may target out-of-tree options once declared). British spelling, no dashes.

- [ ] **Step 5: Full check**

Run: `cargo test --workspace`, `cargo fmt --all --check`, `cargo build --workspace --tests`,
`cargo clippy --all-targets -- -D warnings`. Also the golden pipeline test
(`KNIXL_FORMATTER=/home/wes/.nix-profile/bin/nixfmt cargo test -p knixl-pipeline --test golden`),
unchanged. Expected: all PASS.

- [ ] **Step 6: No commit.**

---

## Self-Review

- Spec coverage: T1 `knixl.kdl` + effective set; T2 lock module pins; T3 resolver + install/upgrade
  pinning; T4 the `nixosOptionsDoc` build + effective-set cache key; T5 gather/plan wiring + ADR
  0008 + docs. Together they build the oracle set from nixpkgs + declared out-of-tree modules,
  pinned and reproducible, validating `disko.*`/`sops.*`. Out-of-scope items (the disko/secrets
  modules, value validation, `Oracle::check` semantics) are excluded.
- Placeholders: T1-T3 give concrete code/tests; T4's nix expression is specified by approach +
  nix-gated tests (a novel expression that needs iteration against a real nix, not fully
  pinnable in a plan); T5 gives the wiring + tests. This is called out, not hidden.
- Type consistency: `OracleModule` (T1) -> resolved to `OracleModulePin` (T2 lock, T3 resolver) ->
  fed as `(url, rev, attr)` tuples to `cache_path_for`/`build_options_json` (T4) and read back in
  gather (T5). `ProjectConfig`/`effective_modules` (T1) consumed by T3 (resolve) and T5 (wire).
  Lock `modules: Vec<OracleModulePin>` on `OraclePin`/`HostBaseline` (T2) is set by T3 and read by
  T5.
- Ordering: each task keeps the workspace green (T2 sets `modules: vec![]` at all sites; new code
  is unused until wired in T5). `cargo fmt --all` at each task end keeps CI's fmt check green.
