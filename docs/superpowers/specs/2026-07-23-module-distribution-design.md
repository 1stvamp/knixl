# Module distribution: embedded stdlib plus fetched modules

Closes #13.

## Problem

Only the compiled Rust built-ins ship with the binary. Declarative modules
(web-service, zfs, user, openssh, security-headers, tailscale, incus) are repo
files that `build_registry` finds only under `<project>/modules/*`. A user's own
project sees none of them unless they hand-copy the files, so knixl is barely
useful out of the box beyond the built-ins. #12 (home-manager) depends on a
distribution story.

Two mechanisms, both in this slice:

- an **embedded stdlib**: the curated declarative modules are compiled into the
  binary and available in any project, offline, with no copying.
- **fetched modules**: a project declares external module sources in `knixl.kdl`;
  knixl resolves them to a pinned rev, caches the module text, records the pin in
  the lock, and loads them at generate. This mirrors the existing out-of-tree
  `oracle-modules` mechanism (ADR 0008) exactly, for knixl's own declarative
  modules rather than NixOS option modules.

## Module precedence

A node may be claimed by at most one module. Four layers, highest precedence
first:

1. **built-in** (compiled Rust: host, postgres, backups, package, raw-nix, disko)
2. **local** (`<project>/modules/*/knixl-module.kdl`)
3. **fetched** (declared in `knixl.kdl` `modules { }`, resolved from the lock)
4. **stdlib** (embedded in the binary)

`build_registry` layers them in that order. A duplicate node *within* a layer is
a hard error (as today for local modules). A lower layer claiming a node already
taken by a higher layer is a **shadow**: the higher one wins and knixl emits a
notice (a warning surfaced like other lints), so shadowing is never silent. This
lets a user fork a stdlib or fetched module by dropping a local one of the same
node, and see that they have done so.

## Embedded stdlib

The curated set is every module currently under `modules/`: web-service, zfs,
user, openssh, security-headers, tailscale, incus. The repo `modules/` tree stays
the single source of truth (the golden tests already validate it); the binary
embeds it at compile time.

- Add the `include_dir` crate to `knixl-modules`. Embed the tree with
  `static STDLIB: include_dir::Dir = include_dir!("$CARGO_MANIFEST_DIR/../../modules")`.
- `knixl_modules::stdlib::register_stdlib(reg: &mut Registry, claimed: &HashSet<String>) -> Vec<ShadowNotice>`:
  for each embedded `*/knixl-module.kdl`, parse it with `DeclarativeModule::from_kdl`
  and register it only if its node is not already `claimed`; otherwise record a
  shadow notice. Iterate the embedded entries in sorted name order for
  determinism.
- The embedded manifests are parsed once at registry build. A parse or
  type-check failure in an embedded module is a knixl bug (the modules are ours
  and golden-tested), so it surfaces as an internal error, not a user error.

## Fetched modules (runtime-resolve, mirrors oracle-modules)

### Declaration

`knixl.kdl` gains a `modules { }` block, a sibling of the existing
`oracle-modules { }`:

```kdl
modules {
    module "nginx" flake="github:someorg/knixl-nginx"
    module "grafana" flake="github:someorg/knixl-grafana" path="modules/grafana"
}
```

- `flake` is a flake-style ref resolved by the existing
  `knixl_nix::module::ModuleResolver` (the same resolver `oracle-modules` uses:
  `git ls-remote` by default, `KNIXL_MODULE_RESOLVER` override).
- `path` (optional, default the repo root) is the directory within the source
  repo that holds `knixl-module.kdl`.
- `name` is the local handle and must be unique within the block. It does not
  have to equal the module's `claims-node`; the fetched module's own manifest
  decides the node it claims.

Parsed into `ProjectConfig.module_sources: Vec<ModuleSource { name, flake, path }>`.

### Resolve and cache (install / upgrade)

Network resolution happens only in `install`/`upgrade`, never in `generate`,
exactly as the oracle augmented-set build does. For each declared `ModuleSource`:

1. Resolve `flake` to `{ url, rev }` via `ModuleResolver`.
2. Fetch `<path>/knixl-module.kdl` at `rev` (a shallow `git` fetch/archive of the
   single file; the resolver's URL plus rev is enough, matching how oracle module
   sources are fetched).
3. Validate it (`DeclarativeModule::from_kdl`) so a broken remote fails at
   install, not generate.
4. Write it into a content cache keyed by `(url, rev, path)`, alongside the
   oracle cache under knixl's cache dir. Record its hash.
5. Record a `ModuleSourcePin { name, url, rev, hash }` in the lock (a new
   `module_sources` list, sibling to `oracle.modules`).

### Load (generate)

`generate` is offline. For each declared `ModuleSource`, look up its
`ModuleSourcePin` in the lock, read the cached `knixl-module.kdl` for that rev,
verify the hash (a mismatch is a hard error, never a silent refetch), parse it,
and register it in the fetched layer. A declared source with no lock pin is a
validation error naming `install`/`upgrade` as the fix, exactly as an unresolved
nixpkgs baseline does today.

### Lock

`knixl.lock.kdl` gains, under the existing lock document:

```kdl
module-source "nginx" url="https://github.com/someorg/knixl-nginx" rev="<40-hex>" hash="<sha256>"
```

`ModuleSourcePin { name: String, url: String, rev: String, hash: Hash }` in
`knixl_lock::model`, parsed and rendered next to the oracle pins. This is part of
the reproducibility boundary: the rev pins the source, the hash pins the exact
bytes.

## Registry construction

`gather::build_registry(root)` becomes layered and returns notices:

```
build_registry(root, module_sources, lock) -> Result<(Registry, Vec<ShadowNotice>)>
```

1. register built-ins,
2. register local `<root>/modules/*` (duplicate-within-layer still errors),
3. register fetched modules from the cache (per the lock pins),
4. register embedded stdlib for any node not yet claimed,

recording a shadow notice whenever a lower layer's module is skipped because a
higher layer claimed its node. `gather` threads the notices into the generate
warnings, so `plan`/`generate` report them. The golden-test harness's
`build_registry` mirrors the real layering (built-ins + local + stdlib; no
fetched sources in the golden project).

## Determinism and reproducibility

- The embedded stdlib is fixed at compile time and iterated in sorted order, so
  the registry is a pure function of the binary plus the project.
- Fetched modules are pinned by rev and hash in the lock; generate reads the
  cache offline and verifies the hash, so output is reproducible and a tampered
  or corrupt cache is caught, never silently refetched.
- No `HashMap` on any emit path (unchanged).

## Testing

- **Embed**: a test that a fresh project (a temp dir with only host KDL, no local
  `modules/`) resolves a stdlib node (e.g. `web-service`) purely from the embedded
  set. A test that `register_stdlib` skips a node already claimed and returns a
  shadow notice.
- **Precedence**: a local module and a stdlib module claiming the same node ->
  local wins, one notice; two local modules claiming the same node -> hard error.
- **Fetch (offline)**: drive the resolver and fetch through a fake
  `KNIXL_MODULE_RESOLVER` and a seeded cache so no network is needed. Assert:
  a declared source with a matching lock pin loads from cache and registers;
  a declared source with no pin is a validation error; a cached file whose hash
  does not match the pin is a hard error.
- **Lock**: round-trip a `module-source` pin through `Lock::render`/parse.
- **Golden**: the existing goldens keep passing. The harness's `build_registry`
  switches from reading the repo `modules/` directory to registering the embedded
  stdlib (built-ins + stdlib; the golden temp projects have no local `modules/`
  of their own), so the golden hosts' declarative modules (web-service, zfs, user,
  openssh, tailscale, incus) now come from the embedded set and the blessed `.nix`
  outputs are unchanged. This also gives the embed path real end-to-end coverage.

## Non-goals

- No `knixl module add` command that writes files; sources are declared in
  `knixl.kdl` and resolved, matching the chosen runtime-resolve model.
- No transitive module dependencies (a fetched module pulling in others); a
  fetched module is a single `knixl-module.kdl`.
- No version constraints or semver ranges on fetched sources; a flake ref pins a
  rev, like `oracle-modules`.
- No stdlib versioning independent of the binary; the stdlib is whatever the
  binary embeds.
- No change to how built-in or local modules are authored.

## Docs

- `docs/03-module-system.md`: document the four-layer precedence and the shadow
  notice.
- A new ADR (0010): module distribution is an architectural decision (embedded
  stdlib as source-of-truth in `modules/`, fetched modules pinned in the lock,
  four-layer precedence). It relates to ADR 0008 (out-of-tree oracle modules),
  whose resolve/cache/lock pattern it mirrors.
- `docs/05-cli.md` / `docs/06`: note that `install`/`upgrade` resolve fetched
  module sources and generate loads them from the lock-pinned cache.
