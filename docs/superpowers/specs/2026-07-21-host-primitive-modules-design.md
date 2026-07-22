# Host primitive modules: zfs, user, openssh

Closes #39.

## Problem

The easy host primitives (ZFS, a login user, hardened OpenSSH) are today only
expressible as `raw-nix` or ad hoc `set`s. They are all stock NixOS options the
oracle already covers, so they can each be a small declarative module under
`modules/<name>/knixl-module.kdl`, and a host stops needing `raw-nix` for the
basics.

These ship as general-purpose knixl primitives, not narrow homelab presets. The
homelab work (project:homelab issues) is one consumer, but the modules expose
the knobs any knixl user would want. Reference behaviour comes from the homelab
ansible: `tasks/system.yml` (ARC cap) and `autoinstall/user-data` (identity,
ssh keys, `allow-pw: false`).

## Grammar addition: `(collect-opt)`

To make the modules genuinely general, one small, additive grammar feature is
needed. Today the `set` value forms are scalar, interpolated string, indent
string, and `(collect)"child"`. `(collect)` always emits its assignment, so an
optional list-valued option (SSH ports, extra ZFS pools) would emit `= [ ]` when
the child is absent, overriding the NixOS default. For `services.openssh.ports`
that default is `[ 22 ]`, so an empty emit would lock SSH out. That is a footgun,
not flexibility.

Add a new value form:

- `(collect-opt)"child"` : like `(collect)`, but the enclosing `set` emits
  **nothing** when the child is empty. When non-empty it behaves exactly like
  `(collect)` (a flat `List` of the child's first args, in KDL source order).

`(collect)` is unchanged (still always emits `[ ]` when empty), so this is
non-breaking and no existing golden moves.

Implementation (all in `crates/knixl-modules/src/template.rs`):

1. `enum ValueTemplate`: add `CollectOpt(String)`.
2. `parse_value`: map the `(collect-opt)` type annotation to `CollectOpt`
   (alongside the existing `Some("collect")` arm).
3. `ValueTemplate::interpret`: resolve `CollectOpt` identically to `Collect`
   (reuse the same scalar-item folding), returning a `NixExpr::List`.
4. `Stmt::Set` in `run`: after interpreting the value, if the value template is
   `CollectOpt` and the resolved `NixExpr::List` is empty, skip pushing the unit
   (`continue`), so no assignment is emitted. Otherwise emit as today.
5. The dry-check pass (the `match value` over `ValueTemplate`): handle
   `CollectOpt` the same as `Collect` (must reference a repeated child of scalar
   items).

Documented in `docs/04-template-grammar.md` under Values.

The other conditional / repetition tools are used as-is: `when-flag` (bool),
`for-each "<var>" in "<repeated-child>"` (0..n; emits nothing when empty, the
honest way to make a scalar optional as a 0-or-1 repeated child).

## Module 1: zfs

The mandatory `networking.hostId` is the point: ZFS refuses to import a pool
whose host ID does not match, so it must be set and stable.

```kdl
module name="zfs" version="1.0.0" {
    summary "Enable ZFS with the mandatory hostId, optional ARC cap and scrubbing."
    claims-node "zfs"

    schema {
        arg "host-id" type="string" required=#true \
            doc="8 hex-digit machine ID, mandatory for ZFS. Generate with: head -c4 /dev/urandom | od -A none -t x4"
        child "auto-scrub" type="bool" \
            doc="Enable periodic pool scrubbing (services.zfs.autoScrub.enable)."
        child "extra-pool" type="string" repeated=#true \
            doc="Pool name to import at boot (boot.zfs.extraPools)."
        child "arc-max-bytes" repeated=#true \
            doc="Cap the ARC at N bytes via boot.extraModprobeConfig. At most one." {
            arg "bytes" type="int" required=#true
        }
    }

    emit {
        set "networking.hostId" "{host-id}"
        set "boot.supportedFilesystems.zfs" #true
        set "boot.zfs.extraPools" (collect-opt)"extra-pool"
        when-flag "auto-scrub" {
            set "services.zfs.autoScrub.enable" #true
        }
        for-each "cap" in "arc-max-bytes" {
            set "boot.extraModprobeConfig" "options zfs zfs_arc_max={cap.bytes}"
        }
    }
}
```

Notes:

- `boot.supportedFilesystems.zfs = true` (the attrset form, valid on nixpkgs
  24.05+) rather than a `[ "zfs" ]` list, because the grammar has no list
  literal.
- `extra-pool` uses `(collect-opt)`, so no `boot.zfs.extraPools = [ ]` noise when
  none is given.
- `arc-max-bytes` is a 0-or-1 repeated child (the grammar has no "emit only if
  present" gate for a scalar). Documented "at most one"; more than one emits the
  modprobe line more than once (last wins under Nix), the user's error to make.
  `bytes` is the raw byte count; knixl does not parse human sizes like "8G".

## Module 2: user

A normal (login) user with an optional description, supplementary groups and
authorised SSH keys, matching the ansible identity. No password handling here:
secrets are #38.

```kdl
module name="user" version="1.0.0" {
    summary "A normal login user with groups and SSH authorised keys."
    claims-node "user"

    schema {
        arg "name" type="string" required=#true doc="Login name."
        child "description" repeated=#true \
            doc="Full name / GECOS field. At most one." {
            arg "text" type="string" required=#true
        }
        child "group" type="string" repeated=#true \
            doc="Supplementary group, e.g. wheel."
        child "ssh-key" type="string" repeated=#true \
            doc="Authorised SSH public key."
    }

    emit {
        set "users.users.{name}.isNormalUser" #true
        set "users.users.{name}.extraGroups" (collect-opt)"group"
        set "users.users.{name}.openssh.authorizedKeys.keys" (collect-opt)"ssh-key"
        for-each "d" in "description" {
            set "users.users.{name}.description" "{d.text}"
        }
    }
}
```

Notes:

- `isNormalUser = true` (a login/admin user), not `isSystemUser` (service
  account).
- `description` is a 0-or-1 repeated child, same idiom as `arc-max-bytes`.
- No `uid` or `shell`: a dynamic `uid` would need an int-valued interpolation
  (value interpolation only produces strings today), and `shell` is a package
  reference (raw Nix). Both are follow-ups.

## Module 3: openssh

A hardened OpenSSH: enabled, password and keyboard-interactive auth off
(public-key auth stays on, the NixOS default), with the common general knobs.

```kdl
module name="openssh" version="1.0.0" {
    summary "Hardened OpenSSH (password auth off) with port and login knobs."
    claims-node "openssh"

    schema {
        child "port" type="int" repeated=#true \
            doc="Listen port(s) (services.openssh.ports). NixOS default [ 22 ] if omitted."
        child "permit-root" repeated=#true \
            doc="PermitRootLogin value, e.g. \"no\" or \"prohibit-password\". At most one." {
            arg "value" type="string" required=#true
        }
        child "x11-forwarding" type="bool" \
            doc="Enable X11 forwarding (off by default)."
    }

    emit {
        set "services.openssh.enable" #true
        set "services.openssh.settings.PasswordAuthentication" #false
        set "services.openssh.settings.KbdInteractiveAuthentication" #false
        set "services.openssh.ports" (collect-opt)"port"
        when-flag "x11-forwarding" {
            set "services.openssh.settings.X11Forwarding" #true
        }
        for-each "r" in "permit-root" {
            set "services.openssh.settings.PermitRootLogin" "{r.value}"
        }
    }
}
```

Notes:

- Password-off is the module's baked opinion (its identity is *hardened*
  OpenSSH). A declarative module is straight-line substitution with no
  conditional priorities (docs/03), so it cannot express an "off by default,
  overridable on" toggle; that would need a built-in module or an `else`-style
  gate. A host that genuinely wants password auth overrides via a sibling module
  (the documented override path) or does not use this one. `permit-root` is the
  general login knob provided instead.
- `port` uses `(collect-opt)`: omitted -> no `ports` line -> NixOS default
  `[ 22 ]` stays. This is exactly why the grammar feature is needed.

## Acceptance tests (golden)

The three modules live at `modules/{zfs,user,openssh}/knixl-module.kdl` and are
auto-discovered by the golden harness (`build_registry` walks `modules/*`).

Add one golden example host that uses all three together, exercising every emit
shape, so this lands as a first-class golden like `web`/`shared`:

- `examples/hosts/nas.kdl`:

  ```kdl
  host "nas" {
      system "x86_64-linux"

      zfs "8425e349" {
          auto-scrub #true
          extra-pool "tank"
          arc-max-bytes 8589934592
      }

      user "wes" {
          description "Wes Mason"
          group "wheel"
          ssh-key "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAExampleKeyForGoldenTest wes@nas"
      }

      openssh {
          port 22
          port 2222
          permit-root "prohibit-password"
      }
  }
  ```

- `examples/expected/nas.nix`: the byte-exact `nixfmt` output, generated by
  running the pipeline with a real formatter (`KNIXL_FORMATTER=nixfmt`), then
  committed. Do not hand-write it.
- Tests in `crates/knixl-pipeline/tests/golden.rs`:
  - `nas_pipeline_produces_expected_structure` (structural needles, identity
    formatter, run unconditionally), asserting: `networking.hostId = "8425e349"`,
    `boot.supportedFilesystems.zfs = true`, `boot.zfs.extraPools`,
    `services.zfs.autoScrub.enable = true`,
    `options zfs zfs_arc_max=8589934592`, `users.users."wes".isNormalUser = true`,
    `users.users."wes".description = "Wes Mason"`,
    `users.users."wes".openssh.authorizedKeys.keys`,
    `services.openssh.settings.PasswordAuthentication = false`,
    `services.openssh.ports`, `PermitRootLogin = "prohibit-password"`;
  - `nas_matches_golden` (byte-for-byte), gated on `formatter_available()` like
    the other `*_matches_golden` tests;
  - a module-attribution assertion: `nas.nix` lists `host`, `zfs`, `user`,
    `openssh`.

- Unit tests for `(collect-opt)` in `crates/knixl-modules/src/template.rs`:
  - a non-empty `collect-opt` child emits the list (same as `collect`);
  - an empty `collect-opt` child emits no assignment at all (the key new
    behaviour), whereas an empty `collect` still emits `[ ]`.

## Non-goals

- No lock/oracle change: these are stock options the oracle already covers.
- No secrets/password handling (that is #38).
- No human-readable size parsing for the ARC cap.
- No change to `(collect)` behaviour or any existing golden. Switching
  web-service's `serverAliases` to `(collect-opt)` to de-noise its `[ ]` is a
  possible later cleanup, out of scope here.

## Determinism

Every emit path is a plain `set`, `collect`/`collect-opt`, `when-flag`, or
`for-each` over KDL source order, so output is a pure function of the input, as
the lock requires. No `HashMap` on any emit path.
