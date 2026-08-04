# Changelog

All notable changes to knixl are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and knixl uses
[semantic versioning](https://semver.org/spec/v2.0.0.html). Release notes on
GitHub are generated from the matching section here (cargo-dist reads this file);
see `docs/release-changelog.md` for how each entry is written.

## [Unreleased]

## [1.4.0] - 2026-08-04

A breaking change, released as a minor deliberately: KDL that knixl cannot fully
interpret is now refused instead of being silently dropped.

Read the first entry below before upgrading. A project carrying a stray or
misspelt node has been generating without it and will start failing, which is the
point of the change. Anyone resolving these crates as `^1` picks this up
automatically, both the CLI behaviour and the library API changes noted at the
end.

### Changed
- KDL that knixl cannot fully interpret now refuses to generate, with exit 5
  (#85). A node no module claims, or an unknown child of a claimed one, was a
  warning at any depth below the top level, so a typo (`timezon` for `timezone`)
  was dropped from the emitted Nix while `generate` wrote the file and `check`
  exited 0. The lock records the emitted Nix rather than the intent of the KDL, so
  nothing downstream could see the loss. `docs/05-cli.md` already documented exit
  5 as covering a KDL schema error; only the top-level path honoured it. There is
  no flag to downgrade this: an escape hatch would restore exactly the
  silently-wrong output. **A project carrying a stray or misspelt node has been
  generating without it and will now fail until the node is fixed or removed.**
- `--json` carries diagnostics: `{"files":[...],"warnings":[...]}` normally, and
  `{"validation":[...]}` when validation refused (#85). Both only existed on
  stderr, so CI could not branch on them.
- A top-level unclaimed node reports exit 5 rather than exit 1 (#85). It was
  raised as an internal error, when a typo in the input is validation.
- Library API, for anyone using the crates directly rather than the CLI:
  `knixl_modules::Diagnostic` gains a `severity` field (so constructing one by
  literal no longer compiles; a new `Severity` enum accompanies it), and
  `knixl_pipeline::GenerateError::UnknownNode` is removed in favour of
  `Validation` (#85).

## [1.3.0] - 2026-08-04

Four knobs that previously needed `raw-nix`, and an oracle that was rejecting
values NixOS accepts.

### Added
- `os` gains `session-variable "<NAME>"="<value>"`, a prop map emitting
  `environment.sessionVariables` (#86). NixOS writes those into
  `/etc/pam/environment`, which `pam_env` loads for every session including a
  non-interactive `ssh host cmd`; `environment.variables` reaches interactive
  shells only, so it is not offered. A name that is a valid bare Nix attribute
  renders unquoted.
- `os` gains repeated `kernel-module "<name>"` (`boot.kernelModules`), the other
  half of the existing `sysctl` child: a sysctl often only exists once its module
  is loaded, so `net.bridge.bridge-nf-call-iptables` needs `br_netfilter`
  alongside it (#87).
- `os` gains repeated `tmpfiles-rule "<path>" type="d" [mode=] [user=] [group=]
  [age=] [argument=]` (`systemd.tmpfiles.rules`), with the fields named because
  the bare tmpfiles line is not readable (#88). `type` is required; the rest
  default to `-`, tmpfiles' own leave-this-to-the-default marker. Rules keep KDL
  source order, since tmpfiles applies them in order.
- `nix-ld` module: repeated `library "<name>"` emitting `programs.nix-ld.enable`
  and `programs.nix-ld.libraries` (#89), for hosts that run binaries they did not
  build (a project-pinned rustup toolchain, a prebuilt release binary), where
  `/lib64/ld-linux-x86-64.so.2` is NixOS's `stub-ld`. The node's presence is the
  opt-in, so there is no `enable` child. Dotted names work, so
  `library "stdenv.cc.cc.lib"` emits `pkgs.stdenv.cc.cc.lib`.

### Fixed
- The oracle no longer rejects a legitimate value for an option whose type is a
  top-level union, which is how `nixosOptionsDoc` renders an `either` (#86, #87).
  `environment.sessionVariables` was typed as an integer and `boot.kernelModules`
  as an attribute set that refuses the list form, so both failed validation with
  `WrongType`. Any project setting an option of that shape, through knixl's own
  modules or a fetched one, was blocked.
- The oracle no longer panics on an option whose type description carries a
  multibyte character, such as nixpkgs' `3×3 matrix of floating point numbers`.
  Every command that loads the option set was affected.

## [1.2.1] - 2026-07-31

Image targets generate. Both kinds emitted invalid Nix in 1.2.0, so neither had
ever worked.

### Fixed
- `installer` and `guest-image` targets now generate (#81). The base module
  reached the generated `imports` list as a bare `modulesPath + "/..."`, which
  cannot be a list element, so every run aborted at the formatter and wrote
  nothing. Fixed in the emitter, so a raw seam holding a binary expression is
  bracketed wherever a single value is required.
- A formatter failure now reports what the formatter said (#81). `knixl plan`
  gave only `formatter exited non-zero: 1` and swallowed the stderr naming the
  line and column, which for the above meant the cause was invisible. A
  formatter that rejects its input can also exit before reading all of it, and
  the resulting broken pipe was reported in place of the real complaint.

## [1.2.0] - 2026-07-27

Incus lxc guest images, and one image-target code path behind them.

### Added
- `guest-image "<name>" [system=]` targets (#75): a NixOS system built as an lxc
  image for Incus, the sibling of the nspawn `guest` module. Alongside a
  `system {}` block the assembly flake gains `nixosConfigurations.<name>` and two
  package outputs, `packages.<system>."<name>-lxc"` (the rootfs) and
  `"<name>-lxc-metadata"`, so `nix build .#<name>-lxc` produces what
  `incus image import` takes. knixl builds the image; importing and launching it
  stays with the operator (ADR 0013).

### Changed
- The installer and guest-image paths are now one image-target abstraction
  (`ImageKind`), so a future image format is a new variant rather than a new code
  path (#75).

### Fixed
- Two image targets on the same system no longer emit a duplicate
  `packages.<system>` attribute in the assembly flake, which Nix rejects (#75).

## [1.1.0] - 2026-07-26

The homelab-migration feature set: the modules and knobs a real host needed that
were previously worked around with `raw-nix` (#57-#65).

### Added
- `os` module: core host config in one place, `system.stateVersion`, boot loader,
  kernel package, `time.timeZone`, `i18n.defaultLocale`, `boot.kernel.sysctl`,
  `nix.settings`, `environment.systemPackages`, and `users.mutableUsers` (#59, #60).
- `guest` module: NixOS system containers, a nested module tree re-rooted under
  `containers.<name>.config` (#64, ADR 0011).
- `installer "<name>"`: bootable installer media, a generated `installation-cd`
  module plus a `.#<name>-iso` flake output (#65, ADR 0012).
- disko: pool `options`/`root-fs-options`, filesystem `mount-option`, and a
  configurable data-partition label on the `boot-root-zfs` preset (#57).
- A `(scalar)` template value form that emits a bound argument as its native Nix
  bool/int/string instead of a string, and zfs `force-import-root` on top of it
  (#58).
- tailscale `open-firewall` (#63) and user `hashed-password` (#61).

### Changed
- incus is now a built-in module: the daemon preseed gained optional bridge ipv6,
  a `core.https_address` API listener (static or bound to an interface at runtime
  via a oneshot), and host-firewall integration (#62).
- Every host now sets `networking.hostName` from its label, overridable with
  `host { hostname "<name>" }`. Existing projects will see a one-line
  regeneration per host on the next `knixl upgrade`.

### Fixed
- The embedded stdlib is self-contained in the published crate, so a fresh
  `cargo install knixl` builds (#56).

## [1.0.0] - 2026-07-24

First stable release. knixl compiles opinionated KDL into maintainable,
committed, nixfmt-formatted NixOS module source, with a lockfile-backed
reproducibility and drift-detection model.

### Added
- The generate/check/plan/upgrade/doc/install workflow, with stable exit codes,
  and the `Clean`/`Stale`/`Drifted`/`Missing`/`Orphaned` state model over a
  lockfile (whole-file taint, ADR 0004).
- Built-in and declarative modules: host, postgres, backups, package, raw-nix,
  web-service, security-headers, zfs, user, openssh, disko, tailscale, incus,
  home-manager.
- Oracle validation of emitted option paths against a pinned NixOS option set,
  including out-of-tree modules declared in `knixl.kdl` (ADR 0003, 0008).
- Package version pinning with automatic strategy selection, and a per-host
  nixpkgs baseline (ADR 0005, 0006, 0007).
- Reference-by-name secrets (sops-nix or agenix), `let`-hoisting of repeated
  values, an opt-in system-assembly flake (ADR 0009), and an embedded stdlib plus
  flake-based fetched modules (ADR 0010).
- A TUI for installing packages, browsing modules, and authoring declarative
  modules.
- Published to crates.io with prebuilt binaries for Linux (gnu and musl) and
  macOS on x86_64 and aarch64.

[Unreleased]: https://github.com/1stvamp/knixl/compare/v1.4.0...HEAD
[1.4.0]: https://github.com/1stvamp/knixl/compare/v1.3.0...v1.4.0
[1.3.0]: https://github.com/1stvamp/knixl/compare/v1.2.1...v1.3.0
[1.2.1]: https://github.com/1stvamp/knixl/compare/v1.2.0...v1.2.1
[1.2.0]: https://github.com/1stvamp/knixl/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/1stvamp/knixl/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/1stvamp/knixl/releases/tag/v1.0.0
