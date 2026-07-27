# ADR 0013: Guest-image targets (lxc images for Incus)

Status: accepted

Relates to: ADR 0012 (installer media), ADR 0011 (nspawn guests), ADR 0009 (assembly flake).

## Context

ADR 0011's `guest` module builds systemd-nspawn `containers.<name>`, re-rooted into
the host. That is not the only kind of guest: an Incus guest is a NixOS system
built as a standalone **lxc image** (a metadata tarball plus a rootfs tarball) that
Incus imports and launches. The homelab's `llm` guest is this second kind
(`nixos-generators -f lxc`, launched under a ROCm profile), so it stayed
hand-written even after ADR 0011 (issue #75).

Structurally this is installer media (ADR 0012) with a different build output: a
top-level block in `knixl.kdl` whose body is an ordinary module tree, lowered like
a host (not re-rooted) into a generated module, plus a flake output that builds the
image. So rather than copy the installer path, the two are generalised into one
**image-target** concept.

## Decision

An image target is a top-level block in `knixl.kdl` whose children are ordinary
knixl module nodes, lowered through the module registry (like a host, at top
level) into a generated module file, with a base module imported via `modulesPath`
ahead of the tree. Two kinds share one code path (`generate_image_targets`,
`FlakeImage`):

| Kind | KDL block | base import | generated file | flake package output(s) |
|------|-----------|-------------|----------------|--------------------------|
| Installer (ADR 0012) | `installer "<n>"` | `installer/cd-dvd/installation-cd-minimal.nix` | `generated/installer/<n>.nix` | `<n>-iso` = `config.system.build.isoImage` |
| Guest-lxc (this ADR) | `guest-image "<n>"` | `virtualisation/lxc-container.nix` | `generated/guest-image/<n>.nix` | `<n>-lxc` = `config.system.build.tarball`, `<n>-lxc-metadata` = `config.system.build.metadata` |

Both require `system {}` (they need the assembly flake to pin nixpkgs and carry the
output, ADR 0009), pin to the project's nixpkgs rev, and get a
`nixosConfigurations.<name>` entry alongside the package output(s).

### Drive the lxc build directly, no nixos-generators

The image is built straight from `lxc-container.nix`'s own build products
(`config.system.build.tarball` + `.metadata`), not by depending on
`nixos-generators`. That is what `nixos-generators -f lxc` wraps anyway, and it
keeps the generated flake self-contained (no extra flake input), matching how the
installer ISO output avoided pulling in a generator.

Two outputs are emitted (rootfs + metadata) because `incus image import` takes
both. Combining them into a single derivation was considered and rejected for v1:
it adds a `runCommand` wrapper to the generated flake for no functional gain over
naming the two products.

### Produce the image, not the launch

knixl produces the image. Importing and launching it (`incus image import`,
`incus launch -p default -p rocm`, profile wiring) stays out: that is
imperative/incus-admin territory, better left to the operator or a separate tool,
as the issue itself recommends.

## Consequences

- One image-target code path serves both installer ISOs and Incus lxc images; a
  future format (e.g. a raw VM image) is a new `ImageKind` variant, not a new path.
- Guest-image config re-uses the whole module registry, same as installers, with a
  `raw-nix` seam available for the parts NixOS options do not model (the homelab's
  ROCm/ollama bits).
- The crate tests cover the KDL -> module -> flake wiring. The build attributes
  themselves were confirmed against real nixpkgs (unstable, 26.11pre) by
  `nix eval`: a config importing `virtualisation/lxc-container.nix` has both
  `config.system.build.tarball` and `.metadata` as derivations, and an
  `installation-cd-minimal` config has `config.system.build.isoImage`. Full
  end-to-end `nix build` + `incus image import` is left to the operator (building
  a whole system is heavy), but the attribute references are not guesswork.
- Deeper Incus integration (profiles, launch) is deliberately excluded.
