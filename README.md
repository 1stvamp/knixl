<p align="center">
  <img src="assets/knixl-logo.png" alt="knixl" width="620">
</p>

# knixl

[![CI](https://github.com/1stvamp/knixl/actions/workflows/ci.yml/badge.svg)](https://github.com/1stvamp/knixl/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/knixl.svg)](https://crates.io/crates/knixl)
[![licence](https://img.shields.io/crates/l/knixl.svg)](https://github.com/1stvamp/knixl#licence)

knixl generates maintainable, human-readable Nix from small amounts of opinionated KDL.

Pronounced "nix-ull". Written in Rust. KDL is the source of truth, the generated Nix is a committed build artefact, and regeneration is version-aware so a framework upgrade cannot change your output without telling you first.

<p align="center">
  <img src="assets/tui-tour.gif" alt="knixl TUI: home, browse modules, new module" width="760">
</p>

## What it does

You write a few lines of KDL:

```kdl
host "web" {
    system "x86_64-linux"
    web-service "example.com" {
        upstream "http://127.0.0.1:3000"
        acme email="ops@example.com"
        hardened #true
    }
}
```

knixl expands it into a full, idiomatic NixOS module (nginx enabled, TLS and proxy recommendations on, ACME wired up, security headers added), formats it with a pinned formatter, writes it to `generated/`, and records hashes in a lockfile so the output is reproducible byte-for-byte.

The whole loop is compile, check, and read back the typed reference for any node:

<p align="center">
  <img src="assets/cli-workflow.gif" alt="knixl generate, check, doc, and a Stale regeneration" width="820">
</p>

## Install

### Nix (flake)

```sh
nix run github:1stvamp/knixl -- --help      # run without installing
nix profile install github:1stvamp/knixl    # install into your profile
```

Or add `overlays.default` and put `pkgs.knixl` in your `environment.systemPackages` (NixOS) or `home.packages` (home-manager).

### crates.io

```sh
cargo install knixl
```

### Prebuilt binary

Download a tarball for your platform from the [latest release](https://github.com/1stvamp/knixl/releases/latest), or run the installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/1stvamp/knixl/releases/latest/download/knixl-installer.sh | sh
```

Built for Linux (gnu and musl) and macOS, on x86_64 and aarch64.

### From source

```sh
git clone https://github.com/1stvamp/knixl && cd knixl
cargo build --release        # or: mise run build
```

The binary lands at `target/release/knixl`.

### Runtime prerequisites

`generate`, `check`, and `install` also need:

- **Nix** on PATH, for the oracle (option-path validation) and for `install`'s package eval.
- **a formatter**: `nixfmt-rfc-style` or `nixfmt` on PATH. `KNIXL_FORMATTER` overrides which binary is used. The Nix flake install already wraps `nixfmt` onto knixl's PATH.

**Note:** without the oracle's `options.json` cache populated, path validation is quietly skipped, so a typo'd option path will not be caught. See docs/06-oracle.md for the cache.

## Quickstart

A knixl project is a directory with a `hosts/` folder. The stdlib modules are built into knixl, so that is all you need to start; your own declarative modules, if you write any, live in an optional `modules/` folder beside it. knixl walks up from the current directory to the first folder that has `hosts/` or `knixl.lock.kdl`, and treats that as the root.

```sh
mkdir -p demo/hosts && cd demo
cat > hosts/web.kdl <<'EOF'
host "web" {
    system "x86_64-linux"
}
EOF

knixl plan        # missing generated/hosts/web.nix
knixl generate    # writes generated/hosts/web.nix and knixl.lock.kdl
knixl check       # clean
```

From there, add a module node to the host and regenerate. The modules are built in (see below), so there is nothing to install or copy first. `knixl doc <node>` prints what a node accepts before you write it.

## Batteries included

knixl ships a curated stdlib of modules, embedded in the binary, usable in any project with no setup (no `modules/` directory to place, nothing to fetch):

- **web-service** : an nginx vhost with TLS, ACME, and hardening presets.
- **disko** : declarative disk layout, GPT partitions with ext4/vfat/swap/LUKS content and ZFS pools, plus a `boot-root-zfs` shorthand.
- **zfs**, **user**, **openssh** : the host primitives, ZFS with a stable `hostId`, a login user with SSH keys, a hardened sshd.
- **tailscale** : Tailscale with an auth key pulled from a named secret.
- **incus** : an Incus host, enable plus the web UI and the storage/network/profile preseed.
- **home-manager** : per-user home-manager with the "Words of warning" guardrails (required `stateVersion`, safe `useUserPackages`, `nix.gc` left to NixOS) baked in.
- **host**, **postgres**, **backups**, **package**, **raw-nix**, **security-headers** : the rest of the built-ins.

Run `knixl doc <node>` for the typed reference of any of them.

Three things worth calling out:

- **Generate a bootable system.** Add a `system { }` block to `knixl.kdl` and `generate` also emits `generated/flake.nix` with `nixosConfigurations.<host>`, each host pinned to its own nixpkgs baseline. `nixos-rebuild switch --flake .#<host>` (or nixos-anywhere) and it boots. Absent the block, knixl emits modules only and the assembly seam stays yours (ADR 0009).
- **Reference secrets without seeing them.** A `(secret)"name"` value wires a module option to a decrypted path (`config.sops.secrets."name".path`, or agenix), so the encrypted material stays out of band and knixl never reads or hashes the plaintext.
- **Bring your own modules.** Declare `modules { module "x" flake="github:org/x" }` in `knixl.kdl`; knixl resolves it to a pinned rev, caches the manifest, and records it in the lock, so `generate` stays offline and byte-reproducible. Precedence is built-in, then your local `modules/`, then fetched, then stdlib, and a shadowed module is reported, never silent (ADR 0010).

## The model in four points

- **KDL is authoritative.** Generated Nix is derived and disposable. There is no round-trip from edited Nix back to KDL (that is a tar pit, see ADR 0001).
- **Override via the module system, not by editing generated files.** Anything expressed as a NixOS option is overridable from a sibling module with `lib.mkForce` / `lib.mkAfter`. Structural choices (which files, which modules) change at the KDL layer.
- **Escape hatch:** a `raw-nix` passthrough node for inline snippets, or just import a hand-written `.nix` module alongside. knixl does not need to model all of Nix, only provide a clean seam.
- **Reproducible + version-aware:** `output = f(kdl, tool_version, module_versions, formatter_version, oracle_rev)`, deterministic to the byte. A lockfile pins all five. Regeneration is a reconcile, and a version bump is opt-in and reviewable.

## Commands

- `knixl check` : CI gate. Exits 0 only if every generated file matches the lock. Never writes.
- `knixl plan` : recompute and report, write nothing.
- `knixl generate` : apply. Silent for input changes, refuses hand-edited (tainted) files without `--accept-drift`, refuses version skew (points you at `upgrade`).
- `knixl upgrade` : the only path that bumps recorded versions. Shows migration notes and a diff, applies on `--yes`.
- `knixl doc <node>` : typed reference for a module node, generated from its schema.
- `knixl install <pkg>` : add a package to a host. Drafts the KDL, verifies it under nix, previews, then regenerates. `pkg@version` pins the package to a nixpkgs commit. On a real terminal it opens the TUI install screen; piped or `--yes` it uses a plain confirm.
- `knixl tui` : the interactive hub (shown above). Install a package, browse registered modules and their schemas, or author a new declarative module (build its schema and emit template, validated live as you type).

## Drift and versions

A generated file that someone hand-edited is **tainted**. knixl tells drift apart from a stale input by a third hash, and refuses to silently overwrite a human's edit:

<p align="center">
  <img src="assets/drift-demo.gif" alt="knixl catches a hand-edited generated file and refuses to overwrite it" width="820">
</p>

## Key terms

- **Clean** : the generated file matches what its inputs and versions produce. Nothing to do.
- **Stale** : an input (KDL) changed, so a regeneration is owed. `generate` fixes it.
- **Drifted** : the generated file was hand-edited (tainted). `generate` refuses it without `--accept-drift`.
- **Missing** : the file should exist and does not. **Orphaned** : it exists but no host claims it (deleted only with `--prune`).
- **taint** : the whole-file drift concept. A hand-edit taints the file, because a partial re-merge would lose the edit silently (ADR 0004).
- **oracle** : the NixOS option set knixl validates emitted paths against, built from a pinned nixpkgs rev (ADR 0003).
- **baseline** : a per-host nixpkgs rev. A host may declare `nixpkgs release="25.05"`, resolved to a commit at install/upgrade time (ADR 0007).
- **pin strategy** : how a version-pinned package is emitted, `override` or `commit-mix`, chosen automatically at pin time (ADR 0005/0006).

## Examples

`examples/` holds worked hosts (`db`, `web`, `shared`, `pinned`, `pinned-override`, plus `nas`, `gateway`, `vault`, `vmhost`, `workstation` exercising the newer modules) with their golden Nix output under `examples/expected/` and the matching lock. They double as the acceptance tests, and run with nothing beside them: every module they use is part of the embedded stdlib. See examples/README.md.

## Status

1.0.0 is released (on crates.io, with prebuilt binaries). `check`, `plan`, `generate`, `upgrade`, `doc`, `install`, and `tui` work; every example host reproduces byte-for-byte through the pinned nixfmt; the oracle validates emitted paths against the NixOS option set; and the stdlib covers disks (disko), ZFS, users, OpenSSH, Tailscale, Incus, home-manager, and nginx, with opt-in bootable-system flake emission, reference-by-name secrets, and flake-based fetched modules. Design specs are under `docs/superpowers/specs/`; new work is tracked in GitHub issues.

## Prior art

Nothing does KDL to committed Nix source. The ecosystem goes the other way (home-manager `toKDL`, niri-flake, the `pkgs.formats.kdl` request all generate KDL from Nix). Nickel exports to JSON/YAML/TOML, not Nix source. dhall-to-nix emits Nix but at eval time as normalised values, and cannot express callPackage, overlays, or the module system. terranix is the closest structural precedent (a DSL compiling to target config), just mirrored. See docs/00-overview.md for the full write-up.

## Licence

Licensed under either of Apache License, Version 2.0 (LICENSE-APACHE) or the MIT licence
(LICENSE-MIT) at your option. Unless you state otherwise, any contribution you submit for
inclusion is dual licensed as above, with no additional terms.
