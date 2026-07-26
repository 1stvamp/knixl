# ADR 0011: Guests as re-rooted nested module trees

Status: accepted

Relates to: ADR 0002 (emit source, not values), docs/03 (module system).

## Context

knixl models NixOS hosts. A homelab often also runs guest systems: NixOS system
containers (`containers.<name>` in nixpkgs, backed by systemd-nspawn), where each
guest carries its own full NixOS configuration under `containers.<name>.config`.
Before this, a guest stayed entirely hand-written (issue #64): knixl could emit a
host but had no way to express a system inside a system.

A guest's config is, structurally, another host: a set of NixOS modules
(`services.*`, `users.*`, boot, packages) that must land under
`containers.<name>.config.*` instead of at the top level. The single-level
declarative template grammar cannot express this, and re-implementing every
service module for the nested case would be absurd.

## Decision

A built-in `guest "<name>"` module lowers a nested knixl module tree and
**re-roots** its output under `containers.<name>.config`.

- A `config { ... }` sub-block holds ordinary knixl module nodes (`os`, `user`,
  `openssh`, `web-service`, ...). The guest lowers them through the same
  `ctx.lower_children` path a host uses, so the entire module registry is reused
  recursively: a guest is a mini-host.
- Each assignment the nested modules produce is re-rooted by prepending
  `containers.<name>.config` to its attribute path. `web-service` inside a guest
  therefore yields `containers.<name>.config.services.nginx.enable = true;`, which
  is valid NixOS (the container's `config` is a submodule that merges such paths).
- Guest-level envelope options (not part of the nested config) map to
  `containers.<name>.<opt>`: `autostart` (`autoStart`), `ephemeral`,
  `private-network` (`privateNetwork`), `host-address`/`local-address`
  (`hostAddress`/`localAddress`), and repeated `bind-mount "/path" { host-path;
  read-only }` (`bindMounts."/path"`).

The re-rooting module is the novel piece: a module that consumes another module
tree's output and relocates it under a sub-path, rather than emitting fixed
option paths of its own.

### Oracle exemption for nested config paths

`nixosOptionsDoc` types `containers.<name>.config` as a single `submodule`
option; the inner paths (`...config.services.nginx.enable`) are not keys in the
built `options.json`. Validating them against the flat option set would fail every
path with `UnknownOption`. So paths under `containers.*.config` are **exempt** from
oracle validation (docs/06), the same opaque treatment `raw-nix` gets (ADR 0004):
knixl cannot cheaply model a submodule's interior, so it does not pretend to. The
envelope options (`containers.<name>.autoStart`, etc.) are ordinary flat paths and
stay validated.

### v1 scope

The nested config re-roots `Bucket::Default` assignments only. A guest child that
produces a named side-file bucket or raw-nix passthrough is rejected: side-files
cannot nest into a container's config, and opaque raw text cannot be re-rooted by
path. These can be revisited if a real need appears.

## Consequences

- A guest's config is first-class knixl: every existing and future module works
  inside a guest with no per-module nesting support.
- Drift/reproducibility are unchanged: re-rooted assignments hash into the host
  file like any other output.
- The oracle exemption is a real gap: a typo'd option path inside a guest's config
  is not caught. Documented, and consistent with the `raw-nix` trade-off.
- Deeper nesting (a guest inside a guest) falls out of the recursion for free, but
  is untested territory in v1.
