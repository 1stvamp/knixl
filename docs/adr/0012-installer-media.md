# ADR 0012: Installer media (bootable ISO)

Status: accepted

Relates to: ADR 0009 (system-assembly flake), ADR 0011 (nested module trees).

## Context

Deploying a knixl-managed host to bare metal needs an installer: a bootable
image that comes up far enough for `nixos-anywhere` (or a hand-run
`nixos-install`) to reach it. A common pattern (the homelab's `installer/iso.nix`)
is a self-bootstrapping ISO that joins a tailnet on boot, so the target is
reachable over the tailnet without console access. Before this, that ISO stayed
hand-written; knixl modelled hosts but not installer media (issue #65).

An installer is structurally a NixOS configuration like a host, with two
differences: it imports the nixpkgs `installation-cd` base module, and it builds
to an ISO image rather than a switchable system.

## Decision

A top-level `installer "<name>" { ... }` block, declared in `knixl.kdl`, whose
body is an ordinary knixl module tree (`tailscale`, `openssh`, `user`, `os`, ...).
It is lowered through the normal module registry (like a host, at top level, not
re-rooted) into a generated module file `generated/installer/<name>.nix`, which:

- takes `{ modulesPath, config, lib, pkgs, ... }` formals, and
- `imports = [ (modulesPath + "/installer/cd-dvd/installation-cd-minimal.nix") ]`,
  the minimal installation-cd base, ahead of the lowered module tree.

The generated assembly flake (ADR 0009, emitted only when `system {}` is declared)
gains, per installer:

- a `nixosConfigurations.<name>` entry, evaluated at the project's nixpkgs rev via
  the same `eval-config.nix` constructor the hosts use, with the installer module
  and `installation-cd` imported; and
- a `packages.<system>."<name>-iso"` output equal to that configuration's
  `config.system.build.isoImage`, so `nix build .#<name>-iso` produces the image.

An installer therefore requires `system {}` (it has nowhere to pin nixpkgs or emit
the flake output otherwise), the same precondition ADR 0009 sets for hosts.

### Build-time auth key, not a runtime secret

A live ISO has no sops/agenix infrastructure, so the `(secret)` reference-by-name
mechanism (which points at `config.sops.secrets.<name>.path`) does not work in an
installer. The tailnet auth key an installer bakes in is supplied at build time: a
plain file path or a build argument, not a runtime secret. The `(secret)` form
stays correct for running hosts; installers document the build-time path instead.
(In v1 the installer simply carries whatever the `tailscale` module emits; wiring
a build-time key file is a follow-up if the plain path proves insufficient.)

### v1 scope

- Installers are declared in `knixl.kdl` and require `system {}`.
- Single target system per installer (the project's `system {}` platform).
- The module tree re-uses every existing module; installer-specific base is only
  the `installation-cd-minimal` import.
- Installer modules are not oracle-validated in v1 (installers are not hosts, so
  they carry no per-host option set); the emitted paths are ordinary nixpkgs
  options and are checked when the ISO is actually built by nix. Wiring a
  per-installer oracle is a follow-up.

## Consequences

- Installer media is first-class and reproducible: the ISO config hashes into the
  lock like any other generated file, and builds at the pinned nixpkgs rev.
- The assembly flake grows a `packages` output; it was `nixosConfigurations`-only.
- `modulesPath` in the installer file's formals is the first per-file formals
  variation; the emitter must support it.
- Deeper installer customisation (custom squashfs, extra ISO contents, a dedicated
  build-time key mechanism) is deferred until a concrete need appears.
