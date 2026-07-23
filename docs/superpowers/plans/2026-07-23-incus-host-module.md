# incus-host module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a declarative `incus` module that generates `virtualisation.incus.enable`, the web UI toggle, and the daemon `preseed` (storage pools, networks, a default profile), plus a golden host proving it.

**Architecture:** A declarative module at `modules/incus/knixl-module.kdl` using the existing `set`/`when-flag`/`list` grammar (no Rust). #34's `list` statement emits the preseed lists-of-attrsets. VM-support qemu and the incus-admin group are out of the module (companion `package`/`user` nodes), documented only.

**Tech Stack:** knixl declarative template grammar (KDL), the golden-test harness, nixfmt for the golden.

## Global Constraints

From the spec (`docs/superpowers/specs/2026-07-23-incus-host-module-design.md`) and repo house rules. Every task implicitly includes these:

- The repo IS rustfmt-normalised; CI runs `cargo fmt --all --check`. Run `cargo fmt` before committing (this feature adds no Rust, but keep the tree fmt-clean).
- The module owns only `virtualisation.incus.{enable, ui.enable, preseed.*}`. It does NOT emit qemu or the incus-admin group; those are the companion `package "qemu"` and `user` nodes, documented not emitted.
- `virtualisation.incus.ui.enable` is the verified-correct option for the UI (confirmed against the oracle's cached NixOS options).
- Network config for v1 is `ipv4` (`config."ipv4.address"`) and `nat` (`config."ipv4.nat"`), both required props. Profiles emit the fixed default shape: a `root` disk on `pool` and an `eth0` nic on `network`. No ipv6, no variable devices (grammar is single-level).
- `examples/expected/vmhost.nix` MUST be real nixfmt output (blessed), not hand-written.
- Determinism: only `set`/`when-flag`/`list` over KDL source order; no `HashMap`. Output a pure function of input.
- British spelling in prose/comments; no em/en-dashes; no banned AI-tell vocabulary.
- Implementers: leave changes uncommitted, run no git/`but` command (including `git stash`). The controller commits.

---

### Task 1: The incus module and the vmhost golden

**Files:**
- Create: `modules/incus/knixl-module.kdl`
- Create: `examples/hosts/vmhost.kdl`
- Create: `examples/expected/vmhost.nix` (blessed with nixfmt, not hand-written)
- Modify: `crates/knixl-pipeline/tests/golden.rs` (three tests)

**Interfaces:**
- Consumes: the golden harness helpers `generate_host`, `assert_host_matches`, `formatter_available`, `build_registry`, `formatter`, `examples_dir` (already in `golden.rs`); the `set`/`when-flag`/`list` grammar (already shipped).
- Produces: node `incus` in the auto-discovered registry (`build_registry` walks `modules/*/knixl-module.kdl`, so no registration code).

- [ ] **Step 1: Write the module manifest**

Create `modules/incus/knixl-module.kdl`:

```kdl
module name="incus" version="1.0.0" {
    summary "An Incus host: enable, the web UI, and the daemon preseed (storage pools, networks, a default profile)."
    claims-node "incus"

    schema {
        child "ui" type="bool" \
            doc="Enable the Incus web UI (virtualisation.incus.ui.enable)."
        child "storage-pool" repeated=#true \
            doc="A preseed storage pool. The label is the pool name." {
            arg "name" type="string" required=#true
            prop "driver" type="string" required=#true
            prop "source" type="string" required=#true
        }
        child "network" repeated=#true \
            doc="A preseed network. The label is the network name." {
            arg "name" type="string" required=#true
            prop "type" type="string" required=#true
            prop "ipv4" type="string" required=#true
            prop "nat" type="string" required=#true
        }
        child "profile" repeated=#true \
            doc="A preseed profile with a root disk on `pool` and an eth0 nic on `network`." {
            arg "name" type="string" required=#true
            prop "pool" type="string" required=#true
            prop "network" type="string" required=#true
        }
    }

    emit {
        set "virtualisation.incus.enable" #true
        when-flag "ui" {
            set "virtualisation.incus.ui.enable" #true
        }
        list "virtualisation.incus.preseed.storage_pools" from "storage-pool" {
            set "name" "{storage-pool.name}"
            set "driver" "{storage-pool.driver}"
            set "config.source" "{storage-pool.source}"
        }
        list "virtualisation.incus.preseed.networks" from "network" {
            set "name" "{network.name}"
            set "type" "{network.type}"
            set "config.\"ipv4.address\"" "{network.ipv4}"
            set "config.\"ipv4.nat\"" "{network.nat}"
        }
        list "virtualisation.incus.preseed.profiles" from "profile" {
            set "name" "{profile.name}"
            set "devices.root.path" "/"
            set "devices.root.pool" "{profile.pool}"
            set "devices.root.type" "disk"
            set "devices.eth0.name" "eth0"
            set "devices.eth0.network" "{profile.network}"
            set "devices.eth0.type" "nic"
        }
    }
}
```

Notes for the implementer:
- The `name` is a schema `arg` (the node label), so `storage-pool "default"` binds `{storage-pool.name}` to `"default"`. `driver`/`source`/`type`/`ipv4`/`nat`/`pool`/`network` are `prop`s (key=value on the node).
- `config."ipv4.address"` in the path is written with escaped quotes in KDL: `set "config.\"ipv4.address\"" "{network.ipv4}"`. This is the same shape the shipped `list` tests use.
- `ui` is a bool flag child, gated with `when-flag "ui"` (present -> true), matching the `openssh` `x11-forwarding` idiom.

- [ ] **Step 2: Write the golden host**

Create `examples/hosts/vmhost.kdl`:

```kdl
host "vmhost" {
    system "x86_64-linux"

    incus {
        ui
        storage-pool "default" driver="zfs" source="rpool/incus"
        network "incusbr0" type="bridge" ipv4="auto" nat="true"
        profile "default" pool="default" network="incusbr0"
    }
}
```

- [ ] **Step 3: Add the tests**

Add to `crates/knixl-pipeline/tests/golden.rs` (mirror the `gateway_*`/`nas_*` tests):

```rust
#[test]
fn vmhost_pipeline_produces_expected_structure() {
    let files = generate_host("vmhost.kdl");
    assert_eq!(files.len(), 1, "vmhost has no side-files");
    let text = &files[0].text;
    for needle in [
        "virtualisation.incus.enable = true",
        "virtualisation.incus.ui.enable = true",
        "virtualisation.incus.preseed.storage_pools",
        "driver = \"zfs\"",
        "source = \"rpool/incus\"",
        "virtualisation.incus.preseed.networks",
        "\"ipv4.address\" = \"auto\"",
        "\"ipv4.nat\" = \"true\"",
        "virtualisation.incus.preseed.profiles",
        "type = \"disk\"",
        "type = \"nic\"",
        "network = \"incusbr0\"",
    ] {
        assert!(text.contains(needle), "vmhost.nix missing `{needle}`\n---\n{text}");
    }
}

#[test]
fn vmhost_file_attributes_incus() {
    let files = generate_host("vmhost.kdl");
    let vm = &files[0];
    for m in ["host", "incus"] {
        assert!(
            vm.modules.contains(&m.to_string()),
            "vmhost.nix should list {m}, got {:?}",
            vm.modules
        );
    }
}

#[test]
fn vmhost_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping vmhost_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("vmhost.kdl");
}
```

- [ ] **Step 4: Run the structural + attribution tests (identity formatter)**

Run: `cargo test -p knixl-pipeline vmhost_pipeline_produces_expected_structure vmhost_file_attributes_incus`
Expected: both pass. If a needle is missing, fix the module manifest (not the test).

- [ ] **Step 5: Bless the byte-exact golden**

Same procedure as the disko/gateway goldens:

1. Confirm the local formatter reproduces an existing golden:
   `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline nas_matches_golden -- --nocapture`
   Expected PASS. If it FAILS, STOP and report BLOCKED (local formatter differs from the pinned one; do not hand-write the expected file).
2. Add a temporary bless test:

```rust
#[test]
#[ignore]
fn bless_vmhost() {
    let examples = examples_dir();
    let path = PathBuf::from("hosts").join("vmhost.kdl");
    let src = fs::read_to_string(examples.join(&path)).unwrap();
    let tool = "0.3.1".parse().unwrap();
    let no_pins = std::collections::BTreeMap::new();
    let no_oracles = std::collections::BTreeMap::new();
    let real = generate(
        &[HostSource { path, src }],
        &build_registry(),
        &formatter(),
        &tool,
        &no_oracles,
        &no_pins,
        knixl_modules::SecretsBackend::default(),
    )
    .expect("generate");
    fs::write(examples.join("expected/vmhost.nix"), &real[0].text).unwrap();
}
```

   (The `generate` call takes the `secrets_backend` param that shipped with #38; pass `knixl_modules::SecretsBackend::default()`. If the arity differs in the current tree, match the signature of the other `generate(...)` calls in this file.)

   Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline bless_vmhost -- --ignored --nocapture`
3. Open `examples/expected/vmhost.nix` and sanity-check: valid Nix; `virtualisation.incus.enable = true`; `ui.enable = true`; `preseed.storage_pools` with `driver = "zfs"` and `config = { source = "rpool/incus"; }`; `preseed.networks` with `"ipv4.address" = "auto"` and `"ipv4.nat" = "true"`; `preseed.profiles` with a `root` disk and an `eth0` nic.
4. REMOVE the `bless_vmhost` test so it is not in the committed diff.

- [ ] **Step 6: Verify the golden and full suite**

Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline vmhost`
Then: `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets`
Expected: all green, `bless_vmhost` gone. (A `knixl_nix` test flakes under full parallel runs and is unrelated; if it is the only failure, note it and proceed.)

- [ ] **Step 7: Report** (confirm the bless test was removed)

---

### Task 2: Docs

**Files:**
- Modify: `docs/04-template-grammar.md` (add `incus` to the declarative-modules section)

**Interfaces:** none (prose only).

- [ ] **Step 1: Read the declarative-modules section**

Read `docs/04-template-grammar.md`, find the `## Declarative modules shipped with knixl` section (it currently documents `tailscale`), and match its style and depth.

- [ ] **Step 2: Add the incus entry**

Add an `### incus` subsection documenting:

- `incus` claims the `incus` node and generates an Incus host: `virtualisation.incus.enable`, the web UI via a `ui` flag (`virtualisation.incus.ui.enable`), and the daemon `preseed`.
- Node shape: repeated `storage-pool "<name>" driver= source=`, `network "<name>" type= ipv4= nat=`, and `profile "<name>" pool= network=` children. A profile emits the default shape (a root disk on `pool` and an eth0 nic on `network`).
- The companion pattern for the parts outside the module: VM support via a host-level `package "qemu"`, and the admin via the `user` module (`user "wes" { group "incus-admin" }`). The incus module deliberately does not emit these.
- A one-line example, e.g. the `vmhost` node shape.

Keep British spelling, no em/en-dashes, no banned vocabulary. Do not over-write; match the `tailscale` entry's length.

- [ ] **Step 3: Report**

---

## Notes for the controller

- Base commit before Task 1: the tip of `feat/incus-module` (the spec commit). Record it; Task 1's start commit is the BASE for Task 2's review package.
- The module's correctness is its golden: the review should confirm the blessed `vmhost.nix` is valid, nixfmt-shaped, and matches the emit shape (preseed lists, nested `config`/`devices` maps, quoted `"ipv4.*"` keys), and that `bless_vmhost` left no trace.
- The final whole-branch review should confirm: the module owns only `virtualisation.incus.*` (no qemu/group emitted); `ui.enable` is the option used; the golden was blessed (not hand-written) and `nas_matches_golden` passed under the same formatter; determinism (only set/when-flag/list); fmt + clippy clean; workspace suite green.
- This branch is independent (off `main`), not stacked.
