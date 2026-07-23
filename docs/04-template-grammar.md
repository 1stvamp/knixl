# 04: EmitTemplate grammar

The substitution grammar for declarative modules. Parsed once from a module's `emit { ... }` block into a small AST, then interpreted per-node against a bindings tree built from the validated input. Full types in `crates/knixl-modules/src/template.rs`.

## Five statement forms

Matching exactly the boundary in docs/03 (substitute, repeat-into-list, fold-into-list-of-attrsets, gate-on-flag, gate-at-runtime):

- `set <path> <value>` : assign a value into an option path.
- `when-flag "<flag>" { ... }` : generation-time gate on a bool input. Includes or drops its body.
- `when-config "<cond>" { ... }` : runtime gate. Always emits its body, wrapping each assignment in `lib.mkIf (<cond>) <value>`. The condition is raw Nix off `config.*` with `{lookup}` interpolation (dry-checked at load like a `set` path); the Nix expression itself is opaque and unvalidated. Nested `when-config` conjoin: `(A) && (B)`.
- `for-each "<var>" in "<repeated-child>" { ... }` : iterate a repeated child, binding `<var>` per item, in KDL source order.
- `list "<path>" from "<repeated-child>" { set "<attr>" <value> ... }` : fold a repeated child into a list of attribute sets (`[ { ... } { ... } ]`) at `<path>`, one element per child in KDL source order. The child name is the loop binding (`from "network"` binds `{network.field}`). Each element is built from inner `set` statements (relative attr paths, so nested and quoted keys like `config."ipv4.address"` work), optionally gated by `when-flag` (generation-time) or `when-config` (which wraps that attr's value in `lib.mkIf`). Two inner sets writing the same path is an error.

## Values

- scalars: `#true`, `16`, `"literal"`
- interpolated string: `"{upstream}"`, parts are literal or `{lookup}`
- indent string: `(indent-str)#""" ... """#`, interpolated, emits a `'' ... ''` block
- `(collect)"child"` : fold a repeated child's first arg into a `List`. The only value form that reads a repeated child directly into a flat list. Use `for-each` when each item must produce distinct structure (a path per item) rather than a flat list. (Both are KDL type annotations: `(type)value`.)
- `(collect-opt)"child"` : like `(collect)`, but the whole `set` is omitted when the
  child is empty, rather than emitting `[ ]`. Use it for an optional list-valued option
  whose absence should leave the NixOS default in place (e.g. `services.openssh.ports`,
  which defaults to `[ 22 ]`). `(collect)` still emits `[ ]` when empty.
- `(secret)"name"` : a reference to a decrypted secret path, emitting `config.<backend>.secrets."name".path`. The name may interpolate bindings (e.g. `(secret)"{k.secret}"`). The backend is the project's `secrets backend=` setting (default sops-nix; the other value is agenix). knixl never sees the secret material: reference-only, no declaration and no name validation.

## Paths

A dotted option path where each segment is literal or interpolated:

- bare word (`services`, `nginx`, `forceSSL`) -> `AttrKey::Ident`
- quoted literal (`"/"`) -> `AttrKey::Quoted`, may itself interpolate
- interpolation (`{host}`, `{loc.match}`) -> `AttrKey::Quoted` (a dynamic name)

This is where the oracle's `to_option_key()` promise is kept: every `Quoted` segment collapses to `<name>` for option lookup, so `services.nginx.virtualHosts."example.com".forceSSL` matches the option `services.nginx.virtualHosts.<name>.forceSSL`.

## Bindings tree

`bind()` walks the *schema*, not the raw node, so resolution is total and typed (validation already ran, so every referenceable name is present). Three shapes:

- `Scalar` : `"example.com"`, `16`, `true`
- `Scope` : a structured child, e.g. `acme -> { email }`, resolved with a dotted lookup `{acme.email}`
- `List` : a repeated child, in KDL source order

How each schema field maps:

- arg field -> `Scalar` (positional value)
- prop field -> `Scalar` (key=value on the node)
- flag child -> `Scalar::Bool` (present-and-true)
- scalar child -> `Scalar` (child's first arg)
- structured child -> `Scope` over its own args/props
- repeated child -> `List` of the above

## Lookup resolution

`{host}` resolves top-level. `{acme.email}` walks `Scope("acme")` then `Scalar("email")`. `{loc.match}` resolves the loop var `loc` (a `Scope` pushed by `for-each`) then `Scalar("match")`.

Loop variables live in a separate `LoopScopes` stack, checked before top-level bindings, so `for-each "host" in ...` shadows the node's `host` rather than colliding (the loader warns on shadow). Resolving a lookup to a non-`Scalar` in value position is a template authoring error, caught at module-load time by a dry type-pass, not at generation time.

## Worked expansion (both list forms)

Input:

```kdl
web-service "example.com" {
    upstream "http://127.0.0.1:3000"
    acme email="ops@example.com"
    alias "www.example.com"
    alias "example.org"
    location "/api" upstream="http://127.0.0.1:4000"
    location "/metrics" upstream="http://127.0.0.1:9090"
}
```

Template fragment:

```kdl
set "services.nginx.virtualHosts.{host}.serverAliases" (collect)"alias"
for-each "loc" in "location" {
    set "services.nginx.virtualHosts.{host}.locations.{loc.match}.proxyPass" "{loc.upstream}"
}
```

Emits (post-nixfmt):

```nix
services.nginx.virtualHosts."example.com".serverAliases = [
  "www.example.com"
  "example.org"
];
services.nginx.virtualHosts."example.com".locations."/api".proxyPass = "http://127.0.0.1:4000";
services.nginx.virtualHosts."example.com".locations."/metrics".proxyPass = "http://127.0.0.1:9090";
```

`collect` gives the flat list, `for-each` gives one dynamic-keyed path per item. Both iterate in KDL source order, so the output hash is a pure function of the input, which is the property the lock depends on.

## Migration notes (optional)

A module may declare notes that `knixl upgrade` prints when it moves the module across a version. They live in a `migrations` block alongside `schema` and `emit`, keyed by the target version:

```kdl
migrations {
    to "1.1.0" {
        note "enableACME now defaults on; drop any manual security.acme.certs entry per host."
    }
    to "1.2.0" {
        note "serverAliases is generated from the repeated `alias` children; remove any hand-written list."
    }
}
```

A step applies when its `to` version lands in the half-open range `(recorded, running]`, so an upgrade from 1.0.0 to 1.2.0 shows both notes (ascending), while 1.1.0 to 1.2.0 shows only the last. The block is metadata: it does not affect emitted Nix or the output hash. Built-in modules provide the same notes through `Module::migration_notes`.

## Built-in modules

Some modules are written in Rust because their logic exceeds what the declarative template grammar can express. Each claims a KDL node and owns its own output structure.

### disko

`disko` claims the `disko` node and generates NixOS disko configuration. It is built-in because disko's config is name-keyed attribute sets nested several levels deep with heterogeneous per-partition content, which the single-level declarative template grammar cannot express.

Node shape: `disk "<label>" device="<path>"` holds `partition "<name>" size="<size>" [type="<code>"]` children. Each partition holds exactly one content child, which is one of:

- `filesystem format="<fmt>" mountpoint="<path>"` : a mounted filesystem.
- `zfs pool="<name>"` : a ZFS member of the pool `<name>`. The pool itself must be declared as `zpool "<name>"` in the same node.
- `swap [resume=#true]` : a swap partition, optionally resuming to it.
- `luks name="<name>"` wrapping one inner content : LUKS encryption around a filesystem, zfs, or swap.

`zpool "<label>"` holds an optional `mountpoint` and zero or more `dataset "<name>" [type="<type>"] [mountpoint="<path>"]` children. Dataset type defaults to `zfs_fs`.

The `preset="boot-root-zfs" pool="<name>" root-size="<size>" [boot-size="<size>"]` shorthand is pure sugar for the common three-partition layout: an ESP (`512M` by default, or `boot-size` if set), an ext4 root (sized by `root-size`), and a ZFS vdev at 100% of the remaining space handed to `pool`. It suits single-disk systems that keep the OS on ext4 and the data on ZFS.

Validation of `disko.*` paths (e.g. `disko.devices.disk.main.device`) runs only when the project declares disko as an out-of-tree oracle module via `oracle-modules { module "disko" ... }` in `knixl.kdl`. Without that declaration, disko paths remain unchecked (docs/06, ADR 0008).

## Declarative modules shipped with knixl

These are authored in the grammar above and live under `modules/<name>/knixl-module.kdl`, not in Rust.

### tailscale

`tailscale` claims the `tailscale` node and generates NixOS tailscale configuration. Node shape: optional `up-flag` children hold flags passed to `tailscale up` (e.g. `up-flag "--ssh"`, `up-flag "--operator=alice"`), and an `auth-key secret="name"` child wires `services.tailscale.authKeyFile` to a named secret via `(secret)`.

The module sets `services.tailscale.enable = true`, collects `up-flag` children into `services.tailscale.extraUpFlags` as a list, and wires the `auth-key` secret reference to `services.tailscale.authKeyFile`. If no `auth-key` is declared, `authKeyFile` is not set, leaving interactive login as the fallback.

### incus

`incus` claims the `incus` node and generates an Incus host. Node shape: repeated `storage-pool "<name>" driver= source=`, `network "<name>" type= ipv4= nat=`, and `profile "<name>" pool= network=` children. A profile emits the default shape: a root disk on `pool` and an `eth0` nic on `network`.

The module sets `virtualisation.incus.enable = true`, enables the web UI via `virtualisation.incus.ui.enable` (gated by a `ui` flag), and configures the daemon preseed from pools, networks, and profiles. Virtual machine support (via `package "qemu"`) and administrative access (via the `user` module with `group "incus-admin"`) are configured separately, which the incus module does not emit.
