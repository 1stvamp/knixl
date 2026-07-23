# incus-host module (declarative)

Closes #36.

## Problem

An Incus host is stock NixOS config: `virtualisation.incus.enable`, a daemon
`preseed` (storage pools, networks, profiles), the web UI, and supporting host
bits. Today it can only be hand-written `raw-nix`. Model it as a knixl module so
the host's Incus setup is declared in KDL and generated.

## Why declarative, not built-in

The issue says this "has to be a built-in ... because the preseed is lists of
attribute sets which the emit grammar cannot produce (#34)". That premise is
stale: #34 shipped the `list` statement, and its own tests use
`virtualisation.incus.preseed.networks` as the list-of-attrsets example
(`crates/knixl-modules/src/template.rs`). The grammar now emits the preseed, so
this is a declarative module at `modules/incus/knixl-module.kdl`.

The two parts the grammar cannot express are handled by existing companion nodes,
not by this module:

- VM support (qemu) is a package reference (`pkgs.qemu`), which the grammar has
  no value form for. A host adds it with the existing `package "qemu"` node.
- The admin user in the `incus-admin` group is the `user` module's job:
  `user "wes" { group "incus-admin" }`. NixOS merges `extraGroups`, and knixl
  merges same-path list assignments, so this composes with any other groups.

The incus module owns enable, the UI, and the preseed. The companion nodes are
documented in the module's docs, not emitted here.

## KDL surface

`incus` claims the `incus` node. All three preseed sections are repeated children.

```kdl
incus {
    ui
    storage-pool "default" driver="zfs" source="rpool/incus"
    network "incusbr0" type="bridge" ipv4="auto" nat="true"
    profile "default" pool="default" network="incusbr0"
}
```

- `ui` : a bool flag. Present -> `virtualisation.incus.ui.enable = true`
  (verified a real option: `virtualisation.incus.ui.enable` /
  `virtualisation.incus.ui.package` exist in the oracle's cached NixOS options).
- `storage-pool "<name>"` (repeated): `driver` and `source` required props. The
  name is the node label (a schema `arg`).
- `network "<name>"` (repeated): `type`, `ipv4`, `nat` required props.
- `profile "<name>"` (repeated): `pool` and `network` required props. Emits the
  standard default-profile shape: a `root` disk on `pool` and an `eth0` nic on
  `network`.

## Emit

```
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
```

Which lowers (for the example above) to:

```nix
virtualisation.incus.enable = true;
virtualisation.incus.ui.enable = true;
virtualisation.incus.preseed.storage_pools = [
  { name = "default"; driver = "zfs"; config = { source = "rpool/incus"; }; }
];
virtualisation.incus.preseed.networks = [
  { name = "incusbr0"; type = "bridge"; config = { "ipv4.address" = "auto"; "ipv4.nat" = "true"; }; }
];
virtualisation.incus.preseed.profiles = [
  {
    name = "default";
    devices = {
      root = { path = "/"; pool = "default"; type = "disk"; };
      eth0 = { name = "eth0"; network = "incusbr0"; type = "nic"; };
    };
  }
];
```

(Attr key order within each element is BTreeMap-lexicographic, as the emitter
always produces; the exact bytes are pinned by the golden.)

## Validation and the oracle

- The declarative loader dry-type-checks the template at module load, and the
  schema validates the node at generate (required props present, no unknown
  children).
- `virtualisation.incus.enable` and `virtualisation.incus.ui.enable` are stock
  in-tree options the oracle already covers. `virtualisation.incus.preseed` is a
  freeform attribute set (incus's own preseed schema), so its inner keys
  (`storage_pools`, `config.source`, `devices.*`, etc.) are validated by incus at
  runtime, not by the NixOS oracle. No oracle-module dependency, no oracle-crate
  change.

## Acceptance tests

Golden host (mirroring `nas`/`gateway`):

- `examples/hosts/vmhost.kdl`: a host declaring `system` and an `incus` node with
  the example above. The golden stays focused on the incus module: the companion
  `package "qemu"` / `user`-in-`incus-admin` pattern is shown in docs, not bundled
  here (user and package already have their own goldens, and bundling them would
  tie this golden's bytes to those modules).
- `examples/expected/vmhost.nix`: byte-exact nixfmt output, blessed (not
  hand-written).
- `crates/knixl-pipeline/tests/golden.rs`:
  - a structural test (identity formatter, unconditional) asserting the needles:
    `virtualisation.incus.enable = true`, `virtualisation.incus.ui.enable = true`,
    `virtualisation.incus.preseed.storage_pools`, `driver = "zfs"`,
    `source = "rpool/incus"`, `virtualisation.incus.preseed.networks`,
    `"ipv4.address" = "auto"`, `"ipv4.nat" = "true"`,
    `virtualisation.incus.preseed.profiles`, `type = "disk"`, `type = "nic"`,
    `network = "incusbr0"`;
  - a byte-exact `vmhost_matches_golden` gated on `formatter_available()`;
  - a module-attribution assertion that the file lists `incus` (and `host`).

The declarative module needs no new unit tests in `template.rs`: it exercises
only `set`, `when-flag`, and `list`, all already covered. The golden is the
module's contract.

## Non-goals

- Profiles with arbitrary/variable devices: v1 emits the fixed default-profile
  shape (a root disk plus one nic). Nested per-profile device lists need
  repetition the single-level grammar lacks, and the issue scopes out anything
  inside the daemon.
- `ipv6` and other network config keys beyond `ipv4.address` / `ipv4.nat`: each
  would be another fixed prop (the grammar cannot iterate a network's own
  sub-children); follow-ups.
- Non-zfs storage specifics, multiple storage drivers' config shapes: `driver` and
  `source` are passed through; richer per-driver config is a follow-up.
- VM support (qemu) and the admin group are NOT emitted by this module; they are
  the `package` and `user` companion nodes (documented).
- Incus projects, profiles, or instances created inside the daemon: control-plane
  state managed through the incus provider, not NixOS config (issue out-of-scope).

## Determinism

Every emit path is `set`, `when-flag`, or `list` over KDL source order, all
already deterministic (the lock depends on it, and #34 established `list`'s
determinism). No `HashMap`. Output is a pure function of the input.

## Docs

- `docs/04-template-grammar.md`: add `incus` to the declarative-modules section,
  noting the companion `package "qemu"` and `user`-group pattern for the parts
  outside the module.
- No new ADR: a feature within the settled module and emit model.
