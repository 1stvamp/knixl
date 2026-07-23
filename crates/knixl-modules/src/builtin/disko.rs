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
    if let Some(preset) = n.get("preset").and_then(|v| v.as_string()) {
        return expand_preset(n, label, device, preset);
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

fn parse_partition(n: &KdlNode) -> Result<Partition, LowerError> {
    let name =
        first_arg_str(n).ok_or_else(|| LowerError::Other("`partition` needs a name".into()))?;
    let size = n
        .get("size")
        .and_then(|v| v.as_string())
        .ok_or_else(|| LowerError::missing(&format!("partition `{name}`.size")))?
        .to_string();
    let type_code = n
        .get("type")
        .and_then(|v| v.as_string())
        .map(str::to_string);
    let content = parse_content(n, &name)?;
    Ok(Partition {
        name,
        size,
        type_code,
        content,
    })
}

/// The single content child of a partition or luks wrapper. Exactly one of
/// filesystem/zfs/swap/luks; zero or more than one is an error. A partition or luks node has no
/// other legitimate children, so any child not in `CONTENT_NAMES` is a typo, not something to
/// silently drop.
fn parse_content(parent: &KdlNode, label: &str) -> Result<Content, LowerError> {
    let children: Vec<&KdlNode> = parent
        .children()
        .into_iter()
        .flat_map(|d| d.nodes().iter())
        .collect();
    if let Some(unknown) = children
        .iter()
        .find(|n| !CONTENT_NAMES.contains(&n.name().value()))
    {
        return Err(LowerError::Other(format!(
            "`{label}` has unknown content child `{}`; expected one of filesystem, zfs, swap, luks",
            unknown.name().value()
        )));
    }
    let contents: Vec<&KdlNode> = children
        .into_iter()
        .filter(|n| CONTENT_NAMES.contains(&n.name().value()))
        .collect();
    match contents.as_slice() {
        [c] => content_from(c, label),
        [] => Err(LowerError::Other(format!(
            "`{label}` needs exactly one content child (filesystem, zfs, swap, or luks)"
        ))),
        _ => Err(LowerError::Other(format!(
            "`{label}` has more than one content child; exactly one is allowed"
        ))),
    }
}

fn content_from(n: &KdlNode, label: &str) -> Result<Content, LowerError> {
    match n.name().value() {
        "filesystem" => {
            let format = n
                .get("format")
                .and_then(|v| v.as_string())
                .ok_or_else(|| LowerError::missing(&format!("`{label}`.filesystem.format")))?
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
                .ok_or_else(|| LowerError::missing(&format!("`{label}`.zfs.pool")))?
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
                .ok_or_else(|| LowerError::missing(&format!("`{label}`.luks.name")))?
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

fn ensure_unique<'a>(labels: impl Iterator<Item = &'a str>, what: &str) -> Result<(), LowerError> {
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
            let mut e = vec![
                ("type", str_expr("filesystem")),
                ("format", str_expr(format)),
            ];
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
            node_child(
                "disk",
                "A physical disk with a GPT partition table. Repeatable.",
            ),
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
        let mut ctx = LowerCtx::new(
            Scope {
                host: "vault".into(),
            },
            &reg,
            &mut diags,
            vec![],
        );
        d.lower(&node(src), &mut ctx).expect("lower ok").units
    }

    fn lower_err(src: &str) -> String {
        let d = Disko::new();
        let reg = Registry::new();
        let mut diags = Vec::new();
        let mut ctx = LowerCtx::new(
            Scope {
                host: "vault".into(),
            },
            &reg,
            &mut diags,
            vec![],
        );
        // LowerOutput has no Debug impl, so expect_err (which requires T: Debug) does not
        // work here; match instead.
        match d.lower(&node(src), &mut ctx) {
            Ok(_) => panic!("lower err: expected an error, got Ok"),
            Err(e) => format!("{e}"),
        }
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
        let NixExpr::AttrSet(parts) = content.get(&AttrKey::Ident("partitions".into())).unwrap()
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
             \x20   zpool \"tank\" { mountpoint \"/tank\"; dataset \"media\" mountpoint=\"/tank/media\" }\n\
             }",
        );
        // one disk assignment + one pool assignment
        assert_eq!(units.len(), 2);
        let parts = partitions_of(&units, "main");
        for name in ["ESP", "swap", "crypt", "data"] {
            assert!(
                parts.contains_key(&AttrKey::Quoted(name.into())),
                "missing {name}"
            );
        }
        // swap without resume has no resumeDevice
        let NixExpr::AttrSet(swap) = parts.get(&AttrKey::Quoted("swap".into())).unwrap() else {
            panic!()
        };
        let NixExpr::AttrSet(swap_content) = swap.get(&AttrKey::Ident("content".into())).unwrap()
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
            "disko { zpool \"tank\" { mountpoint \"/tank\"; dataset \"media\" mountpoint=\"/tank/media\" } }",
        );
        assert_eq!(units.len(), 1);
        let a = &units[0].assignment;
        assert_eq!(a.path.0.last(), Some(&AttrKey::Quoted("tank".into())));
        let NixExpr::AttrSet(pool) = &a.value else {
            panic!()
        };
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
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"p\" size=\"1G\" { } } }",
        );
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
    fn luks_wrapped_dangling_pool_errors() {
        // check_pool_refs must recurse through Content::Luks to find the zfs ref it wraps.
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"c\" size=\"1G\" { luks name=\"a\" { zfs pool=\"nope\" } } } }",
        );
        assert!(e.contains("nope"));
    }

    #[test]
    fn luks_in_luks_rejected() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"c\" size=\"1G\" { luks name=\"a\" { luks name=\"b\" { filesystem format=\"ext4\" mountpoint=\"/\" } } } } }",
        );
        assert!(e.contains("luks may not nest inside luks"));
    }

    #[test]
    fn duplicate_partition_label_errors() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"p\" size=\"1G\" { swap }\n partition \"p\" size=\"2G\" { swap } } }",
        );
        assert!(e.contains("duplicate partition"));
    }

    #[test]
    fn duplicate_disk_label_errors() {
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"p\" size=\"1G\" { swap } };disk \"m\" device=\"/dev/sdb\" { partition \"q\" size=\"1G\" { swap } } }",
        );
        assert!(e.contains("duplicate disk"));
    }

    #[test]
    fn duplicate_dataset_label_errors() {
        let e = lower_err(
            "disko { zpool \"tank\" { dataset \"d\" mountpoint=\"/tank/d\"; dataset \"d\" mountpoint=\"/tank/d2\" } }",
        );
        assert!(e.contains("duplicate dataset"));
    }

    #[test]
    fn unknown_content_child_errors() {
        // A valid `filesystem` content sibling must not mask an unknown node under the same
        // partition: both are hard errors, not a silently-dropped extra child.
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"p\" size=\"1G\" { filesystem format=\"ext4\" mountpoint=\"/\"\n btrfs } } }",
        );
        assert!(e.contains("btrfs"));
    }

    #[test]
    fn luks_inner_arity_enforced() {
        let zero = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" { partition \"c\" size=\"1G\" { luks name=\"x\" { } } } }",
        );
        assert!(zero.contains("exactly one content"));
    }

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
        // KDL requires a separator between sibling nodes even when the preceding sibling's own
        // children block just closed; `disk { ... } zpool` alone does not parse, so join with a
        // `;` (must sit directly against the closing brace: a space either side of `;` fails to
        // parse under this crate's KDL grammar).
        let e = lower_err(
            "disko { disk \"m\" device=\"/dev/sda\" preset=\"boot-root-zfs\" pool=\"tank\" root-size=\"10G\" { partition \"x\" size=\"1G\" { swap } };zpool \"tank\" { } }",
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
}
