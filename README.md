# knixl

knixl generates maintainable, human-readable Nix from small amounts of opinionated KDL.

Pronounced "nickle" or "nixell". Written in Rust. KDL is the source of truth, the generated Nix is a committed build artefact, and regeneration is version-aware so a framework upgrade cannot change your output without telling you first.

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

## Status

Design-complete, not yet implemented. See HANDOFF.md for exactly what exists and what is still a sketch, and NEXT-STEPS.md for the ordered backlog. The Rust under `crates/` is specification-grade: it will not compile as-is (elided bodies, cross-crate wiring pending). Making it compile is task one.

## Prior art

Nothing does KDL to committed Nix source. The ecosystem goes the other way (home-manager `toKDL`, niri-flake, the `pkgs.formats.kdl` request all generate KDL from Nix). Nickel exports to JSON/YAML/TOML, not Nix source. dhall-to-nix emits Nix but at eval time as normalised values, and cannot express callPackage, overlays, or the module system. terranix is the closest structural precedent (a DSL compiling to target config), just mirrored. See docs/00-overview.md for the full write-up.

## Licence

Intended: Apache-2.0 (matches the `kdl` crate). Not yet applied.
