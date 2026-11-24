# 00: Overview and prior art

## The niche

knixl compiles opinionated KDL into maintainable, committed Nix source. The input direction (KDL to Nix) is essentially unoccupied. Everything mature in the space points the other way, or bridges at evaluation time rather than emitting source you would commit and read.

That gap is the reason to build it.

## What exists, and why none of it is this

- **home-manager, niri-flake:** both generate KDL as an output format from Nix data (home-manager's `lib.hm.generators.toKDL`, niri-flake's `niri.lib.kdl`). The mirror of what knixl does. A `formats.kdl` for nixpkgs has been proposed but not merged. Worth reading the home-manager PR that added the generator (nix-community/home-manager#3399) anyway: it works through the KDL-to-data mismatches (repeated nodes, args vs properties, JSON-in-KDL, bool-vs-int coercion) that knixl hits in reverse. That discussion is a free test corpus.
- **Nickel (Tweag):** the closest philosophical precedent, a config language its README calls "an evolution of the Nix language, while trying to overcome some of its limitations". It exports to JSON, YAML, TOML or plain text, never Nix source, and its Nix interop (experimental) imports Nix into Nickel for evaluation. Opposite direction again.
- **dhall-to-nix:** the one thing that emits Nix, and instructive because its README calls it a proof of concept and lists what Dhall cannot encode. It prints Nix expressions for normalised Dhall values, and nixpkgs uses it at eval time via import-from-derivation, so in practice its output is unformatted and never committed. Dhall cannot encode general recursion (`pkgs.callPackages`, the overlay system, the NixOS module system), weakly-typed conversions (`builtins.listToAttrs`) or row polymorphism. So it is a value bridge rather than a config generator. The lesson: do not try to be a general Nix-expression compiler.
- **terranix:** the closest structural precedent. It compiles a Nix DSL to `config.tf.json`. Same shape as knixl (DSL compiles to target-language config via an explicit generation step), just a different source and target. Its generate-commit-apply ergonomics transfer directly.
- **Nixtamal:** a KDL manifest with a diff-friendly (JSON) lockfile for Nix input pinning. Precedent for "KDL manifest plus grep-friendly lockfile" as an ergonomic pattern, close to the knixl lock design.

## Rust tooling

Use the official `kdl` crate (kdl-rs). It is formatting-preserving (think toml_edit for KDL), defaults to v2.0.0 with optional v1 and v1-fallback, Apache-2.0, and already integrates miette for pretty diagnostics, which knixl wants for good input errors. `just-kdl` is faster but drops formatting and span info, so only reach for it if profiling demands it: spans matter on the input side.

There is no mature Nix-emitter crate to depend on, so knixl builds its own IR plus a deterministic printer, then delegates final formatting to a pinned `nixfmt-rfc-style` (the canonical formatter post-RFC-166; alejandra is the alternative).
