# disko disk-layout module

Closes #37.

## Problem

A host's disk layout is today only expressible as `raw-nix`: an ESP, an ext4
root, and a raw partition handed straight to ZFS as a vdev all have to be
hand-written Nix under `disko.devices.*`. That is exactly the kind of stock,
structured NixOS config knixl exists to generate. The motivating case is the
raw ZFS-vdev partition (no filesystem, consumed by the pool), which disko
expresses as a named declarative pool rather than a size `-1` partition you
hope nothing formats. The homelab `autoinstall/user-data` storage config is the
reference, including its note about preserving the pool partition across a
reinstall.

## Why a built-in, not a declarative module

The issue anticipated the fork ("or the module lands as a built-in"). It has to
be a built-in. disko's config is a set of name-keyed attribute sets nested three
levels deep, with heterogeneous content per partition:

```
disko.devices.disk.<label>.content.partitions.<name>.content = { ... }
disko.devices.zpool.<label>.datasets.<name> = { ... }
```

The declarative template grammar (`crates/knixl-modules/src/template.rs`) is
single-level: `for-each` and `list` iterate one top-level repeated child whose
fields are scalars (`binding_for_child` builds exactly one scope level and never
recurses into grandchildren), and `list` emits a Nix list `[ ]`, never a
name-keyed attrset. It cannot express disk to partitions to per-partition
content. A built-in builds `NixExpr::AttrSet` directly (as `builtin/backups.rs`
already does), so it can. disko therefore joins `host` / `postgres` / `backups`
in `crates/knixl-modules/src/builtin/`.

## KDL surface

One `disko` node, claimed by a new built-in. It holds `disk` children (repeated)
and `zpool` children (repeated).

```kdl
disko {
    disk "main" device="/dev/nvme0n1" {
        partition "ESP"   size="512M" type="EF00" { filesystem format="vfat" mountpoint="/boot" }
        partition "swap"  size="8G"               { swap }
        partition "crypt" size="100G"             { luks name="cryptroot" { filesystem format="ext4" mountpoint="/" } }
        partition "data"  size="100%"             { zfs pool="tank" }
    }
    zpool "tank" {
        mountpoint "/tank"
        dataset "media" mountpoint="/tank/media"
    }
}
```

### disk

- `disk "<label>"` : the label is the attribute name under
  `disko.devices.disk.<label>`.
- `device="<path>"` : required. Emits `device` and a fixed `type = "disk"`.
- The partition table is always GPT: emits `content = { type = "gpt"; partitions = { ... }; }`.
- `partition` children, in KDL source order, populate `partitions`.

### partition

- `partition "<name>"` : the label is the attribute name under `partitions`.
- `size="<size>"` : required. A disko size string, e.g. `512M`, `100G`, `100%`.
  knixl does not parse or validate the string; it passes it through.
- `type="<code>"` : optional GPT type code, e.g. `EF00`. Emitted as `type` when
  present, omitted otherwise (disko defaults it).
- exactly one content child (see below). Zero or more than one is a validation
  error.

### content (recursive)

Each partition, and a luks wrapper, has exactly one content child. Four forms:

- `filesystem format="<fmt>" mountpoint="<path>"`
  -> `content = { type = "filesystem"; format = "<fmt>"; mountpoint = "<path>"; }`.
  `format` required; `mountpoint` optional (omitted when absent).
- `zfs pool="<poolname>"`
  -> `content = { type = "zfs"; pool = "<poolname>"; }`.
  `pool` required. `<poolname>` must match a `zpool "<poolname>"` declared in the
  same `disko` node (see Validation).
- `swap [resume=#true]`
  -> `content = { type = "swap"; }`, adding `resumeDevice = true` when `resume`
  is true.
- `luks name="<mapper>" { <inner content> }`
  -> `content = { type = "luks"; name = "<mapper>"; content = <inner>; }`.
  `name` required. The block holds exactly one inner content child (filesystem,
  zfs, or swap); zero or more than one is a validation error. luks does not nest
  inside luks in v1.

### zpool

- `zpool "<label>"` : the label is the attribute name under
  `disko.devices.zpool.<label>`. Emits a fixed `type = "zpool"`.
- `mountpoint "<path>"` : optional (at most one). Emits `mountpoint` when present.
- `dataset "<name>"` children (repeated), in source order, populate `datasets`:
  - `dataset "<name>" [type="<zfs-type>"] [mountpoint="<path>"]`
    -> `datasets.<name> = { type = "<zfs-type>"; [mountpoint = "<path>"]; }`.
    `type` defaults to `zfs_fs`; `mountpoint` emitted when present.

## Shorthand

For the common OS-plus-data layout, a `disk` may carry `preset` instead of
explicit `partition` children. It is pure sugar: the built-in expands it into
the same internal disk representation before emit, so it produces byte-identical
output to the verbose form.

```kdl
disk "main" device="/dev/nvme0n1" preset="boot-root-zfs" pool="tank" root-size="100G"
```

`preset="boot-root-zfs"` expands to three partitions:

- `ESP`  : `size = boot-size` (default `512M`), `type = "EF00"`, filesystem
  `vfat` at `/boot`.
- `root` : `size = root-size` (required with this preset), filesystem `ext4` at
  `/`.
- `data` : `size = "100%"` (the remainder), zfs vdev handed to `pool`.

`pool` is required with this preset and must match a declared `zpool` (same rule
as an explicit `zfs pool=`). The `100%` remainder goes to the ZFS data
partition, matching the reinstall note: the sized OS partitions are reinstalled,
the big pool partition takes the rest and survives.

Shorthand props (`preset`, `pool`, `boot-size`, `root-size`) are mutually
exclusive with explicit `partition` children on the same disk: a disk with both
`preset` and a `partition` child is a validation error. Only `preset="boot-root-zfs"`
is defined; any other value is a validation error.

## Emit

The motivating verbose example above lowers to the following. Note that
partitions render lexicographically by name (`ESP`, `crypt`, `data`, `swap`),
not in KDL source order: a `NixExpr::AttrSet` is a `BTreeMap`, and a Nix attrset
is unordered anyway, so disko always sees partitions sorted by name and controls
on-disk allocation through its own per-partition `priority` field, never attr
order (see Determinism).

```nix
disko.devices.disk.main = {
  device = "/dev/nvme0n1";
  type = "disk";
  content = {
    type = "gpt";
    partitions = {
      ESP = {
        size = "512M";
        type = "EF00";
        content = { type = "filesystem"; format = "vfat"; mountpoint = "/boot"; };
      };
      crypt = {
        size = "100G";
        content = {
          type = "luks";
          name = "cryptroot";
          content = { type = "filesystem"; format = "ext4"; mountpoint = "/"; };
        };
      };
      data = { size = "100%"; content = { type = "zfs"; pool = "tank"; }; };
      swap = { size = "8G"; content = { type = "swap"; }; };
    };
  };
};
disko.devices.zpool.tank = {
  type = "zpool";
  mountpoint = "/tank";
  datasets = { media = { type = "zfs_fs"; mountpoint = "/tank/media"; }; };
};
```

Each `disko.devices.disk.<label>` and `disko.devices.zpool.<label>` is one
`Assignment` whose value is a fully-built `NixExpr::AttrSet`. Two assignments per
disko node (one disk, one pool in the example); a multi-disk or multi-pool node
emits one assignment each.

## Validation

Checked in the built-in's `lower`, surfaced as `LowerError` / diagnostics like
the other built-ins, before any oracle pass:

- `device` is required on every `disk`.
- a `disk` has either `preset` (shorthand) or `partition` children, never both.
- `preset`, if present, is `boot-root-zfs`; `pool` and `root-size` are required
  with it.
- every `partition` has exactly one content child.
- a `luks` block has exactly one inner content child.
- every `zfs pool="X"` (explicit or preset-expanded) references a `zpool "X"`
  declared in the same `disko` node. A dangling reference is a hard error, not a
  silent emit (a typo must not produce a vdev pointing at a pool that is never
  created).
- disk, partition, pool, and dataset labels are unique within their scope. Two
  partitions with the same name would collapse to one `BTreeMap` key, silently
  dropping one, so a duplicate label is a hard error.

Beyond that the module is oracle-agnostic. The emitted `disko.*` paths are
validated only when the project declares disko as an out-of-tree oracle module
in `knixl.kdl` (the #35 mechanism); absent that they are unchecked, which the
issue accepts. No change to the oracle crate.

## Determinism

Output is a pure function of the input, as the lock requires:

- partitions, datasets, disks, and pools are all built into name-keyed
  `NixExpr::AttrSet`s, which are `BTreeMap`s, so their emit order is lexicographic
  by label and deterministic by construction. No `HashMap` on any emit path. KDL
  source order is not preserved and does not need to be: a Nix attrset is
  unordered, and disko orders partition allocation through its own `priority`
  field, so attr order carries no meaning. Exposing `priority` is a follow-up
  (see Non-goals).
- dynamic names (disk, partition, pool, dataset labels) are `AttrKey::Quoted`,
  so `to_option_key` collapses them to `<name>` for the oracle exactly as
  `backups` does for `services.restic.backups.<name>`. Structural keys (`device`,
  `type`, `content`, `partitions`, `format`, `mountpoint`, `size`, `pool`,
  `datasets`) are `AttrKey::Ident`. The committed golden reflects the emitter's
  actual rendering of these quoted keys.

## Acceptance tests (golden)

A golden host `vault` exercising every content form (filesystem, zfs, swap,
luks) and a pool with a dataset, landing as a first-class golden like `nas`:

- `examples/hosts/vault.kdl` : the verbose form above (the disko node on a host
  that also declares `system`).
- `examples/expected/vault.nix` : byte-exact `nixfmt` output, generated by
  running the pipeline with a real formatter and committed. Not hand-written.
- Tests in `crates/knixl-pipeline/tests/golden.rs`:
  - a structural test (identity formatter, run unconditionally) asserting the
    distinguishing leaf fragments are present: `"/dev/nvme0n1"`, `"disk"`,
    `"gpt"`, the four partition names, `"EF00"`, `"filesystem"`, `"vfat"`,
    `"swap"`, `"luks"`, `"cryptroot"`, `"zfs"` with `pool = "tank"`, `"zpool"`,
    `datasets`, `"zfs_fs"`. These are substrings, not exact lines: the byte-exact
    form is nested and is nailed down by the golden below, so the structural test
    only proves every piece reached the output.
  - a byte-for-byte golden test gated on `formatter_available()` like the other
    `*_matches_golden` tests, against `examples/expected/vault.nix`;
  - a module-attribution assertion that the generated file lists `disko` (plus
    whatever else the host uses).

A no-drift test (in `disko.rs` or the pipeline tests) that lowers a
`preset="boot-root-zfs"` disk and its hand-written verbose equivalent and asserts
the two disk attrsets are equal, so the sugar can never diverge from the
desugared form. This is an in-test comparison, not a second committed golden.

Unit tests in `disko.rs`:

- each content form lowers to the right attrset (filesystem, zfs, swap with and
  without `resume`, luks wrapping a filesystem);
- `preset="boot-root-zfs"` expands to ESP + ext4 root + zfs data, with default
  and overridden `boot-size`;
- validation errors fire: missing `device`; a partition with zero and with two
  content children; a luks block with zero and with two inner content children;
  `preset` plus an explicit `partition`; an unknown `preset` value; a `zfs pool=`
  naming a pool with no matching `zpool`; a duplicate partition label.

## Non-goals

- No swap `randomEncryption`, no luks key-file / TPM settings, no `settings`
  passthrough on any content: v1 covers the plain forms only.
- No mdraid, lvm, btrfs subvolumes, or bcachefs.
- No luks-inside-luks, and no zpool `options` / `rootFsOptions` passthrough.
- No per-partition `priority` knob. disko's default allocation handles a single
  `100%` partition; explicit ordering control is a follow-up.
- No oracle or lock change. No human-size parsing (sizes pass through verbatim).
- No hardware-profile generation; that is separate (noted on #40).

## Docs

- `docs/04-template-grammar.md` (or the built-in module reference it points to):
  add disko to the built-in module list with its node shape.
- No new ADR: this is a feature within the settled built-in-module and emit
  model, not a decision that reverses one.
