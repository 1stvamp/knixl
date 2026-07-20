# 06: The oracle

The oracle validates every emitted option path against real NixOS option data, so a wrong or renamed option fails at generation time with a KDL span, not at `nixos-rebuild`. Sketch in `crates/knixl-oracle/src/`.

## Why validate against real options, not hand-written schemas

Hand-written per-module output schemas would force a recompile per module and duplicate what NixOS already publishes. Instead, extract the option set NixOS itself documents and use it as the type oracle. Then the opinionated modules become curated presets on top of the full, already-typed option namespace, and the arbitrary-option escape hatch is validated too, for free, with the option docs coming along.

## Extraction

Use `nixosOptionsDoc`, the same mechanism behind search.nixos.org and the manual. It produces an `options.json` mapping each option path to a type description, default, and declaration site. The caller pins the nixpkgs rev, so the oracle is reproducible, and that rev goes in the lock (`oracle nixpkgs-rev=... options-hash=...`).

## Rev cache

`options.json` is cached keyed by rev at `$XDG_CACHE_HOME/knixl/options-<rev>.json` (falling back to `$HOME/.cache/knixl/`). The set for a given rev is identical across projects, so a user-level cache is fetched once and reused. Resolution order when planning:

1. `KNIXL_OPTIONS_JSON` if set: an explicit path, and what the tests use. Wins over everything.
2. Otherwise the file cached for the lock's `oracle nixpkgs-rev`, loaded automatically so a `check` validates against the locked options with no env var.
3. Otherwise no options file: generation proceeds without option checks (best-effort, the same as an unknown rev).

Populate the cache for a rev with `nixosOptionsDoc`, e.g. build the doc against the pinned nixpkgs and copy the result:

```
nix-build '<nixpkgs>' -A nixosOptionsDoc ...   # or an equivalent flake attr
cp result/share/doc/nixos/options.json "$HOME/.cache/knixl/options-<rev>.json"
```

Fetching by rev is not yet automated inside knixl (it needs a nix evaluation); the lookup is. `knixl_oracle::cache_path(rev)` returns the exact path to write.

## Per-host baselines

A single global oracle rev is not always enough: fleets migrate host by host, not all at once. A host can declare its own nixpkgs release with a `nixpkgs` node, e.g. `nixpkgs release="25.05"` inside `host "shared" { ... }` (`examples/hosts/shared.kdl`). That host is then validated against its own release's option set instead of the project-wide one; a host with no declared release simply falls back to the global oracle rev.

Declaring a release does not resolve it on its own. The release string (`"25.05"`) has to become a nixpkgs commit, and that resolution is recorded per host in the lock as a `baseline` line (`release`, `nixpkgs-rev`, `options-hash`; see docs/02). Resolution goes through `KNIXL_BASELINE_RESOLVER` if set (an external `<bin> <release>` command), otherwise a built-in resolver: `git ls-remote` against the `nixos-<release>` branch, falling back to the GitHub commits API if `git` is unavailable or fails. A failure to resolve blocks, it never guesses.

Planning keys the option set per host: `gather` (the read side of `Plan::compute`, in `crates/knixl-pipeline/src/gather.rs`) builds a `BTreeMap<String, Oracle>` from host name to oracle, each host's rev taken from its lock baseline if declared, else the lock's default `oracle nixpkgs-rev`. A host absent from the map (nothing cached for its rev) generates without option checks for that host alone, the same best-effort fallback as the single-oracle case, just scoped to one host instead of the whole project.

## The honest limit, and how to live with it

In `options.json` the option *type* is a human-readable string (`"boolean"`, `"list of string"`, `"attribute set of submodule"`), not a structured type. So the oracle does best-effort structural checking, not full inference:

- It reliably catches unknown option paths (the single most common generated-Nix bug: a wrong or renamed path).
- It catches gross type mismatches (a string where a boolean is required).
- It punts on submodule interiors, returning `Ok` for anything it cannot parse (`NixType::Unknown`).

That is still most of the value. Do not over-invest in parsing every type description string. The path-existence check earns its keep on its own.

## Shape

- `Oracle::from_options_json(path)` builds a `BTreeMap<String, OptionInfo>` keyed by option path (`"services.nginx.enable"`).
- `Oracle::check(path, value)` collapses dynamic quoted keys to `<name>` via `AttrPath::to_option_key()`, looks up the option, and returns:
  - `UnknownOption` if the path is not in the set,
  - `ReadOnly` if the option is read-only,
  - `WrongType` if the parsed `NixType` rejects the value,
  - `Ok` if the type is `Unknown` (punt) or accepts the value,
  - `Ok` if the path is not itself a leaf option but is a strict prefix of a real one: an intermediate attrset such as a dynamic-key submodule root (e.g. `services.restic.backups.<name>`), detected via `is_option_prefix`. The interior stays unchecked; a genuine typo has no known children and is still rejected.
- `NixType::parse_description(s)` is best-effort: `"boolean"` -> `Bool`, `"list of string"` -> `List(Str)`, `"null or (attribute set of package)"` -> `NullOr(AttrsOf(Package))`, `"one of ..."` -> `Enum`, anything else -> `Unknown(s)`.

## What it cannot catch

Value conflicts between two modules assigning the same path (both `mkForce`, say) are not type errors and the oracle cannot see them. That is the plan-time cross-module lint's job (docs/02, docs/03). Keep the two concerns separate.
