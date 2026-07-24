# ADR 0010: Module distribution and precedence

Status: accepted

Relates to: ADR 0008 (out-of-tree oracle modules).

## Context

knixl's declarative module system (docs/03) supports two sources of modules:

1. Built-in Rust modules, compiled into the binary.
2. Declarative modules written in KDL, scanned from `modules/` at startup.

A project may also want to consume modules written and versioned by others, distributed as flakes (like the oracle modules in ADR 0008). Currently, the only place to put those is the local `modules/` directory, checked into the project repository. That binds their lifecycle to the project's, prevents sharing and reuse, and means every project hosting an external module must commit a copy of it.

The module system also grows when built-ins, local declarative modules, and fetched modules coexist: if two layers define the same node name (e.g. both local and fetched define `postgres`), the expectation should be stable and explicit, not silently whoever registers last wins.

## Decision

Modules are sourced from four layers, ordered by precedence (highest to lowest):

1. **Built-in** (Rust modules compiled into the binary)
2. **Local** (`<project>/modules/*`, declarative KDL modules)
3. **Fetched** (declared in `knixl.kdl`, resolved and pinned at install/upgrade)
4. **Embedded stdlib** (curated declarative modules bundled in the binary, via `include_dir`)

Each layer is scanned in reverse precedence order (stdlib first, then fetched, then local, then built-in). The first layer to claim a node name wins; any later layer claiming the same name is shadowed. When a shadow occurs, the generator emits a notice in the generated code naming the winning layer, the shadowed layer, and the module name, so hand-readers and auditors see the precedence is not accidental.

### Embedded stdlib

The repository's `modules/` directory (under version control in the knixl repository, not the project repository) is the single source of truth for the curated, versioned baseline modules. These are bundled into the knixl binary via `include_dir`, registered on startup at the lowest precedence, and carry no external dependency or network fetch. Projects see them exactly as the knixl version provides; they cannot be overridden except by local or fetched modules claiming the same node name.

### Fetched modules

A project may declare fetched modules in its `knixl.kdl`:

```kdl
modules {
  module "name" flake="<flake-ref>" [path="path/in/flake"]
}
```

Each declared flake ref resolves to a pinned `{url, rev}` pair at `install`/`upgrade` time (mirroring ADR 0005/0008). The manifest (the `knixl-module.kdl` file contents) is cached locally by `(rev, path)`, keyed by content hash, and the lock records a `module-source` line per project-level fetched module:

```
module-source "name" url="..." rev="..." path="..." hash="..."
```

`generate` loads fetched modules offline from the cache, verifies the manifest hash against the pin, and refuses with a hard error if they mismatch (no silent refetch). A declared module with no lock pin fails at generate time with exit 5 (Validation), telling the user to run `install` or `upgrade` to resolve it.

### Composition

Registration order and resolution happen once at startup. A module in a lower layer may depend on attributes or re-use emitted by a module in a higher layer (they all see the same `LowerCtx`); shadowing is purely a name-collision rule, not a hierarchy, and all layers that do not shadow are active.

## Consequences

- The curated stdlib is now part of the knixl release (versioned with the tool) and is distributed with every user, reducing the barrier to entry for new projects.
- Projects can extend or override the stdlib by adding local declarative modules to `modules/`.
- Third-party modules can be distributed as flakes, fetched and pinned like oracle modules, with the same determinism and offline-loading guarantees.
- Shadowing is non-silent: a generated file that names the precedence lets auditors and future maintainers see that a choice was made, not forgotten.
- A module's place in the precedence stack is determined by its layer, not registration order or commit dates, so the precedence is stable and reproducible.
- Deferred: merging (rather than shadowing) modules across layers; per-project layer orderings; and lazy loading of fetched modules only when referenced.
