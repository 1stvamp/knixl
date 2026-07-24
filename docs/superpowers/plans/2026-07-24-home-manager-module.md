# home-manager module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a declarative `home-manager` module that safely enables home-manager for one user per host, with the "Words of warning" guardrails baked in, plus a golden host proving it.

**Architecture:** A declarative module at `modules/home-manager/knixl-module.kdl` using the shipped `set`/`for-each` grammar with dynamic attr-key interpolation (no Rust). It emits the global integration guardrails once and the per-user config. Ships via the embedded stdlib (#13). A blessed golden host is its contract.

**Tech Stack:** knixl declarative template grammar (KDL), the golden-test harness, nixfmt for the golden.

## Global Constraints

From the spec (`docs/superpowers/specs/2026-07-24-home-manager-module-design.md`) and repo house rules:

- The repo IS rustfmt-normalised; CI runs `cargo fmt --all --check`. Keep the tree fmt-clean (this feature adds no Rust).
- Guardrails, all baked: `home.stateVersion` required (a required prop); `home-manager.useUserPackages = true` and `home-manager.useGlobalPkgs = true` emitted; the module never emits `nix.gc`/`nix.settings` (documented); `home.sessionVariables` supported with the sourcing caveat documented (prose, not a runtime lint — the declarative grammar has none).
- v1 is ONE `home-manager` node per host (the global settings collide otherwise); multi-user and `home.packages` are non-goals.
- Only `programs.<x>.enable` (no rich per-program config), no `home.file`/`xdg.*`, no standalone HM (NixOS-module integration only).
- Determinism: only `set`/`for-each` over KDL source order; no `HashMap`. Dynamic names (`{name}`, `{v.name}`, `{p.name}`) become quoted attr keys.
- `examples/expected/workstation.nix` MUST be real nixfmt output (blessed), not hand-written.
- British spelling in prose/comments; no em/en-dashes; no banned AI-tell vocabulary.
- Implementers: leave changes uncommitted, run no git/`but` command (including `git stash`). The controller commits.

---

### Task 1: The home-manager module and the workstation golden

**Files:**
- Create: `modules/home-manager/knixl-module.kdl`
- Create: `examples/hosts/workstation.kdl`
- Create: `examples/expected/workstation.nix` (blessed with nixfmt, not hand-written)
- Modify: `crates/knixl-pipeline/tests/golden.rs` (three tests)

**Interfaces:**
- Consumes: the golden harness helpers `generate_host`, `assert_host_matches`, `formatter_available` (already in `golden.rs`); the `set`/`for-each` grammar. The module is auto-discovered by the embedded stdlib and the golden harness's `register_stdlib` (it lives under `modules/`), so no registration code.

- [ ] **Step 1: Write the module manifest**

Create `modules/home-manager/knixl-module.kdl`:

```kdl
module name="home-manager" version="1.0.0" {
    summary "home-manager for one user, with safe NixOS-module integration and a required stateVersion. Session variables are sourced only in home-manager-managed shells, not every context (display managers, non-login shells)."
    claims-node "home-manager"

    schema {
        arg "name" type="string" required=#true doc="Login name of the user whose home is managed."
        prop "state-version" type="string" required=#true \
            doc="home.stateVersion, e.g. \"24.11\". Required: home-manager refuses to build without it."
        child "session-var" repeated=#true \
            doc="A home.sessionVariables entry. Sourced only in home-manager-managed shells." {
            arg "name" type="string" required=#true
            arg "value" type="string" required=#true
        }
        child "program" type="string" repeated=#true \
            doc="Enable programs.<name> for the user, e.g. program \"git\"."
    }

    emit {
        set "home-manager.useUserPackages" #true
        set "home-manager.useGlobalPkgs" #true
        set "home-manager.users.{name}.home.stateVersion" "{state-version}"
        for-each "v" in "session-var" {
            set "home-manager.users.{name}.home.sessionVariables.{v.name}" "{v.value}"
        }
        for-each "p" in "program" {
            set "home-manager.users.{name}.programs.{p.name}.enable" #true
        }
    }
}
```

Notes for the implementer:
- `name` is the node label (arg 0), read as `{name}`; `state-version` is a prop, read as `{state-version}`.
- `session-var "EDITOR" "nvim"` is a structured child with two positional args: `{v.name}` = "EDITOR", `{v.value}` = "nvim". `{v.name}` is a dynamic attr-key segment (interpolated into the path).
- `program "git"` is a repeated scalar child; `{p.name}` is its first arg, interpolated into `programs.{p.name}.enable`.
- The `useUserPackages`/`useGlobalPkgs` lines are emitted once per node; this is why v1 is one home-manager node per host (two nodes would emit them twice and Nix rejects the duplicate attribute). Do not try to guard against a second node here; it is a documented v1 limit.
- Do NOT emit any `nix.gc`/`nix.settings` (guardrail).

- [ ] **Step 2: Write the golden host**

Create `examples/hosts/workstation.kdl`:

```kdl
host "workstation" {
    system "x86_64-linux"

    home-manager "wes" state-version="24.11" {
        session-var "EDITOR" "nvim"
        session-var "PAGER" "less"
        program "git"
        program "fish"
    }
}
```

- [ ] **Step 3: Add the tests**

Add to `crates/knixl-pipeline/tests/golden.rs` (mirror the `vmhost_*`/`gateway_*` triplet):

```rust
#[test]
fn workstation_pipeline_produces_expected_structure() {
    let files = generate_host("workstation.kdl");
    assert_eq!(files.len(), 1, "workstation has no side-files");
    let text = &files[0].text;
    for needle in [
        "home-manager.useUserPackages = true",
        "home-manager.useGlobalPkgs = true",
        "home-manager.users.\"wes\"",
        "stateVersion = \"24.11\"",
        "sessionVariables",
        "EDITOR = \"nvim\"",
        "programs",
        "git",
        "enable = true",
    ] {
        assert!(text.contains(needle), "workstation.nix missing `{needle}`\n---\n{text}");
    }
}

#[test]
fn workstation_file_attributes_home_manager() {
    let files = generate_host("workstation.kdl");
    let ws = &files[0];
    for m in ["host", "home-manager"] {
        assert!(
            ws.modules.contains(&m.to_string()),
            "workstation.nix should list {m}, got {:?}",
            ws.modules
        );
    }
}

#[test]
fn workstation_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping workstation_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("workstation.kdl");
}
```

- [ ] **Step 4: Run the structural + attribution tests (identity formatter)**

Run: `cargo test -p knixl-pipeline workstation_pipeline_produces_expected_structure workstation_file_attributes_home_manager`
Expected: both pass. If a needle is missing, fix the module manifest (not the test). Note: `home-manager.users.{name}` may render as a nested `users = { "wes" = {...}; }` block; if the flat needle `home-manager.users."wes"` is not a literal substring, adjust that one needle to match the actual nesting the identity formatter produces (the byte-exact golden in Step 5 is the real contract) — but keep the leaf needles (`stateVersion`, `EDITOR`, `enable = true`).

- [ ] **Step 5: Bless the byte-exact golden**

Same procedure as the disko/gateway/vmhost goldens:

1. Confirm the local formatter reproduces an existing golden:
   `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline nas_matches_golden -- --nocapture`
   Expected PASS. If it FAILS, STOP and report BLOCKED (do not hand-write the expected file).
2. Add a temporary bless test:

```rust
#[test]
#[ignore]
fn bless_workstation() {
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join("workstation.kdl");
    let src = fs::read_to_string(examples.join(&path)).unwrap();
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let files = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");
    fs::write(examples.join("expected/workstation.nix"), &files[0].text).unwrap();
}
```

   (Match the arity of the other `generate(...)` calls in this file; the `secrets_backend` param shipped with #38.)

   Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline bless_workstation -- --ignored --nocapture`
3. Open `examples/expected/workstation.nix` and sanity-check: valid Nix; `home-manager.useUserPackages = true`; `useGlobalPkgs = true`; a `home-manager.users."wes"` config with `home.stateVersion = "24.11"`, `home.sessionVariables` containing `EDITOR`/`PAGER`, and `programs` with `git`/`fish` each `enable = true`; and NO `nix.gc`/`nix.settings`.
4. REMOVE the `bless_workstation` test so it is not in the committed diff.

- [ ] **Step 6: Verify the golden and full suite**

Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline workstation`
Then: `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets`
Expected: all green, `bless_workstation` gone. (A `knixl_nix` test flakes under full parallel runs and is unrelated; if it is the only failure, note it and proceed.)

- [ ] **Step 7: Report** (confirm the bless test was removed)

---

### Task 2: Docs

**Files:**
- Modify: `docs/04-template-grammar.md` (add `home-manager` to the declarative-modules section)

**Interfaces:** none (prose only).

- [ ] **Step 1: Read the declarative-modules section**

Read `docs/04-template-grammar.md`, find the `## Declarative modules shipped with knixl` section (it documents `tailscale` and `incus`), and match its style and depth.

- [ ] **Step 2: Add the home-manager entry**

Add a `### home-manager` subsection documenting:

- `home-manager` claims the `home-manager` node and configures home-manager for one user, used as a NixOS module.
- Node shape: `home-manager "<user>" state-version="<rel>"` with repeated `session-var "<name>" "<value>"` and `program "<name>"` children.
- The baked guardrails: required `state-version` (home.stateVersion); `useUserPackages`/`useGlobalPkgs` set true for safe integration; the module never emits `nix.gc`/`nix.settings` (left to NixOS); and the session-variable caveat (sourced only in home-manager-managed shells, not display managers or non-login shells).
- The v1 limit: one `home-manager` node per host (the global settings would otherwise be emitted twice); multi-user and `home.packages` are follow-ups.
- That the home-manager NixOS module must be imported via the system seam/flake, and `home-manager.*` validates only when the project declares home-manager as an oracle module (#35), like disko/sops.

Keep British spelling, no em/en-dashes, no banned vocabulary; match the neighbouring entries' length.

- [ ] **Step 3: Report**

---

## Notes for the controller

- Base commit before Task 1: the tip of `feat/home-manager-module` (the spec commit). Record it; Task 1's start commit is the BASE for Task 2's review package.
- The module's correctness is its golden: the review should confirm the blessed `workstation.nix` bakes both guardrail settings, requires/sets stateVersion, emits no `nix.*`, and is valid HM-as-NixOS-module config; and that `bless_workstation` left no trace.
- The final whole-branch review should confirm: the guardrails are all present (stateVersion required, useUserPackages/useGlobalPkgs true, no nix.gc); the session-var caveat is documented; only `set`/`for-each` used (determinism); the golden was blessed (not hand-written) and `nas_matches_golden` passed under the same formatter; fmt + clippy clean; workspace green.
- This branch is independent (off `main`), not stacked.
