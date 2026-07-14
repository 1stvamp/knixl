# 06: The oracle

The oracle validates every emitted option path against real NixOS option data, so a wrong or renamed option fails at generation time with a KDL span, not at `nixos-rebuild`. Sketch in `crates/knixl-oracle/src/`.

## Why validate against real options, not hand-written schemas

Hand-written per-module output schemas would force a recompile per module and duplicate what NixOS already publishes. Instead, extract the option set NixOS itself documents and use it as the type oracle. Then the opinionated modules become curated presets on top of the full, already-typed option namespace, and the arbitrary-option escape hatch is validated too, for free, with the option docs coming along.

## Extraction

Use `nixosOptionsDoc`, the same mechanism behind search.nixos.org and the manual. It produces an `options.json` mapping each option path to a type description, default, and declaration site. The caller pins the nixpkgs rev, so the oracle is reproducible, and that rev goes in the lock (`oracle nixpkgs-rev=... options-hash=...`). Cache `options.json` keyed by rev under `.knixl-cache/`.

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
  - `Ok` if the type is `Unknown` (punt) or accepts the value.
- `NixType::parse_description(s)` is best-effort: `"boolean"` -> `Bool`, `"list of string"` -> `List(Str)`, `"null or (attribute set of package)"` -> `NullOr(AttrsOf(Package))`, `"one of ..."` -> `Enum`, anything else -> `Unknown(s)`.

## What it cannot catch

Value conflicts between two modules assigning the same path (both `mkForce`, say) are not type errors and the oracle cannot see them. That is the plan-time cross-module lint's job (docs/02, docs/03). Keep the two concerns separate.
