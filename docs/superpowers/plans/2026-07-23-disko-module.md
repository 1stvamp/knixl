# disko disk-layout module Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a built-in `disko` module that lowers a `disko { }` host node into disko's `disko.devices.{disk,zpool}` config (partitions with filesystem/zfs/swap/luks content, ZFS pools with datasets), plus a `preset` shorthand for the common boot+root+ZFS layout.

**Architecture:** A new built-in in `crates/knixl-modules/src/builtin/disko.rs`, alongside `host`/`postgres`/`backups`. `lower` parses the subtree into internal `Disk`/`Partition`/`Content`/`Pool`/`Dataset` structs, validates them, then emits one `Assignment` per disk and per pool whose value is a fully-built `NixExpr::AttrSet`. The shorthand desugars into the same structs before emit, so it cannot drift from the verbose form. A golden host `vault` exercises every shape end to end.

**Tech Stack:** Rust, the `kdl` crate (v2), `knixl_ir::NixExpr`, `knixl_kdl` readers, `nixfmt` for the golden.

## Global Constraints

Copied from the spec (`docs/superpowers/specs/2026-07-22-disko-module-design.md`) and repo house rules. Every task's requirements implicitly include these:

- The repo IS rustfmt-normalised; CI runs `cargo fmt --all --check`. Run `cargo fmt` before every commit and keep it clean. (The old "do not run cargo fmt" convention is dead.)
- Determinism is load-bearing (the lock depends on it): no `HashMap` on any emit path. `NixExpr::AttrSet` is a `BTreeMap`, so keyed sets emit lexicographically by construction; disks and pools are sorted by label before emit so their order is independent of KDL source order.
- Dynamic names (disk, partition, pool, dataset labels) are `AttrKey::Quoted` so `to_option_key` collapses them to `<name>` for the oracle, exactly as `backups` does. Structural keys (`device`, `type`, `content`, `partitions`, `format`, `mountpoint`, `size`, `pool`, `datasets`, `resumeDevice`) are `AttrKey::Ident`.
- Semantic errors are hard errors, not silent fallbacks: return `Err(LowerError::…)`. Never emit a partial or dangling layout.
- Emit Nix source text, not values (ADR 0002). KDL is authoritative (ADR 0001). No oracle-crate or lock change.
- British spelling in prose/comments. No em-dashes or en-dashes: use commas, parentheses, colons, full stops. Banned vocabulary: passionate, leverage, robust, seamless, delve, and the AI-smell set.
- Doc comments explain why, not what. One logical change per commit; commit messages bottom-line-first, present tense, `type(scope): summary`.
- Implementers: leave changes uncommitted, do not run any git/but command (including `git stash`). The controller commits.

---

### Task 1: The `disko` built-in, verbose form

**Files:**
- Create: `crates/knixl-modules/src/builtin/disko.rs`
- Modify: `crates/knixl-modules/src/builtin/mod.rs` (add `pub mod disko;` and register it)
- Test: unit tests inside `disko.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::{Child, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, ValueTy}`; `crate::builtin::host::unit_default`; `kdl::KdlNode`; `knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr}`; `knixl_kdl::{child_arg_str, children_named, first_arg_str}`.
- Produces: `pub struct Disko` implementing `Module` (node `disko`, id `disko` v`0.1.0`); internal `Disk`/`Partition`/`Content`/`Pool`/`Dataset` (all `#[derive(Debug, Clone, PartialEq)]`); `fn parse_disk`, `fn parse_pool`, `fn emit_disk`, `fn emit_pool`. Task 2 adds a `preset` branch to `parse_disk`; Task 3 relies on node `disko` being registered.

- [ ] **Step 1: Write the module with failing unit tests**

Create `crates/knixl-modules/src/builtin/disko.rs`:

```rust
//! `disko`: declarative disk layout via disko. A built-in, not a declarative module: disko's
//! config is a set of name-keyed attribute sets nested several levels deep with heterogeneous
//! per-partition content, which the single-level template grammar cannot express (see
//! docs/04-template-grammar.md). Each disk and each pool lowers to one assignment whose value
//! is a fully-built attribute set.
use crate::builtin::host::unit_default;
use crate::{
    Child, LowerCtx, LowerError, LowerOutput, Module, ModuleId, NodeSchema, Unit, ValueTy,
};
use kdl::KdlNode;
use knixl_ir::{Assignment, AttrKey, AttrPath, NixExpr};
use knixl_kdl::{child_arg_str, children_named, first_arg_str};
use std::collections::BTreeMap;

const CONTENT_NAMES: &[&str] = &["filesystem", "zfs", "swap", "luks"];

pub struct Disko {
    schema: NodeSchema,
}
impl Disko {
    pub fn new() -> Self {
        Self { schema: schema() }
    }
}
impl Default for Disko {
    fn default() -> Self {
        Self::new()
    }
}

// ---- internal representation: both the verbose parser and (later) the preset expander
// produce these, so the two paths share one emit and cannot drift. ----

#[derive(Debug, Clone, PartialEq)]
enum Content {
    Filesystem {
        format: String,
        mountpoint: Option<String>,
    },
    Zfs {
        pool: String,
    },
    Swap {
        resume: bool,
    },
    Luks {
        name: String,
        inner: Box<Content>,
    },
}

#[derive(Debug, Clone, PartialEq)]
struct Partition {
    name: String,
    size: String,
    type_code: Option<String>,
    content: Content,
}

#[derive(Debug, Clone, PartialEq)]
struct Disk {
    label: String,
    device: String,
    partitions: Vec<Partition>,
}

#[derive(Debug, Clone, PartialEq)]
struct Dataset {
    name: String,
    ty: String,
    mountpoint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Pool {
    label: String,
    mountpoint: Option<String>,
    datasets: Vec<Dataset>,
}

impl Module for Disko {
    fn id(&self) -> ModuleId {
        ModuleId {
            name: "disko".into(),
            version: "0.1.0".parse().unwrap(),
        }
    }
    fn node_name(&self) -> &str {
        "disko"
    }
    fn schema(&self) -> &NodeSchema {
        &self.schema
    }
    fn lower(&self, node: &KdlNode, _ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError> {
        let mut disks = Vec::new();
        for dn in children_named(node, "disk") {
            disks.push(parse_disk(dn)?);
        }
        let mut pools = Vec::new();
        for pn in children_named(node, "zpool") {
            pools.push(parse_pool(pn)?);
        }

        ensure_unique(disks.iter().map(|d| d.label.as_str()), "disk")?;
        ensure_unique(pools.iter().map(|p| p.label.as_str()), "zpool")?;

        // Every `zfs pool="X"` must name a `zpool "X"` declared here: a typo must not emit a
        // vdev pointing at a pool that is never created.
        let declared: std::collections::BTreeSet<&str> =
            pools.iter().map(|p| p.label.as_str()).collect();
        for d in &disks {
            for p in &d.partitions {
                check_pool_refs(&p.content, &declared, &d.label)?;
            }
        }

        // Deterministic emit order regardless of KDL source order.
        disks.sort_by(|a, b| a.label.cmp(&b.label));
        pools.sort_by(|a, b| a.label.cmp(&b.label));

        let mut units: Vec<Unit> = Vec::new();
        for d in &disks {
            units.push(unit_default(emit_disk(d)));
        }
        for p in &pools {
            units.push(unit_default(emit_pool(p)));
        }
        Ok(LowerOutput::units(units))
    }
}

// ---- parsing (verbose) ----

fn parse_disk(n: &KdlNode) -> Result<Disk, LowerError> {
    let label = first_arg_str(n).ok_or_else(|| LowerError::Other("`disk` needs a label".into()))?;
    let device = n
        .get("device")
        .and_then(|v| v.as_string())
        .ok_or_else(|| LowerError::missing(&format!("disk `{label}`.device")))?
        .to_string();
    if n.get("preset").is_some() {
        // The `preset` shorthand lands in a later task. Until then reject it explicitly rather
        // than silently emitting a disk with no partitions.
        return Err(LowerError::Other(
            "`preset` shorthand is not supported yet".into(),
        ));
    }
    let mut partitions = Vec::new();
    for pn in children_named(n, "partition") {
        partitions.push(parse_partition(pn)?);
    }
    ensure_unique(partitions.iter().map(|p| p.name.as_str()), "partition")?;
    Ok(Disk {
        label,
        device,
        partitions,
    })
}

fn parse_partition(n: &KdlNode) -> Result<Partition, LowerError> {
    let name =
        first_arg_str(n).ok_or_else(|| LowerError::Other("`partition` needs a name".into()))?;
    let size = n
        .get("size")
        .and_then(|v| v.as_string())
        .ok_or_else(|| LowerError::missing(&format!("partition `{name}`.size")))?
        .to_string();
    let type_code = n.get("type").and_then(|v| v.as_string()).map(str::to_string);
    let content = parse_content(n, &name)?;
    Ok(Partition {
        name,
        size,
        type_code,
        content,
    })
}

/// The single content child of a partition or luks wrapper. Exactly one of
/// filesystem/zfs/swap/luks; zero or more than one is an error.
fn parse_content(parent: &KdlNode, label: &str) -> Result<Content, LowerError> {
    let contents: Vec<&KdlNode> = parent
        .children()
        .into_iter()
        .flat_map(|d| d.nodes().iter())
        .filter(|n| CONTENT_NAMES.contains(&n.name().value()))
        .collect();
    match contents.as_slice() {
        [c] => content_from(c),
        [] => Err(LowerError::Other(format!(
            "`{label}` needs exactly one content child (filesystem, zfs, swap, or luks)"
        ))),
        _ => Err(LowerError::Other(format!(
            "`{label}` has more than one content child; exactly one is allowed"
        ))),
    }
}

fn content_from(n: &KdlNode) -> Result<Content, LowerError> {
    match n.name().value() {
        "filesystem" => {
            let format = n
                .get("format")
                .and_then(|v| v.as_string())
                .ok_or_else(|| LowerError::missing("filesystem.format"))?
                .to_string();
            let mountpoint = n
                .get("mountpoint")
                .and_then(|v| v.as_string())
                .map(str::to_string);
            Ok(Content::Filesystem { format, mountpoint })
        }
        "zfs" => {
            let pool = n
                .get("pool")
                .and_then(|v| v.as_string())
                .ok_or_else(|| LowerError::missing("zfs.pool"))?
                .to_string();
            Ok(Content::Zfs { pool })
        }
        "swap" => {
            let resume = n.get("resume").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(Content::Swap { resume })
        }
        "luks" => {
            let name = n
                .get("name")
                .and_then(|v| v.as_string())
                .ok_or_else(|| LowerError::missing("luks.name"))?
                .to_string();
            let inner = parse_content(n, "luks")?;
            if matches!(inner, Content::Luks { .. }) {
                return Err(LowerError::Other("luks may not nest inside luks".into()));
            }
            Ok(Content::Luks {
                name,
                inner: Box::new(inner),
            })
        }
        other => Err(LowerError::Other(format!("unknown content `{other}`"))),
    }
}

fn parse_pool(n: &KdlNode) -> Result<Pool, LowerError> {
    let label =
        first_arg_str(n).ok_or_else(|| LowerError::Other("`zpool` needs a label".into()))?;
    let mountpoint = child_arg_str(n, "mountpoint");
    let mut datasets = Vec::new();
    for dn in children_named(n, "dataset") {
        let name =
            first_arg_str(dn).ok_or_else(|| LowerError::Other("`dataset` needs a name".into()))?;
        let ty = dn
            .get("type")
            .and_then(|v| v.as_string())
            .unwrap_or("zfs_fs")
            .to_string();
        let mp = dn
            .get("mountpoint")
            .and_then(|v| v.as_string())
            .map(str::to_string);
        datasets.push(Dataset {
            name,
            ty,
            mountpoint: mp,
        });
    }
    ensure_unique(datasets.iter().map(|d| d.name.as_str()), "dataset")?;
    Ok(Pool {
        label,
        mountpoint,
        datasets,
    })
}

fn ensure_unique<'a>(
    labels: impl Iterator<Item = &'a str>,
    what: &str,
) -> Result<(), LowerError> {
    let mut seen = std::collections::BTreeSet::new();
    for l in labels {
        if !seen.insert(l) {
            return Err(LowerError::Other(format!("duplicate {what} label `{l}`")));
        }
    }
    Ok(())
}

fn check_pool_refs(
    c: &Content,
    declared: &std::collections::BTreeSet<&str>,
    disk: &str,
) -> Result<(), LowerError> {
    match c {
        Content::Zfs { pool } => {
            if !declared.contains(pool.as_str()) {
                return Err(LowerError::Other(format!(
                    "disk `{disk}`: zfs partition references pool `{pool}`, but no `zpool \"{pool}\"` is declared in this disko node"
                )));
            }
            Ok(())
        }
        Content::Luks { inner, .. } => check_pool_refs(inner, declared, disk),
        _ => Ok(()),
    }
}

// ---- emit ----

fn str_expr(s: &str) -> NixExpr {
    NixExpr::Str(s.to_string())
}

fn ident_set(entries: Vec<(&str, NixExpr)>) -> NixExpr {
    let mut m: BTreeMap<AttrKey, NixExpr> = BTreeMap::new();
    for (k, v) in entries {
        m.insert(AttrKey::Ident(k.to_string()), v);
    }
    NixExpr::AttrSet(m)
}

fn quoted_set(entries: Vec<(String, NixExpr)>) -> NixExpr {
    let mut m: BTreeMap<AttrKey, NixExpr> = BTreeMap::new();
    for (k, v) in entries {
        m.insert(AttrKey::Quoted(k), v);
    }
    NixExpr::AttrSet(m)
}

fn emit_content(c: &Content) -> NixExpr {
    match c {
        Content::Filesystem { format, mountpoint } => {
            let mut e = vec![("type", str_expr("filesystem")), ("format", str_expr(format))];
            if let Some(mp) = mountpoint {
                e.push(("mountpoint", str_expr(mp)));
            }
            ident_set(e)
        }
        Content::Zfs { pool } => {
            ident_set(vec![("type", str_expr("zfs")), ("pool", str_expr(pool))])
        }
        Content::Swap { resume } => {
            let mut e = vec![("type", str_expr("swap"))];
            if *resume {
                e.push(("resumeDevice", NixExpr::Bool(true)));
            }
            ident_set(e)
        }
        Content::Luks { name, inner } => ident_set(vec![
            ("type", str_expr("luks")),
            ("name", str_expr(name)),
            ("content", emit_content(inner)),
        ]),
    }
}

fn emit_partition(p: &Partition) -> (String, NixExpr) {
    let mut e = vec![("size", str_expr(&p.size))];
    if let Some(t) = &p.type_code {
        e.push(("type", str_expr(t)));
    }
    e.push(("content", emit_content(&p.content)));
    (p.name.clone(), ident_set(e))
}

fn devices_path(category: &str, label: &str) -> AttrPath {
    AttrPath(vec![
        AttrKey::Ident("disko".into()),
        AttrKey::Ident("devices".into()),
        AttrKey::Ident(category.into()),
        AttrKey::Quoted(label.into()),
    ])
}

fn emit_disk(d: &Disk) -> Assignment {
    let parts: Vec<(String, NixExpr)> = d.partitions.iter().map(emit_partition).collect();
    let content = ident_set(vec![
        ("type", str_expr("gpt")),
        ("partitions", quoted_set(parts)),
    ]);
    let value = ident_set(vec![
        ("device", str_expr(&d.device)),
        ("type", str_expr("disk")),
        ("content", content),
    ]);
    Assignment {
        path: devices_path("disk", &d.label),
        value,
        priority: None,
        condition: None,
        doc: None,
    }
}

fn emit_pool(p: &Pool) -> Assignment {
    let mut e = vec![("type", str_expr("zpool"))];
    if let Some(mp) = &p.mountpoint {
        e.push(("mountpoint", str_expr(mp)));
    }
    if !p.datasets.is_empty() {
        let ds: Vec<(String, NixExpr)> = p
            .datasets
            .iter()
            .map(|d| {
                let mut de = vec![("type", str_expr(&d.ty))];
                if let Some(mp) = &d.mountpoint {
                    de.push(("mountpoint", str_expr(mp)));
                }
                (d.name.clone(), ident_set(de))
            })
            .collect();
        e.push(("datasets", quoted_set(ds)));
    }
    Assignment {
        path: devices_path("zpool", &p.label),
        value: ident_set(e),
        priority: None,
        condition: None,
        doc: None,
    }
}

fn schema() -> NodeSchema {
    NodeSchema {
        summary: "Declarative disk layout via disko: disks, partitions, and ZFS pools.".into(),
        args: vec![],
        props: vec![],
        children: vec![
            node_child("disk", "A physical disk with a GPT partition table. Repeatable."),
            node_child("zpool", "A ZFS pool created on this host. Repeatable."),
        ],
        open_children: false,
    }
}

fn node_child(name: &str, doc: &str) -> Child {
    Child {
        name: name.into(),
        ty: ValueTy::Node,
        required: false,
        repeated: true,
        delegate: false,
        doc: doc.into(),
        args: vec![],
        props: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Registry, Scope};

    fn node(src: &str) -> KdlNode {
        src.parse::<kdl::KdlDocument>()
            .unwrap()
            .nodes()
            .first()
            .unwrap()
            .clone()
    }

    fn lower_ok(src: &str) -> Vec<Unit> {
        let d = Disko::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "vault".into() }, &reg, &mut diags, vec![]);
        d.lower(&node(src), &mut ctx).expect("lower ok").units
    }

    fn lower_err(src: &str) -> String {
        let d = Disko::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(Scope { host: "vault".into() }, &reg, &mut diags, vec![]);
        format!("{}", d.lower(&node(src), &mut ctx).expect_err("lower err"))
    }

    // Reach into one disk's partitions attrset by walking the emitted value.
    fn partitions_of<'a>(units: &'a [Unit], disk: &str) -> &'a BTreeMap<AttrKey, NixExpr> {
        let a = units
            .iter()
            .map(|u| &u.assignment)
            .find(|a| a.path.0.last() == Some(&AttrKey::Quoted(disk.to_string())))
            .expect("disk assignment");
        let NixExpr::AttrSet(disk_set) = &a.value else {
            panic!("disk value is not an attrset")
        };
        let NixExpr::AttrSet(content) = disk_set.get(&AttrKey::Ident("content".into())).unwrap()
        else {
            panic!("content not a set")
        };
        let NixExpr::AttrSet(parts) =
            content.get(&AttrKey::Ident("partitions".into())).unwrap()
        else {
            panic!("partitions not a set")
        };
        parts
    }

    #[test]
    fn each_content_form_lowers() {
        let units = lower_ok(
            "disko {\n\
             \x20   disk \"main\" device=\"/dev/sda\" {\n\
             \x20       partition \"ESP\" size=\"512M\" type=\"EF00\" { filesystem format=\"vfat\" mountpoint=\"/boot\" }\n\
             \x20       partition \"swap\" size=\"8G\" { swap }\n\
             \x20       partition \"crypt\" size=\"100G\" { luks name=\"cryptroot\" { filesystem format=\"ext4\" mountpoint=\"/\" } }\n\
             \x20       partition \"data\" size=\"100%\" { zfs pool=\"tank\" }\n\
             \x20   }\n\
             \x20   zpool \"tank\" { mountpoint \"/tank\" dataset \"media\" mountpoint=\"/tank/media\" }\n\
             }",
        );
        // one disk assignment + one pool assignment
        assert_eq!(units.len(), 2);
        let parts = partitions_of(&units, "main");
        for name in ["ESP", "swap", "crypt", "data"] {
            assert!(parts.contains_key(&AttrKey::Quoted(name.into())), "missing {name}");
        }
        // swap without resume has no resumeDevice
        let NixExpr::AttrSet(swap) = parts.get(&AttrKey::Quoted("swap".into())).unwrap() else {
            panic!()
        };
        let NixExpr::AttrSet(swap_content) =
            swap.get(&AttrKey::Ident("content".into())).unwrap()
        else {
            panic!()
        };
        assert!(!swap_content.contains_key(&AttrKey::Ident("resumeDevice".into())));
        // NixExpr does not implement PartialEq (it has an f64 variant), so assert with
        // matches!/pattern binding throughout these tests rather than `==`.
    }

    #[test]
    fn swap_with_resume_emits_resume_device() {
        let units = lower_ok(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"s\" size=\"1G\" { swap resume=#true } } }",
        );
        let parts = partitions_of(&units, "m");
        let NixExpr::AttrSet(s) = parts.get(&AttrKey::Quoted("s".into())).unwrap() else {
            panic!()
        };
        let NixExpr::AttrSet(content) = s.get(&AttrKey::Ident("content".into())).unwrap() else {
            panic!()
        };
        assert!(matches!(
            content.get(&AttrKey::Ident("resumeDevice".into())),
            Some(NixExpr::Bool(true))
        ));
    }

    #[test]
    fn pool_and_dataset_emit() {
        let units = lower_ok(
            "disko { zpool \"tank\" { mountpoint \"/tank\" dataset \"media\" mountpoint=\"/tank/media\" } }",
        );
        assert_eq!(units.len(), 1);
        let a = &units[0].assignment;
        assert_eq!(a.path.0.last(), Some(&AttrKey::Quoted("tank".into())));
        let NixExpr::AttrSet(pool) = &a.value else { panic!() };
        assert!(matches!(
            pool.get(&AttrKey::Ident("type".into())),
            Some(NixExpr::Str(s)) if s == "zpool"
        ));
        assert!(pool.contains_key(&AttrKey::Ident("datasets".into())));
    }

    #[test]
    fn missing_device_errors() {
        assert!(lower_err("disko { disk \"m\" { } }").contains("device"));
    }

    #[test]
    fn zero_content_errors() {
        let e = lower_err("disko { disk \"m\" device=\"/dev/sda\" { partition \"p\" size=\"1G\" { } } }");
        assert!(e.contains("exactly one content"));
    }

    #[test]
    fn two_content_errors() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"p\" size=\"1G\" { swap\n filesystem format=\"ext4\" } } }",
        );
        assert!(e.contains("more than one content"));
    }

    #[test]
    fn dangling_pool_errors() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"d\" size=\"100%\" { zfs pool=\"nope\" } } }",
        );
        assert!(e.contains("nope"));
    }

    #[test]
    fn duplicate_partition_label_errors() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"p\" size=\"1G\" { swap }\n partition \"p\" size=\"2G\" { swap } } }",
        );
        assert!(e.contains("duplicate partition"));
    }

    #[test]
    fn luks_inner_arity_enforced() {
        let zero = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"c\" size=\"1G\" { luks name=\"x\" { } } } }",
        );
        assert!(zero.contains("exactly one content"));
    }
}
```

- [ ] **Step 2: Register the module**

Edit `crates/knixl-modules/src/builtin/mod.rs`. Add the module declaration alongside the others (keep alphabetical-ish with the existing list):

```rust
pub mod backups;
pub mod disko;
pub mod host;
pub mod package;
pub mod postgres;
pub mod raw_nix;
```

And inside `register_builtins`, add:

```rust
    let _ = reg.register(Box::new(disko::Disko::new()));
```

- [ ] **Step 3: Run the tests, expecting the emit/parse tests to pass**

Run: `cargo test -p knixl-modules disko`
Expected: all `disko::tests::*` pass. If a test fails, fix the implementation (not the test), then re-run.

- [ ] **Step 4: fmt + clippy**

Run: `cargo fmt --all && cargo fmt --all --check && cargo clippy -p knixl-modules --all-targets`
Expected: no diff, no warnings.

- [ ] **Step 5: Report** (implementer writes the report file; controller commits)

---

### Task 2: `preset` shorthand

**Files:**
- Modify: `crates/knixl-modules/src/builtin/disko.rs` (replace the preset-reject stub in `parse_disk`, add `fn expand_preset`, add tests)

**Interfaces:**
- Consumes: Task 1's `Disk`/`Partition`/`Content` structs and `parse_disk`.
- Produces: `fn expand_preset(n: &KdlNode, label: String, device: String, preset: &str) -> Result<Disk, LowerError>`; the desugared disk is emitted by Task 1's `emit_disk` unchanged.

- [ ] **Step 1: Write the failing tests**

Add to `disko.rs`'s `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn preset_expands_to_boot_root_zfs() {
        // The preset disk and its hand-written verbose equivalent must parse to the same Disk,
        // so the sugar can never drift from the desugared form.
        let preset = node(
            "disk \"main\" device=\"/dev/nvme0n1\" preset=\"boot-root-zfs\" pool=\"tank\" root-size=\"100G\"",
        );
        let verbose = node(
            "disk \"main\" device=\"/dev/nvme0n1\" {\n\
             \x20   partition \"ESP\" size=\"512M\" type=\"EF00\" { filesystem format=\"vfat\" mountpoint=\"/boot\" }\n\
             \x20   partition \"root\" size=\"100G\" { filesystem format=\"ext4\" mountpoint=\"/\" }\n\
             \x20   partition \"data\" size=\"100%\" { zfs pool=\"tank\" }\n\
             }",
        );
        assert_eq!(
            super::parse_disk(&preset).unwrap(),
            super::parse_disk(&verbose).unwrap()
        );
    }

    #[test]
    fn preset_boot_size_override() {
        let disk = super::parse_disk(&node(
            "disk \"m\" device=\"/dev/sda\" preset=\"boot-root-zfs\" pool=\"tank\" root-size=\"50G\" boot-size=\"1G\"",
        ))
        .unwrap();
        assert_eq!(disk.partitions[0].name, "ESP");
        assert_eq!(disk.partitions[0].size, "1G");
    }

    #[test]
    fn preset_with_explicit_partition_errors() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" preset=\"boot-root-zfs\" pool=\"tank\" root-size=\"10G\" { partition \"x\" size=\"1G\" { swap } } zpool \"tank\" { } }",
        );
        assert!(e.contains("mutually exclusive"));
    }

    #[test]
    fn preset_unknown_value_errors() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" preset=\"raid\" pool=\"tank\" root-size=\"10G\" zpool \"tank\" { } }",
        );
        assert!(e.contains("unknown preset"));
    }

    #[test]
    fn preset_requires_pool_and_root_size() {
        assert!(lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" preset=\"boot-root-zfs\" root-size=\"10G\" }"
        )
        .contains("pool"));
        assert!(lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" preset=\"boot-root-zfs\" pool=\"tank\" zpool \"tank\" { } }"
        )
        .contains("root-size"));
    }
```

Note: `parse_disk` and `expand_preset` are module-private; the tests refer to them as `super::parse_disk`. If Rust visibility complains, mark `fn parse_disk` and `fn expand_preset` as `pub(super)` (they are already in the same module as the tests, so `super::` resolves without a visibility change; keep them private).

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p knixl-modules disko::tests::preset`
Expected: FAIL (the stub returns "not supported yet").

- [ ] **Step 3: Replace the stub with expansion**

In `parse_disk`, replace this block:

```rust
    if n.get("preset").is_some() {
        // The `preset` shorthand lands in a later task. Until then reject it explicitly rather
        // than silently emitting a disk with no partitions.
        return Err(LowerError::Other(
            "`preset` shorthand is not supported yet".into(),
        ));
    }
```

with:

```rust
    if let Some(preset) = n.get("preset").and_then(|v| v.as_string()) {
        return expand_preset(n, label, device, preset);
    }
```

Add the expander below `parse_disk`:

```rust
/// Desugar `preset="boot-root-zfs"` into the standard OS-plus-data layout: a sized ESP, a sized
/// ext4 root, and a ZFS vdev taking the remainder. Pure sugar: it returns the same `Disk` a
/// verbose block would, so both go through one emit path.
fn expand_preset(
    n: &KdlNode,
    label: String,
    device: String,
    preset: &str,
) -> Result<Disk, LowerError> {
    if children_named(n, "partition").next().is_some() {
        return Err(LowerError::Other(format!(
            "disk `{label}`: `preset` and explicit `partition` children are mutually exclusive"
        )));
    }
    if preset != "boot-root-zfs" {
        return Err(LowerError::Other(format!(
            "disk `{label}`: unknown preset `{preset}` (only `boot-root-zfs`)"
        )));
    }
    let pool = n
        .get("pool")
        .and_then(|v| v.as_string())
        .ok_or_else(|| {
            LowerError::Other(format!(
                "disk `{label}`: preset `boot-root-zfs` requires `pool`"
            ))
        })?
        .to_string();
    let root_size = n
        .get("root-size")
        .and_then(|v| v.as_string())
        .ok_or_else(|| {
            LowerError::Other(format!(
                "disk `{label}`: preset `boot-root-zfs` requires `root-size`"
            ))
        })?
        .to_string();
    let boot_size = n
        .get("boot-size")
        .and_then(|v| v.as_string())
        .unwrap_or("512M")
        .to_string();
    let partitions = vec![
        Partition {
            name: "ESP".into(),
            size: boot_size,
            type_code: Some("EF00".into()),
            content: Content::Filesystem {
                format: "vfat".into(),
                mountpoint: Some("/boot".into()),
            },
        },
        Partition {
            name: "root".into(),
            size: root_size,
            type_code: None,
            content: Content::Filesystem {
                format: "ext4".into(),
                mountpoint: Some("/".into()),
            },
        },
        Partition {
            name: "data".into(),
            size: "100%".into(),
            type_code: None,
            content: Content::Zfs { pool },
        },
    ];
    Ok(Disk {
        label,
        device,
        partitions,
    })
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p knixl-modules disko`
Expected: all pass (Task 1 tests plus the new preset tests).

- [ ] **Step 5: fmt + clippy**

Run: `cargo fmt --all && cargo fmt --all --check && cargo clippy -p knixl-modules --all-targets`
Expected: clean.

- [ ] **Step 6: Report**

---

### Task 3: Golden host `vault`

**Files:**
- Create: `examples/hosts/vault.kdl`
- Create: `examples/expected/vault.nix` (generated with the real formatter, committed; not hand-written)
- Modify: `crates/knixl-pipeline/tests/golden.rs` (add three tests)

**Interfaces:**
- Consumes: node `disko` registered in Task 1; the existing golden harness helpers `generate_host`, `assert_host_matches`, `formatter_available`.

- [ ] **Step 1: Write the host input**

Create `examples/hosts/vault.kdl`:

```kdl
host "vault" {
    system "x86_64-linux"

    disko {
        disk "main" device="/dev/nvme0n1" {
            partition "ESP" size="512M" type="EF00" {
                filesystem format="vfat" mountpoint="/boot"
            }
            partition "swap" size="8G" {
                swap
            }
            partition "crypt" size="100G" {
                luks name="cryptroot" {
                    filesystem format="ext4" mountpoint="/"
                }
            }
            partition "data" size="100%" {
                zfs pool="tank"
            }
        }
        zpool "tank" {
            mountpoint "/tank"
            dataset "media" mountpoint="/tank/media"
        }
    }
}
```

- [ ] **Step 2: Add the tests (structural + attribution + byte-exact)**

Add to `crates/knixl-pipeline/tests/golden.rs` (mirror the `nas_*` tests; `formatter_available` already exists in this file, used by the other `*_matches_golden` tests):

```rust
#[test]
fn vault_pipeline_produces_expected_structure() {
    let files = generate_host("vault.kdl");
    assert_eq!(files.len(), 1, "vault has no side-files");
    let text = &files[0].text;
    // Distinguishing leaf fragments; the byte-exact form is nailed by vault_matches_golden.
    for needle in [
        "/dev/nvme0n1",
        "\"disk\"",
        "\"gpt\"",
        "\"ESP\"",
        "\"crypt\"",
        "\"data\"",
        "\"swap\"",
        "\"EF00\"",
        "\"filesystem\"",
        "\"vfat\"",
        "\"luks\"",
        "\"cryptroot\"",
        "\"zfs\"",
        "pool = \"tank\"",
        "\"zpool\"",
        "datasets",
        "\"zfs_fs\"",
    ] {
        assert!(text.contains(needle), "vault.nix missing `{needle}`\n---\n{text}");
    }
}

#[test]
fn vault_file_attributes_disko() {
    let files = generate_host("vault.kdl");
    let vault = &files[0];
    for m in ["host", "disko"] {
        assert!(
            vault.modules.contains(&m.to_string()),
            "vault.nix should list {m}, got {:?}",
            vault.modules
        );
    }
}

#[test]
fn vault_matches_golden() {
    if !formatter_available() {
        eprintln!("skipping vault_matches_golden: no formatter (set KNIXL_FORMATTER)");
        return;
    }
    assert_host_matches("vault.kdl");
}
```

Confirm `formatter_available` exists in this file; the `nas_matches_golden` test uses it. If its exact name differs, match whatever `nas_matches_golden` calls.

- [ ] **Step 3: Run the structural test (identity formatter, no nixfmt needed)**

Run: `cargo test -p knixl-pipeline vault_pipeline_produces_expected_structure vault_file_attributes_disko`
Expected: both pass. Fix emit if a needle is missing.

- [ ] **Step 4: Bless the byte-exact golden**

`examples/expected/vault.nix` must be the real nixfmt output, not hand-written. First confirm the local formatter matches the committed goldens, then produce `vault.nix` with a temporary bless test:

1. Sanity-check the formatter reproduces an existing golden:
   `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline nas_matches_golden -- --nocapture`
   Expected: PASS (proves this `nixfmt` matches what produced the committed goldens). If it FAILS, stop and report: the local formatter differs from the pinned one and the golden cannot be blessed here.

2. Add a temporary bless test to `golden.rs`:

```rust
#[test]
#[ignore]
fn bless_vault() {
    let files = {
        let examples = examples_dir();
        let path = PathBuf::from("hosts").join("vault.kdl");
        let src = fs::read_to_string(examples.join(&path)).unwrap();
        let tool = "0.3.1".parse().unwrap();
        let no_pins = std::collections::BTreeMap::new();
        let no_oracles = std::collections::BTreeMap::new();
        generate(
            &[HostSource { path, src }],
            &build_registry(),
            &formatter(),
            &tool,
            &no_oracles,
            &no_pins,
        )
        .expect("generate")
    };
    fs::write(
        examples_dir().join("expected/vault.nix"),
        &files[0].text,
    )
    .unwrap();
}
```

   Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline bless_vault -- --ignored --nocapture`
   Then open `examples/expected/vault.nix` and sanity-check it: valid-looking Nix, `disko.devices.disk."main"`, the four partitions (lexicographic: ESP, crypt, data, swap), the luks-wrapped ext4 root, and `disko.devices.zpool."tank"` with `datasets."media"`.

3. Remove the `bless_vault` test.

- [ ] **Step 5: Verify the golden test passes**

Run: `KNIXL_FORMATTER=$(command -v nixfmt) cargo test -p knixl-pipeline vault`
Expected: `vault_pipeline_produces_expected_structure`, `vault_file_attributes_disko`, and `vault_matches_golden` all pass.

- [ ] **Step 6: Full suite + fmt + clippy**

Run: `cargo test --workspace && cargo fmt --all --check && cargo clippy --workspace --all-targets`
Expected: green, no diff, no warnings.

- [ ] **Step 7: Report**

---

### Task 4: Docs

**Files:**
- Modify: `docs/04-template-grammar.md` (add disko to the built-in module reference)

**Interfaces:** none (prose only).

- [ ] **Step 1: Read the current built-in module section**

Read `docs/04-template-grammar.md` and find where built-in modules (host, postgres, backups, package, raw-nix) are listed or described. Match that section's existing structure and heading style.

- [ ] **Step 2: Add the disko entry**

Document, in the same style as the neighbouring built-ins:

- `disko` claims the `disko` node; it is a built-in because disko's config is name-keyed attribute sets nested several levels deep with heterogeneous per-partition content, which the declarative template grammar (single-level) cannot express.
- The node shape: `disk "<label>" device="<path>"` holding `partition "<name>" size="<size>" [type="<code>"]` children, each with exactly one content child (`filesystem format= mountpoint=`, `zfs pool=`, `swap [resume=#true]`, or `luks name=` wrapping one inner content); and `zpool "<label>"` holding an optional `mountpoint` and `dataset "<name>" [type=] [mountpoint=]` children.
- The `preset="boot-root-zfs" pool= root-size= [boot-size=]` shorthand, and that it is pure sugar for the ESP + ext4 root + ZFS-vdev-at-100% layout.
- A `zfs pool="X"` must name a `zpool "X"` declared in the same node.
- `disko.*` paths are validated only when the project declares disko as an out-of-tree oracle module (#35); otherwise they are unchecked.

Keep to British spelling, no em/en-dashes, no banned vocabulary.

- [ ] **Step 3: Report**

---

## Notes for the controller

- Base commit before Task 1: the current tip of `feat/disko-module` (the spec commit `690c8c7`). Record it; use it as the review-package BASE for Task 1, and each task's own start commit as the BASE for the next.
- Tasks 1 and 2 both live in `disko.rs`: review Task 1 (verbose emit + core validation) independently of Task 2 (shorthand). Task 2's key deliverable is the no-drift equality test.
- The final whole-branch review should confirm: no `HashMap` on the emit path; dynamic labels are `AttrKey::Quoted`; the byte-exact golden was blessed with the real formatter (not hand-written) and `nas_matches_golden` passed under the same formatter; `cargo fmt --all --check` and `cargo clippy --workspace --all-targets` clean; full workspace suite green.
