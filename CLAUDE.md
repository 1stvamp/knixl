# CLAUDE.md

Project instructions for knixl. This sits under the global `~/.claude/CLAUDE.md` (which already governs style, anti-sycophancy, engineering defaults, and git behaviour). This file is project-specific and does not repeat the global rules.

## What knixl is

A Rust tool that compiles opinionated KDL into maintainable, committed Nix source, with a lockfile-backed reproducibility and drift-detection model. Read README.md, then docs/ in order, then HANDOFF.md before writing code.

## Ground truth, in priority order

1. `docs/adr/` : the decisions that must not be quietly reversed. If a task seems to require reversing one, stop and raise it, do not just do it.
2. `docs/01-architecture.md` through `docs/06-oracle.md` : the specs.
3. `crates/` : specification-grade sketches. Signatures and control flow are the intent; bodies marked `/* ... */` and helpers like `assign(..)` / `child_arg_str(..)` are not yet written.
4. `examples/` : the behaviour contract. The generated `.nix` under `examples/expected/` is what the pipeline must reproduce from the `.kdl` inputs. Treat these as golden tests.

## Non-negotiable invariants

- KDL is authoritative, generated Nix is derived. No Nix-to-KDL round-trip. Ever. (ADR 0001)
- Emit Nix source text, not values. Readable, commented, committable. (ADR 0002)
- Generation is deterministic to the byte: no `HashMap` iteration in emit paths (use `BTreeMap` or index-preserving structures), a defined attr sort order, stable list order from KDL source order. This is what the lock depends on, so it is not optional.
- The formatter and the oracle nixpkgs rev are part of the reproducibility boundary. Pin both in the lock. A check that passes under one and fails under another is a bug, not a nuisance.
- `Plan::compute` is pure: it reads the world, generates expected output, compares. It writes nothing. Every command is a thin policy over the same `Plan`.
- `Stale` (input changed) and `Drifted` (generated file hand-edited) are different states told apart by a third hash. Do not collapse them. Drift is the taint concept, and silently overwriting a drifted file loses a human's edit.

## Build and test expectations

- Workspace crates depend one direction only: `cli` on everything, `modules` on `ir` + `oracle`, `lock` on `nix`, `ir` on nothing but `miette`/`semver`. No crate imports `cli`. Keep it that way so the library stays reusable (LSP, GitHub Action later).
- The examples are the acceptance tests. Wire them as golden tests early: parse `examples/hosts/*.kdl`, generate, format, compare against `examples/expected/*.nix`, and diff the produced lock against `examples/knixl.lock.kdl`.
- Determinism test: generate twice, assert byte-identical. Then generate, permute internal collection order under a feature flag, assert still byte-identical.

## First tasks (see NEXT-STEPS.md for the full ordered backlog)

1. Make the workspace compile: fill helper bodies, wire cross-crate types, get `cargo build` green with stubbed `lower()` bodies.
2. Implement `knixl-ir` emit fully (the `Emit` trait + escaping + float formatting + attr-key classification). Unit-test round-trips.
3. Implement `Plan::compute` (the three-hash `FileState` derivation) and `knixl check`.
4. Implement the `nixosOptionsDoc` extraction in `knixl-oracle` with the rev pinned in the lock.

## House style for this repo

- British spelling in all prose and comments. No em-dashes or en-dashes anywhere: colons, parentheses, commas, full stops.
- Banned vocabulary (docs, comments, commit messages): passionate, leverage, robust, seamless, delve, and the AI-smell set. See the global CLAUDE.md and the writing-voice skill.
- Commit messages: bottom line first, present tense, no filler. One logical change per commit.
- Doc comments earn their place: explain why, not what the code already says.
