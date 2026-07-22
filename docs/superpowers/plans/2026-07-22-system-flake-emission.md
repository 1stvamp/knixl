# System assembly flake emission implementation plan (#40)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** When `knixl.kdl` declares a `system {}` block, `knixl generate` emits
`generated/flake.nix` defining `nixosConfigurations.<host>` for every host, each pinned to that
host's baseline nixpkgs rev, as a generated and locked artefact. Absent the block, behaviour is
unchanged (modules only).

**Architecture:** `parse_project` gains an optional `system` block (`state-version`,
`nixpkgs-url`). A pure `knixl_pipeline::flake::render_system_flake` emits the flake text.
`gather` parses the project config, and when `system` is set, requires every host to have a
resolved baseline rev, renders and formats the flake, and inserts it into the generated-files
map and the lock outputs so it reconciles (`Stale`/`Drifted`/`Orphaned`) like any other
generated file.

**Tech Stack:** Rust, knixl-pipeline. No new crates or dependencies.

## Global Constraints

- British spelling in prose and comments. No em-dashes or en-dashes: commas, colons,
  parentheses, full stops.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless,
  delve, and the AI-smell set.
- The repo IS rustfmt-normalised and CI runs `cargo fmt --all --check` (ci.yml). Run
  `cargo fmt` on the files you touch and ensure `cargo fmt --all --check` is clean before
  finishing. (This corrects an earlier stale "no fmt" convention.)
- Never run git/but or commit in a task; the controller commits.
- Determinism: `render_system_flake` is byte-stable and emits hosts in name order; no `HashMap`
  iteration on the emit path.
- knixl.lock.kdl stays the single lock; the emitted flake has no `inputs` and no `flake.lock`.
- The flake is generated and locked: it flows through the same `generated` map + `ExpectedFile`
  path as host modules, so `Plan::compute` reconciles it with no reconcile-logic changes.

---

### Task 1: `system {}` in `knixl.kdl`

**Files:**
- Modify: `crates/knixl-pipeline/src/project.rs` (`SystemConfig`, `ProjectConfig.system`,
  `parse_project`, `ProjectError`)
- Test: `crates/knixl-pipeline/src/project.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct SystemConfig {
      pub state_version: String,
      pub nixpkgs_url: String, // defaulted when omitted
  }
  // added field on ProjectConfig:
  //   pub system: Option<SystemConfig>,
  pub const DEFAULT_NIXPKGS_URL: &str = "https://github.com/NixOS/nixpkgs";
  ```

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `project.rs`:

```rust
    #[test]
    fn parses_system_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"),
            "system {\n    state-version \"25.05\"\n}\n").unwrap();
        let p = parse_project(dir.path()).unwrap();
        let s = p.system.expect("system present");
        assert_eq!(s.state_version, "25.05");
        assert_eq!(s.nixpkgs_url, DEFAULT_NIXPKGS_URL);
    }

    #[test]
    fn system_block_reads_custom_nixpkgs_url() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"),
            "system {\n    state-version \"24.11\"\n    nixpkgs-url \"https://example.com/nixpkgs\"\n}\n").unwrap();
        let s = parse_project(dir.path()).unwrap().system.unwrap();
        assert_eq!(s.nixpkgs_url, "https://example.com/nixpkgs");
    }

    #[test]
    fn system_block_without_state_version_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "system {\n}\n").unwrap();
        let err = parse_project(dir.path()).unwrap_err();
        assert!(format!("{err}").contains("state-version"), "got: {err}");
    }

    #[test]
    fn no_system_block_is_none() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("knixl.kdl"), "nixpkgs release=\"25.05\"\n").unwrap();
        assert!(parse_project(dir.path()).unwrap().system.is_none());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-pipeline --lib project::tests`
Expected: FAIL to compile (`SystemConfig`, `system` field, `DEFAULT_NIXPKGS_URL` absent).

- [ ] **Step 3: Add the types and the error variant**

Add `SystemConfig` and `DEFAULT_NIXPKGS_URL` near `ProjectConfig`. Add
`pub system: Option<SystemConfig>` to `ProjectConfig` (it derives `Default`; `Option` defaults
to `None`, so the derive still holds). Add a `ProjectError` variant:

```rust
    #[error("knixl.kdl: system {{}} block requires a state-version")]
    MissingStateVersion,
```

- [ ] **Step 4: Parse the block**

In `parse_project`, after the existing `oracle_modules` parse, read a top-level `system` node:

```rust
    let system = match doc.nodes().iter().find(|n| n.name().value() == "system") {
        None => None,
        Some(node) => {
            let state_version = node
                .children()
                .and_then(|c| c.nodes().iter().find(|n| n.name().value() == "state-version").cloned())
                .and_then(|n| knixl_kdl::first_arg_str(&n))
                .ok_or(ProjectError::MissingStateVersion)?;
            let nixpkgs_url = node
                .children()
                .and_then(|c| c.nodes().iter().find(|n| n.name().value() == "nixpkgs-url").cloned())
                .and_then(|n| knixl_kdl::first_arg_str(&n))
                .unwrap_or_else(|| DEFAULT_NIXPKGS_URL.to_string());
            Some(SystemConfig { state_version, nixpkgs_url })
        }
    };
```

Add `system` to the returned `ProjectConfig`. (Confirm `knixl_kdl::first_arg_str` takes
`&KdlNode`; it is already used in this file via `oracle_modules_from_node`. If its signature
differs, read the first positional string directly with
`node...entries().iter().find(|e| e.name().is_none()).and_then(|e| e.value().as_string())`.)

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p knixl-pipeline --lib project::tests` and `cargo build --workspace --tests`
Expected: PASS.

- [ ] **Step 6: Clippy**

Run: `cargo clippy -p knixl-pipeline --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: No commit.**

---

### Task 2: The flake emitter

**Files:**
- Create: `crates/knixl-pipeline/src/flake.rs`
- Modify: `crates/knixl-pipeline/src/lib.rs` (add `pub mod flake;`)
- Test: `crates/knixl-pipeline/src/flake.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, PartialEq, Eq, Debug)]
  pub struct FlakeHost {
      pub name: String,
      pub baseline_rev: String,
      pub module_path: String, // relative to generated/, e.g. "./hosts/nas.nix"
  }
  pub fn render_system_flake(hosts: &[FlakeHost], state_version: &str, nixpkgs_url: &str) -> String;
  ```

- [ ] **Step 1: Write the failing tests**

Create `flake.rs` with only the tests first (types/fn come in Step 3):

```rust
//! Emit the opt-in system-assembly flake (ADR 0009): nixosConfigurations.<host>, each host
//! built against its own baseline nixpkgs rev, pinned by fetchGit (a full rev is a pure pin).

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<FlakeHost> {
        vec![
            FlakeHost { name: "web".into(), baseline_rev: "rev-web".into(), module_path: "./hosts/web.nix".into() },
            FlakeHost { name: "db".into(), baseline_rev: "rev-db".into(), module_path: "./hosts/db.nix".into() },
        ]
    }

    #[test]
    fn deterministic_and_name_ordered() {
        let a = render_system_flake(&hosts(), "25.05", "https://github.com/NixOS/nixpkgs");
        let b = render_system_flake(&hosts(), "25.05", "https://github.com/NixOS/nixpkgs");
        assert_eq!(a, b, "same input, byte-identical");
        // Order-independent: reversing the input yields the same text (sorted by name).
        let mut rev = hosts();
        rev.reverse();
        assert_eq!(render_system_flake(&rev, "25.05", "https://github.com/NixOS/nixpkgs"), a);
        // db precedes web in the output.
        assert!(a.find("\"db\"").unwrap() < a.find("\"web\"").unwrap(), "name order: {a}");
    }

    #[test]
    fn emits_per_host_pin_and_state_version() {
        let out = render_system_flake(&hosts(), "25.05", "https://example.com/nixpkgs");
        assert!(out.contains("nixosConfigurations"));
        assert!(out.contains("\"web\""));
        assert!(out.contains("rev-web"));
        assert!(out.contains("rev-db"));
        assert!(out.contains("./hosts/web.nix"));
        assert!(out.contains("system.stateVersion = \"25.05\""));
        assert!(out.contains("https://example.com/nixpkgs"));
        assert!(out.contains("builtins.fetchGit"));
        assert!(!out.contains("inputs ="), "no flake inputs");
    }
}
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p knixl-pipeline --lib flake::tests`
Expected: FAIL to compile.

- [ ] **Step 3: Implement the emitter**

Above the test module in `flake.rs`:

```rust
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlakeHost {
    pub name: String,
    pub baseline_rev: String,
    pub module_path: String,
}

/// Render the system flake. Hosts are emitted in `name` order so the output is a pure function
/// of the input set (sorted here defensively, independent of caller order). The text is
/// unformatted Nix; the caller runs it through the pinned formatter before hashing.
pub fn render_system_flake(hosts: &[FlakeHost], state_version: &str, nixpkgs_url: &str) -> String {
    let mut sorted: Vec<&FlakeHost> = hosts.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));

    let mut s = String::new();
    s.push_str("# Generated by knixl. Do NOT edit; regenerate with `knixl generate`.\n");
    s.push_str("{\n");
    s.push_str("  description = \"knixl-generated system\";\n");
    s.push_str("  outputs = { ... }:\n");
    s.push_str("    let\n");
    s.push_str(&format!(
        "      pkgsAt = rev: import (builtins.fetchGit {{ url = \"{nixpkgs_url}\"; rev = rev; }}) {{ }};\n"
    ));
    s.push_str("    in\n");
    s.push_str("    {\n");
    s.push_str("      nixosConfigurations = {\n");
    for h in sorted {
        s.push_str(&format!("        \"{}\" = (pkgsAt \"{}\").lib.nixosSystem {{\n", h.name, h.baseline_rev));
        s.push_str("          modules = [\n");
        s.push_str(&format!("            {}\n", h.module_path));
        s.push_str(&format!("            {{ system.stateVersion = \"{state_version}\"; }}\n"));
        s.push_str("          ];\n");
        s.push_str("        };\n");
    }
    s.push_str("      };\n");
    s.push_str("    };\n");
    s.push_str("}\n");
    s
}
```

Add `pub mod flake;` to `lib.rs` beside the other `pub mod` lines.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p knixl-pipeline --lib flake::tests` and `cargo build --workspace --tests`
Expected: PASS.

- [ ] **Step 5: Clippy**

Run: `cargo clippy -p knixl-pipeline --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: No commit.**

---

### Task 3: Wire the flake into `gather`

**Files:**
- Modify: `crates/knixl-pipeline/src/gather.rs` (parse the project config, baseline
  precondition, render + format + insert the flake)
- Test: `crates/knixl-pipeline/tests/` (a new `system_flake.rs` integration test, mirroring the
  `identity_formatter` fixture pattern in `golden.rs`)

**Interfaces:**
- Consumes: Task 1 `ProjectConfig.system` / `parse_project`; Task 2 `render_system_flake` /
  `FlakeHost`; the existing `generated: BTreeMap<PathBuf, String>`, `ExpectedFile`,
  `lock.baselines` (`HostBaseline.nixpkgs_rev`), `host_names(&hosts)`, and `formatter`.

- [ ] **Step 1: Read the surrounding code**

Read `crates/knixl-pipeline/src/gather.rs` around the generated-files assembly (the
`generate(...)` match that fills `generated` and builds `expected`, and the
`declared_baselines` validation-error loop just after it). The flake insertion goes AFTER that
loop, so it can see `validation_errors` and skip emission when the project is already invalid.
Also read `crates/knixl-pipeline/tests/golden.rs` for the `identity_formatter()` and temp-project
fixture helpers to reuse in the new integration test.

- [ ] **Step 2: Failing integration test**

Create `crates/knixl-pipeline/tests/system_flake.rs`. Build a temp project with a `knixl.kdl`
carrying a `system {}` block, one host declaring a `nixpkgs release`, and a
`knixl.lock.kdl` whose per-host `baseline` line resolves that host's rev (copy the lock-writing
helper shape from `baseline_validation.rs` if present, else hand-write the minimal lock). Use
`identity_formatter()` so it runs without nixfmt. Assert:

- `gather(...)` succeeds and `project.generated` contains `generated/flake.nix` whose text
  contains `nixosConfigurations`, the host name, and the host's resolved rev.
- The produced lock `outputs` include an entry for `generated/flake.nix`.
- A second fixture with `system {}` but a host lacking a resolved baseline yields a validation
  error mentioning that host (assert via `project` validation errors / the plan; match how
  `baseline_validation.rs` asserts).
- A fixture WITHOUT a `system {}` block produces no `generated/flake.nix` entry.

Run: `cargo test -p knixl-pipeline --test system_flake`
Expected: FAIL (flake not emitted yet).

- [ ] **Step 3: Parse the project config in `gather`**

Add `use crate::project::parse_project;` (and `flake::{render_system_flake, FlakeHost}`). Near
the top of `gather`, parse the project config from `root`:

```rust
    let project = parse_project(root).map_err(|e| GatherError::Module(e.to_string()))?;
```

(Confirm the `GatherError` variant to map into; reuse whatever the existing code uses for
project/parse failures. If there is a more specific variant than `Module`, use it.)

- [ ] **Step 4: Baseline precondition + emit the flake**

After the `declared_baselines` validation loop, add:

```rust
    if let Some(system) = &project.system {
        // Every host must have a resolved baseline rev to pin nixpkgs (ADR 0009). A host with
        // none cannot be emitted, so refuse (a validation error, surfaced as exit 5).
        let mut flake_hosts = Vec::new();
        let mut missing = false;
        for name in host_names(&hosts) {
            match lock.baselines.get(&name) {
                Some(b) if !b.nixpkgs_rev.is_empty() => flake_hosts.push(FlakeHost {
                    name: name.clone(),
                    baseline_rev: b.nixpkgs_rev.clone(),
                    module_path: format!("./hosts/{name}.nix"),
                }),
                _ => {
                    missing = true;
                    validation_errors.push(format!(
                        "host \"{name}\": system {{}} requires a resolved nixpkgs baseline: run knixl install or upgrade"
                    ));
                }
            }
        }
        // Only emit when every host resolved; a partial flake would lie about the fleet.
        if !missing {
            let raw = render_system_flake(&flake_hosts, &system.state_version, &system.nixpkgs_url);
            let text = formatter.format(&raw).map_err(GatherError::from)?;
            let path = PathBuf::from("generated/flake.nix");
            generated.insert(path.clone(), text.clone());
            expected.push(ExpectedFile {
                path,
                hash: hash(text.as_bytes()),
                from: PathBuf::from("knixl.kdl"),
                modules: Vec::new(),
            });
        }
    }
```

Notes for the implementer:
- `expected` is the `Vec<ExpectedFile>` built from the `generate(...)` match. If it is built as
  an immutable `let expected = ...collect()`, change it to `let mut expected` so the flake entry
  can be pushed. Confirm the exact binding name in the file (it is `expected` in the match arm).
- `formatter.format` returns `Result<String, FormatError>` (or similar). Map its error into
  `GatherError` the same way `generate`'s format errors are handled; if there is no `From`
  impl, use `.map_err(|e| GatherError::Module(e.to_string()))?` to match the file's style.
- `hash` and `ExpectedFile` are already in scope in this file (used by the `generate` match).
  Match `ExpectedFile`'s real field set (read it: `path`, `hash`, `from`, `modules`).

- [ ] **Step 5: Run to verify pass**

Run: `cargo test -p knixl-pipeline --test system_flake` then `cargo test -p knixl-pipeline` and
`cargo build --workspace --tests`
Expected: PASS.

- [ ] **Step 6: Clippy**

Run: `cargo clippy -p knixl-pipeline --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 7: No commit.**

---

### Task 4: Docs

**Files:**
- Modify: `docs/05-cli.md` (generate emits `generated/flake.nix` when `system {}` is declared)
- Modify: `docs/01-architecture.md` (the flake as a generated artefact)

**Interfaces:**
- Consumes: the behaviour from Tasks 1-3. Docs only; no code.

- [ ] **Step 1: docs/05**

In the `generate` bullet (or a short paragraph beneath it), state: when `knixl.kdl` declares a
`system { state-version "<rel>" }` block, `generate` also emits `generated/flake.nix`, a
generated and locked artefact defining `nixosConfigurations.<host>` for every host, each pinned
to that host's baseline nixpkgs rev; consume it with `nixos-rebuild --flake .#<host>` or
`nixos-anywhere`. Add one sentence: without `system {}`, `generate` produces modules, not a
bootable system (the flake is the deliberate hand-written seam), per ADR 0009. Note that a host
with no resolved baseline refuses (exit 5), pointing at `install`/`upgrade`.

- [ ] **Step 2: docs/01**

Where the generated outputs are described (`generated/*.nix`), add that `generated/flake.nix` is
an optional generated artefact (opt-in via `system {}`), reconciled and hashed like the host
modules.

- [ ] **Step 3: Prose check**

British spelling, no em/en-dashes, no banned vocabulary. Confirm docs/05 and docs/01 agree with
ADR 0009 (opt-in, generated + locked, hand-written seam when absent).

- [ ] **Step 4: No commit.**

---

## Self-Review

- Spec coverage: Task 1 the `system {}` opt-in (`state-version` required, `nixpkgs-url`
  default); Task 2 the pure deterministic emitter (per-host `fetchGit` pin, name-ordered, no
  inputs); Task 3 the gather wiring (baseline precondition refusal, render + format + insert
  into the generated map and lock outputs so it reconciles); Task 4 the docs. Out-of-scope items
  (disko #37, secrets #38, hardware-from-KDL, flake inputs) are excluded.
- Deviation from the spec's test note: the golden lives as a dedicated `tests/system_flake.rs`
  integration fixture (identity formatter, always runs; a real-formatter byte golden can be
  added later), NOT wired into the shared `examples/` project, because `system {}` requires
  every example host to carry a resolved baseline and would churn the five-host golden. Recorded
  here so the reviewer expects it.
- Placeholders: none; Tasks 1-2 give full code, Task 3 gives the exact insertion with the field
  set to confirm against the file, Task 4 states the doc content.
- Type consistency: `SystemConfig`/`ProjectConfig.system` (Task 1) is read in Task 3;
  `FlakeHost`/`render_system_flake` (Task 2) is called in Task 3 with `lock.baselines`'
  `nixpkgs_rev`; the flake joins `generated` + `expected` using the existing `ExpectedFile`
  shape, so `Plan::compute` needs no change.
