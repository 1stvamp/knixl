# ADR 0007: Per-host baseline nixpkgs rev

Status: accepted

Refines: ADR 0003 (validate against real NixOS options). Relates to: ADR 0005 (pinning).

Refined by: ADR 0008 (2026-07-21): a host's baseline also carries its own out-of-tree oracle
module pins when it declares an override, alongside the release/rev/options-hash this ADR
added.

## Context

knixl validates every emitted option path against a NixOS option set built from one
global nixpkgs rev (`lock.oracle`, ADR 0003). Every host is checked against that single
rev, and the same rev is the "baseline" the pin-strategy feasibility test builds against
(ADR 0006). A fleet often spans NixOS releases: one host on `nixos-25.05`, another on
`nixos-24.11`. A single global rev validates both against the wrong option set for at
least one of them, and builds pin feasibility against a baseline that is not the host's.

ADR 0005 deferred per-host baseline revs. This lifts that deferral.

## Decision

A host may declare its own baseline nixpkgs release; knixl validates that host against
that release's option set and treats that rev as the host's pin baseline.

- **KDL declares intent**: an optional `nixpkgs release="<rel>"` node on a host (e.g.
  `nixpkgs release="25.05"`). A host without it uses the global `oracle` rev, so existing
  projects are unchanged.
- **Resolution is at pin time only** (`install`/`upgrade`): the release resolves to the tip
  commit of the `nixos-<rel>` branch, recorded per host in the lock. `generate`/`check`
  stay offline; a declared release with no lock entry is a validation error pointing at
  `upgrade`, exactly as an unresolved package pin is (ADR 0005).
- **The lock records it per host**: the `host "<name>"` block gains a
  `baseline release="<rel>" nixpkgs-rev="<commit>" options-hash="<hash>"` line beside its
  `pin` lines. The global `oracle` remains the default baseline for undeclared hosts.
- **Validation is per host**: each host is validated against an oracle built from its own
  rev (`Oracle::from_rev_cache(rev)`, best-effort as today, the cache already keys by rev).
  The rev is also the host's pin-strategy feasibility baseline (ADR 0006), retiring that
  ADR's "builds against the builder channel" limitation for hosts that declare a release.
- **Emit is unchanged**: the baseline rev drives validation and pin feasibility only.
  Unpinned packages still emit ambient `pkgs`; knixl does not take over the host's nixpkgs
  (that would contradict the "emit a NixOS module the system evaluates" model, ADR 0002).
- **Resolution mechanism**: a release resolves via an injected command
  (`KNIXL_BASELINE_RESOLVER`) when set; otherwise the built-in resolver runs
  `git ls-remote https://github.com/NixOS/nixpkgs refs/heads/nixos-<rel>` and, if git is
  absent or fails, falls back to the GitHub commits API over `ureq`. Mirrors the pin
  resolver (ADR 0005 / #21).

## Consequences

- A fleet spanning releases is validated correctly per host, and cross-rev pin feasibility
  is tested against the host's real baseline.
- The reproducibility boundary widens per host: each declared host's `nixpkgs-rev` and
  `options-hash` join the lock. Changing a host's release is an `upgrade`-gated event
  (skew), never a silent `generate`.
- knixl gains a soft dependency on git (or the GitHub API) at pin time only, to resolve a
  release to a commit. Offline, a release cannot be resolved (a clear error); it never
  resolves to a wrong commit silently.
- Validation stays best-effort: if a host rev's `options.json` is not cached, that host's
  validation is skipped (as with the global oracle today), not failed.
- Still deferred: driving unpinned package emit from the baseline rev, and a raw
  `nixpkgs rev="<commit>"` escape hatch (release channel is the declared surface for now).
