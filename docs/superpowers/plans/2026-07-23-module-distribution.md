# Module distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make knixl useful out of the box: embed the curated declarative modules into the binary (stdlib), and let a project declare external module sources in `knixl.kdl` that resolve to a lock-pinned, cached module loaded at generate. Four-layer registry precedence with a shadow notice.

**Architecture:** The stdlib is embedded via `include_dir!` from the repo `modules/` tree and registered lowest-precedence. Fetched modules mirror the existing out-of-tree `oracle-modules` mechanism (ADR 0008): declared in `knixl.kdl`, resolved by `ModuleResolver` at install/upgrade, cached by rev, pinned in the lock, loaded offline at generate. `build_registry` layers built-in > local > fetched > stdlib and reports shadows.

**Tech Stack:** Rust, the `include_dir` crate, `git` (single-file fetch at a rev), the existing `knixl_nix::module::ModuleResolver`, `knixl-oracle`'s cache-dir convention.

## Global Constraints

From the spec (`docs/superpowers/specs/2026-07-23-module-distribution-design.md`) and repo house rules:

- The repo IS rustfmt-normalised; CI runs `cargo fmt --all --check`. Keep fmt + clippy clean.
- Precedence is fixed: **built-in > local (`<project>/modules/*`) > fetched (`knixl.kdl` `modules{}`) > stdlib (embedded)**. Duplicate node WITHIN a layer is a hard error; a lower layer claiming a node a higher layer already took is a shadow (higher wins, emit a notice — never silent).
- Reproducibility is load-bearing: `generate` is offline. Network resolution happens only in `install`/`upgrade`. A fetched module is pinned by `rev` AND content `hash` in the lock; generate reads the cache and verifies the hash (mismatch = hard error, never a silent refetch). A declared source with no lock pin is a validation error (exit 5) naming `install`/`upgrade`, exactly like an unresolved nixpkgs baseline.
- The repo `modules/` tree stays the single source of truth for the stdlib (golden tests validate it); the binary embeds that same tree.
- Determinism: iterate embedded entries in sorted order; no `HashMap` on emit paths.
- British spelling in prose/comments; no em/en-dashes; no banned AI-tell vocabulary.
- Implementers: leave changes uncommitted, run no git/`but` command (including `git stash`). The controller commits.

## Reference points to mirror (read these first)

- Lock pin type + render + parse: `knixl_lock::model::OracleModulePin`, `render_oracle_modules_block`, `parse_oracle_modules` (`crates/knixl-lock/src/model.rs`).
- Project parse: `oracle_modules_from_node` / `OracleModule` (`crates/knixl-pipeline/src/project.rs`).
- Flake-ref resolution: `knixl_nix::module::ModuleResolver::resolve().lookup(flake_ref) -> ResolvedModule { url, rev }`.
- Cache dir convention: `knixl-oracle`'s `cache_dir()` (`$XDG_CACHE_HOME/knixl`); mirror its shape for a module cache.
- Current registry: `gather::build_registry` (`crates/knixl-pipeline/src/gather.rs`) and the golden harness `build_registry` (`crates/knixl-pipeline/tests/golden.rs`).

---

### Task 1: Embedded stdlib, layered registry, shadow notices

**Files:**
- Modify: `crates/knixl-modules/Cargo.toml` (add `include_dir`)
- Create: `crates/knixl-modules/src/stdlib.rs`
- Modify: `crates/knixl-modules/src/lib.rs` (expose `stdlib`, add `ShadowNotice`)
- Modify: `crates/knixl-pipeline/src/gather.rs` (layer built-in + local + stdlib; return notices)
- Modify: `crates/knixl-pipeline/tests/golden.rs` (harness registers built-ins + stdlib)

**Interfaces:**
- Produces: `knixl_modules::ShadowNotice { node: String, kept: ModuleLayer, shadowed: ModuleLayer }` (with a `ModuleLayer` enum `Builtin|Local|Fetched|Stdlib`); `knixl_modules::stdlib::register_stdlib(reg: &mut Registry) -> Vec<ShadowNotice>` (registers each embedded module whose node is not already claimed, returns a notice per skip); a layered `gather::build_registry` returning `(Registry, Vec<ShadowNotice>)`.
- Task 5 extends `build_registry` with the fetched layer between local and stdlib.

- [ ] **Step 1: Add the dependency**

In `crates/knixl-modules/Cargo.toml`, add to `[dependencies]`:

```toml
include_dir = "0.7"
```

Run `cargo metadata` or a build to confirm it resolves (Step 3 covers the build).

- [ ] **Step 2: Add the `ShadowNotice`/`ModuleLayer` types**

In `crates/knixl-modules/src/lib.rs`, near `Registry` re-exports, add:

```rust
/// Which precedence layer a module came from. Higher variants win.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleLayer {
    Stdlib,
    Fetched,
    Local,
    Builtin,
}

impl ModuleLayer {
    pub fn label(self) -> &'static str {
        match self {
            ModuleLayer::Stdlib => "stdlib",
            ModuleLayer::Fetched => "fetched",
            ModuleLayer::Local => "local",
            ModuleLayer::Builtin => "built-in",
        }
    }
}

/// A node claimed by more than one layer: `kept` won, `shadowed` was skipped. Surfaced as a
/// warning so shadowing is never silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowNotice {
    pub node: String,
    pub kept: ModuleLayer,
    pub shadowed: ModuleLayer,
}

impl ShadowNotice {
    pub fn message(&self) -> String {
        format!(
            "module node `{}`: {} module shadows the {} one",
            self.node,
            self.kept.label(),
            self.shadowed.label()
        )
    }
}

pub mod stdlib;
```

- [ ] **Step 3: Write the stdlib embed + registration (with failing tests)**

Create `crates/knixl-modules/src/stdlib.rs`:

```rust
//! The curated declarative modules embedded into the binary. Source of truth is the repo
//! `modules/` tree (golden-tested); this bundles it so any project has the stdlib offline,
//! with no local copy.
use crate::registry::Registry;
use crate::template::DeclarativeModule;
use crate::{ModuleLayer, ShadowNotice};
use include_dir::{include_dir, Dir};

static STDLIB: Dir = include_dir!("$CARGO_MANIFEST_DIR/../../modules");

/// Register every embedded stdlib module whose claimed node is not already taken by a
/// higher-precedence layer. Returns a shadow notice for each module skipped because its node
/// was already claimed. Iterates entries in sorted name order for determinism.
pub fn register_stdlib(reg: &mut Registry) -> Vec<ShadowNotice> {
    let mut notices = Vec::new();
    let mut dirs: Vec<&Dir> = STDLIB.dirs().collect();
    dirs.sort_by(|a, b| a.path().cmp(b.path()));
    for d in dirs {
        let Some(file) = d.get_file(d.path().join("knixl-module.kdl")) else {
            continue;
        };
        let src = file
            .contents_utf8()
            .expect("embedded stdlib module is valid UTF-8");
        let doc = src
            .parse::<kdl::KdlDocument>()
            .expect("embedded stdlib module parses (golden-tested)");
        let module = DeclarativeModule::from_kdl(&doc, file.path())
            .expect("embedded stdlib module type-checks (golden-tested)");
        let node = module.node_name().to_string();
        if reg.get(&node).is_some() {
            notices.push(ShadowNotice {
                node,
                kept: layer_of(reg, &module),
                shadowed: ModuleLayer::Stdlib,
            });
            continue;
        }
        // register() only errors on a duplicate, which the guard above already excluded.
        let _ = reg.register(Box::new(module));
    }
    notices
}

// The kept layer is whatever already claimed the node; the caller (build_registry) knows the
// layering, but for the notice we only need "something higher won". Report the highest
// possible source generically: a claimed node here was taken by built-in, local, or fetched.
fn layer_of(_reg: &Registry, _m: &DeclarativeModule) -> ModuleLayer {
    // build_registry registers built-in, then local, then fetched, then stdlib, so anything
    // already present outranks stdlib; the precise higher layer is not needed for the notice's
    // purpose (telling the user their stdlib module was shadowed). Report Local as the common
    // case; build_registry refines this when it has the layer map (see Task 5 note).
    ModuleLayer::Local
}
```

Note: the `layer_of` shortcut is acceptable for Task 1 (only built-in + local + stdlib exist yet, and a shadowed stdlib module is almost always shadowed by a local one). Task 5 introduces the fetched layer; if precise attribution matters there, `build_registry` — which owns the layer map — can construct the notice itself. Keep `register_stdlib` returning `Stdlib` as the `shadowed` layer regardless.

Add tests in `stdlib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::builtin::register_builtins;
    use crate::registry::Registry;

    #[test]
    fn stdlib_registers_declarative_modules() {
        let mut reg = Registry::new();
        let notices = register_stdlib(&mut reg);
        assert!(notices.is_empty(), "fresh registry: no shadows");
        // A known stdlib node resolves purely from the embed.
        assert!(reg.get("web-service").is_some());
        assert!(reg.get("zfs").is_some());
        assert!(reg.get("tailscale").is_some());
        assert!(reg.get("incus").is_some());
    }

    #[test]
    fn stdlib_skips_a_node_a_builtin_already_claims_without_a_false_shadow() {
        // No stdlib module claims a built-in node today, so registering built-ins first must
        // not produce any shadow notice.
        let mut reg = Registry::new();
        register_builtins(&mut reg);
        let notices = register_stdlib(&mut reg);
        assert!(
            notices.iter().all(|n| n.shadowed == ModuleLayer::Stdlib),
            "any notice must be about a shadowed stdlib module"
        );
    }
}
```

- [ ] **Step 4: Build and run the stdlib tests**

Run: `cargo test -p knixl-modules stdlib`
Expected: pass. If `include_dir!` cannot find the path, confirm the glob is `$CARGO_MANIFEST_DIR/../../modules` (knixl-modules is at `crates/knixl-modules`, so `../../modules` is the repo root `modules/`).

- [ ] **Step 5: Layer `gather::build_registry`**

In `crates/knixl-pipeline/src/gather.rs`, change `build_registry` to register built-ins, then local (`<root>/modules/*`, as today), then the embedded stdlib, collecting shadow notices, and return them:

```rust
fn build_registry(root: &Path) -> Result<(Registry, Vec<knixl_modules::ShadowNotice>), GatherError> {
    let mut registry = Registry::new();
    register_builtins(&mut registry);

    // Local project modules (highest after built-ins). Duplicate-within-layer stays a hard error.
    let dir = root.join("modules");
    if dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        entries.sort();
        for entry in entries {
            let manifest = entry.join("knixl-module.kdl");
            if !manifest.exists() {
                continue;
            }
            let src = std::fs::read_to_string(&manifest)?;
            let doc = knixl_kdl::parse(&src).map_err(|e| GatherError::Module(e.to_string()))?;
            let module = DeclarativeModule::from_kdl(&doc, &manifest)
                .map_err(|e| GatherError::Module(e.to_string()))?;
            registry
                .register(Box::new(module))
                .map_err(|e| GatherError::Module(e.to_string()))?;
        }
    }

    // Embedded stdlib fills any node not already claimed.
    let notices = knixl_modules::stdlib::register_stdlib(&mut registry);
    Ok((registry, notices))
}
```

Update `pub fn registry(root)` to return `Ok(build_registry(root)?.0)` (callers that only want the registry drop the notices). Find every caller of `build_registry`/`registry` in `gather.rs` and thread the notices where a generate path can surface them: fold the notice messages into the generate warnings (the same `Vec<String>` warning channel the conflict lints use). If wiring the notices into warnings is non-trivial in this task, at minimum return them from `build_registry` and add a `// TODO(#13): surface` at the single call site, and note it in your report — but prefer wiring them into the existing warnings vector.

- [ ] **Step 6: Update the golden harness to use the stdlib**

In `crates/knixl-pipeline/tests/golden.rs`, the harness `build_registry` currently reads the repo `modules/` directory. Change it to register built-ins + the embedded stdlib (the golden temp projects have no local `modules/`), so the golden hosts' declarative modules come from the embed:

```rust
fn build_registry() -> Registry {
    let mut reg = Registry::new();
    register_builtins(&mut reg);
    let _ = knixl_modules::stdlib::register_stdlib(&mut reg);
    reg
}
```

Remove the now-unused `modules_dir()` helper if nothing else uses it (the compiler will warn). Keep `use knixl_modules::template::DeclarativeModule;` only if still referenced.

- [ ] **Step 7: Run the goldens and the workspace**

Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline` then `cargo test --workspace`.
Expected: all goldens (nas, gateway, vmhost, web, ...) still pass byte-for-byte (same modules, now from the embed). Fix any harness/import fallout. Then `cargo fmt --all --check` and `cargo clippy --workspace --all-targets` clean.

- [ ] **Step 8: Report**

---

### Task 2: Lock `ModuleSourcePin`

**Files:**
- Modify: `crates/knixl-lock/src/model.rs` (type + render + parse + test)

**Interfaces:**
- Produces: `knixl_lock::model::ModuleSourcePin { name: String, url: String, rev: String, hash: Hash }`, rendered as a top-level `module-source "<name>" url= rev= hash=` line and parsed back. Task 5 reads/writes these.

- [ ] **Step 1: Write the failing round-trip test**

In `model.rs` tests, add a test that builds a `Lock` with one `ModuleSourcePin`, calls `.render()`, parses the text back, and asserts the pin round-trips. Mirror the existing oracle-pin round-trip test in this file for structure.

- [ ] **Step 2: Add the type and the lock field**

Add near `OracleModulePin`:

```rust
/// A pin for a fetched declarative module source (issue #13): the resolved source and the
/// exact bytes, so a declared module is reproducible and generate stays offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleSourcePin {
    pub name: String,
    pub url: String,
    pub rev: String,
    pub hash: Hash,
}
```

Add a field to the `Lock` struct: `pub module_sources: Vec<ModuleSourcePin>` (default empty). Update any `Lock` constructor/`Default`/struct-literal sites the compiler flags to include it (empty vec).

- [ ] **Step 3: Render and parse**

In `Lock::render`, after the oracle block, render each `module_sources` pin (sorted by `name` for determinism), one per line:

```rust
module-source "<name>" url="<url>" rev="<rev>" hash="<hash>"
```

Mirror `render_oracle_modules_block`'s escaping/format. In the lock parser's node match, add a `"module-source"` arm that reads the four props (name is arg 0) into a `ModuleSourcePin` and pushes it onto `module_sources`. Mirror the `"oracle"`/`"module"` parse arms.

- [ ] **Step 4: Run the test**

Run: `cargo test -p knixl-lock module_source`
Expected: the round-trip test passes.

- [ ] **Step 5: fmt + clippy, report**

Run: `cargo fmt --all && cargo fmt --all --check && cargo clippy -p knixl-lock --all-targets`. Report.

---

### Task 3: Project `modules { }` parse

**Files:**
- Modify: `crates/knixl-pipeline/src/project.rs` (type, parse, test)

**Interfaces:**
- Produces: `ModuleSource { name: String, flake: String, path: String }` (path defaults to `""` meaning repo root) and `ProjectConfig.module_sources: Vec<ModuleSource>`, parsed from a top-level `modules { module "name" flake="..." [path="..."] }` block. Task 5 consumes it.

- [ ] **Step 1: Write the failing test**

Mirror the existing `oracle_modules`-parse tests: a `knixl.kdl` with a `modules { module "nginx" flake="github:org/knixl-nginx"\n module "graf" flake="github:org/g" path="modules/graf" }` block parses to two `ModuleSource`s with the right fields (`path` defaulting to `""`). An absent block yields an empty vec.

- [ ] **Step 2: Add the type, field, and parser**

Add:

```rust
/// A declared external declarative-module source (issue #13): a flake ref plus the directory
/// within it holding `knixl-module.kdl` (empty = repo root). `name` is the local handle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ModuleSource {
    pub name: String,
    pub flake: String,
    pub path: String,
}
```

Add `pub module_sources: Vec<ModuleSource>` to `ProjectConfig` (it derives `Default`; empty vec is fine). In `parse_project`, find a top-level `modules` node and read its `module` children (mirror `oracle_modules_from_node`), each with arg 0 = name, `flake` prop (required — a `module` with no `flake` is a `ProjectError`), `path` prop (optional, default `""`).

- [ ] **Step 3: Run, fmt, clippy, report**

Run: `cargo test -p knixl-pipeline module_sources`, then fmt + clippy. Report.

---

### Task 4: Fetch and cache a module by rev

**Files:**
- Create: `crates/knixl-nix/src/module_fetch.rs` (or extend `crates/knixl-nix/src/module.rs`)
- Modify: `crates/knixl-nix/src/lib.rs` (expose it)

**Interfaces:**
- Produces:
  - `module_cache_path(url: &str, rev: &str, path: &str) -> Option<PathBuf>` — a deterministic cache location under the same `knixl` cache dir the oracle uses (`$XDG_CACHE_HOME/knixl`, else `$HOME/.cache/knixl`); key on a hash of `url\nrev\npath` so different sources never collide, filename e.g. `module-<hash>.kdl`.
  - `fetch_module(url: &str, rev: &str, path: &str) -> Result<String, ModuleFetchError>` — fetch `<path>/knixl-module.kdl` at `rev` from `url`, returning its text. Writes nothing; the caller caches.
  - `hash_module(text: &str) -> String` — `"blake3:<hex>"`, the same form the rest of the lock uses.

- [ ] **Step 1: Write the cache-path + hash unit tests (pure, offline)**

Test `module_cache_path` returns a stable path for the same `(url, rev, path)` and different paths for different inputs, and lands under a `knixl` cache dir (set `XDG_CACHE_HOME` to a temp dir in the test). Test `hash_module` is stable and `blake3:`-prefixed.

- [ ] **Step 2: Implement cache path + hash**

Mirror `knixl-oracle`'s `cache_dir()` (do not depend on knixl-oracle; duplicate the tiny `$XDG_CACHE_HOME`/`$HOME/.cache` resolution, as the crate graph forbids new cross-deps — see CLAUDE.md). Key the filename on `blake3(url\nrev\npath)` hex. `hash_module` is `blake3::hash(text.as_bytes())` as `"blake3:<hex>"`.

- [ ] **Step 3: Implement `fetch_module` with a local-repo integration test**

Fetch the single file at a rev without a full clone. Use a temp dir and:

```
git init -q <tmp>
git -C <tmp> remote add origin <url>
git -C <tmp> fetch -q --depth 1 origin <rev>
git -C <tmp> checkout -q FETCH_HEAD -- <path>/knixl-module.kdl   # or read via `git -C <tmp> show FETCH_HEAD:<path>/knixl-module.kdl`
```

Prefer `git show FETCH_HEAD:<relpath>` piped to stdout (no working-tree checkout needed); `<relpath>` is `knixl-module.kdl` when `path` is empty, else `<path>/knixl-module.kdl`. Map a non-zero git exit or missing file to a `ModuleFetchError` with the source and rev in the message. Use the existing `output_retrying_etxtbsy` helper in `knixl-nix` for the command, matching `ModuleResolver`.

Write a `#[test]` that creates a throwaway local git repo in a temp dir (`git init`, write `knixl-module.kdl`, `git add`, `git -c user.email=t@t -c user.name=t commit`, capture the rev via `git rev-parse HEAD`), then calls `fetch_module("file://<tmprepo>", <rev>, "")` and asserts the returned text matches. This exercises the real git path offline (a `file://` remote is local). Gate it on `git` being available (skip with an eprintln if not, like the formatter-gated golden tests).

- [ ] **Step 4: Run tests, fmt, clippy, report**

Run: `cargo test -p knixl-nix module`, then fmt + clippy. Report. Note in the report whether the git-backed test ran or was skipped (no git).

---

### Task 5: Resolve at install/upgrade, load at generate

**Files:**
- Modify: `crates/knixl-pipeline/src/gather.rs` (fetched layer in `build_registry`; thread `module_sources` + lock)
- Modify: `crates/knixl/src/main.rs` (resolve declared sources in install/upgrade, write pins to the lock)
- Modify: `crates/knixl-pipeline/tests/*` (offline integration tests)

**Interfaces:**
- Consumes: `ModuleSource` (Task 3), `ModuleSourcePin` (Task 2), `fetch_module`/`module_cache_path`/`hash_module` (Task 4), `ModuleResolver` (existing).
- Produces: `build_registry` gains the fetched layer (built-in > local > **fetched** > stdlib). A declared source with no matching lock pin is a validation error; a cached file whose hash != the pin is a hard error.

- [ ] **Step 1: Add the fetched layer to `build_registry`**

Extend `build_registry` (Task 1's version) to accept the declared `module_sources` and the lock's `module_sources` pins, and register the fetched layer between local and stdlib:

- signature: `build_registry(root, sources: &[ModuleSource], pins: &[ModuleSourcePin]) -> Result<(Registry, Vec<ShadowNotice>), GatherError>` (Task 1's callers pass `&[]`, `&[]` until this task wires the real values through `gather`).
- For each declared `ModuleSource`, find its `ModuleSourcePin` by `name`. If none: push a validation error (mirror the unresolved-baseline error; name `install`/`upgrade` as the fix) rather than registering. If found: read `module_cache_path(pin.url, pin.rev, source.path)`, error if the cache file is absent (name install/upgrade), read it, verify `hash_module(text) == pin.hash` (hard error on mismatch — a corrupt/tampered cache must never be silently refetched), parse + `DeclarativeModule::from_kdl`, and register unless the node is already claimed (built-in or local) -> shadow notice `{ kept: Local-or-Builtin, shadowed: Fetched }`.
- Register the stdlib layer last (as Task 1).

Thread `project.module_sources` and `lock.module_sources` from `gather` into `build_registry`. Validation errors join the existing `validation_errors` channel; shadow notices join the warnings.

- [ ] **Step 2: Resolve + pin in install/upgrade**

In `crates/knixl/src/main.rs`, where install/upgrade already resolve oracle module sources and baselines (search for `OracleModulePin`/`ModuleResolver`/the pending-lock construction), add a parallel step for `project.module_sources`:

- for each `ModuleSource`: `ModuleResolver::resolve().lookup(&source.flake)` -> `{ url, rev }`; `fetch_module(&url, &rev, &source.path)` -> text; write text to `module_cache_path(&url, &rev, &source.path)`; `hash = hash_module(&text)`; build `ModuleSourcePin { name, url, rev, hash }`.
- carry these pins in the pending lock the same way baselines/oracle pins are carried, and write them to `lock.module_sources` when the command commits the lock (behind the same `--yes`/confirm gate; never write before the gate, matching the existing baseline handling).

Follow the existing resolve-in-memory-then-write-on-commit discipline exactly (the code comments in `run` describe it). If the install/upgrade wiring is more than this task can safely carry, split: land the `build_registry` fetched layer + a hand-seeded-cache integration test here, and report the install/upgrade resolution as a follow-up needing its own task — but attempt it.

- [ ] **Step 3: Offline integration tests**

Add tests (in `crates/knixl-pipeline/tests/`) that do NOT hit the network:

- Seed a fake cache: write a `knixl-module.kdl` to `module_cache_path(url, rev, "")` under a temp `XDG_CACHE_HOME`, construct a lock with a matching `ModuleSourcePin` (hash = `hash_module(text)`), a `knixl.kdl` declaring that source, and assert `build_registry` registers the fetched node.
- Declared source with NO pin -> `build_registry` yields a validation error naming install/upgrade.
- Cached file present but hash != pin -> hard error.
- A fetched module and a local module claiming the same node -> local wins, one shadow notice `{ shadowed: Fetched }`.

- [ ] **Step 4: Run the workspace suite, fmt, clippy**

Run: `cargo test --workspace` (+ `KNIXL_FORMATTER=$(command -v nixfmt)` for the goldens), then fmt + clippy clean.

- [ ] **Step 5: Report**

---

### Task 6: Docs and ADR 0010

**Files:**
- Create: `docs/adr/0010-module-distribution.md`
- Modify: `docs/03-module-system.md`, `docs/05-cli.md` (and `docs/06` if it documents resolution)

**Interfaces:** none (prose).

- [ ] **Step 1: Write ADR 0010**

Record: the embedded stdlib (source of truth `modules/`, bundled via `include_dir`), fetched modules pinned in the lock (mirroring ADR 0008's out-of-tree oracle-module resolve/cache pattern), and the four-layer precedence (built-in > local > fetched > stdlib) with shadow notices. Status accepted; Relates to ADR 0008. Match the format of the existing ADRs in `docs/adr/`.

- [ ] **Step 2: Document in docs/03 and docs/05**

`docs/03-module-system.md`: the four-layer precedence and the shadow notice. `docs/05-cli.md`: that `install`/`upgrade` resolve declared `modules {}` sources into lock pins and `generate` loads them offline from the cache; a declared source with no pin fails with exit 5.

- [ ] **Step 3: House-style self-check, report**

British spelling; no em/en-dashes; no banned vocabulary.

---

## Notes for the controller

- Base commit before Task 1: the tip of `feat/module-distribution` (the spec commit). Record it; each task's start commit is the BASE for the next.
- Task ordering matters: Task 1 (embed) is the foundation and makes goldens pass via the stdlib; Tasks 2-4 are independent, isolated pieces (lock, project parse, fetch); Task 5 integrates them and is the hardest (install/upgrade wiring + offline integration tests); Task 6 is docs. Tasks 2, 3, 4 could be reviewed in any order but all precede Task 5.
- The final whole-branch review should confirm: precedence is exactly built-in > local > fetched > stdlib with non-silent shadows; generate is offline and hash-verifies the cache (mismatch = hard error, no silent refetch); a declared source with no pin is exit 5; the stdlib is embedded from the repo `modules/` (single source of truth) and the goldens pass from the embed; determinism (sorted embed iteration, no HashMap); fmt + clippy clean; workspace green.
- This branch is independent (off `main`), not stacked.
- If Task 5's install/upgrade resolution proves too large to land safely in one task, it is acceptable to ship the load-at-generate half (with hand-seeded-cache tests) and open a follow-up issue for the install/upgrade auto-resolution, so long as a declared-but-unpinned source fails closed (validation error), never silently. Flag this in the task report for a controller decision.
