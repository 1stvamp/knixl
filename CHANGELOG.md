# Changelog

All notable changes to knixl are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and knixl uses
[semantic versioning](https://semver.org/spec/v2.0.0.html). Release notes on
GitHub are generated from the matching section here (cargo-dist reads this file);
see `docs/release-changelog.md` for how each entry is written.

## [Unreleased]

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

[Unreleased]: https://github.com/1stvamp/knixl/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/1stvamp/knixl/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/1stvamp/knixl/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/1stvamp/knixl/releases/tag/v1.0.0
