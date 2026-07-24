# 03: Module system

## The trait's job

Make a hand-written Rust module and a runtime-loaded KDL module indistinguishable to the generator. The declarative loader is itself one `Module` impl that interprets KDL, so everything downstream (validation, dispatch, the lock, doc generation) sees only the trait.

See `crates/knixl-modules/src/lib.rs` for the trait, `registry.rs` for dispatch, `builtin/` for Rust modules, and `template.rs` for the declarative interpreter.

## The trait

```rust
pub trait Module: Send + Sync {
    fn id(&self) -> ModuleId;              // name + version, goes in the lock
    fn node_name(&self) -> &str;           // the KDL node it claims, e.g. "postgres"
    fn schema(&self) -> &NodeSchema;       // validates input AND drives `knixl doc`
    fn lower(&self, node: &KdlNode, ctx: &mut LowerCtx) -> Result<LowerOutput, LowerError>;
}
```

Four deliberate exclusions from the trait:

- **Oracle check** is central, run once by the generator over every emitted `Assignment`. No module re-implements it.
- **Filenames** are not a module concern. Modules emit into abstract `Bucket`s; the generator maps buckets to paths.
- **`imports`** are wired by the generator when a named bucket becomes its own file. Modules never learn filenames.
- **`let` hoisting** is a generator pass, not a module concern.

## Two validation systems, kept separate

- `NodeSchema` validates the *input* shape (args, props, children, required-ness, arity, value types). Fails with a KDL span. "Your KDL is well-formed."
- The oracle validates the *output* (option paths exist and are correctly typed against real NixOS options). "The Nix you would produce is valid."

They fail at different layers with different spans. Do not merge them.

## A declarative module can target an out-of-tree option, once declared

The oracle's option set is not always nixpkgs alone. Once `knixl.kdl` (or a host's own
override) declares an out-of-tree module, e.g. disko or sops-nix, its options join the set a
declarative module's `set` is checked against (ADR 0008, docs/06). A module manifest does not
need to know or care whether a path it targets, such as `disko.devices.disk.main.device`, comes
from nixpkgs or a declared flake: the schema/oracle split above still holds, only the oracle's
own option set has grown. A module that targets a path from a module nobody declared still
fails `UnknownOption`, exactly as a typo would.

## Composition lives in the container, not the leaf

`host` consumes its own scalar fields and delegates the rest via `ctx.lower_children(node, &["system"])`, which dispatches each un-consumed child to its registered module and collects the outputs. Leaf modules (`postgres`, `web-service`) read their own subtree directly. Only container modules call `lower_children`.

## Buckets and multi-file output

A module says only "main file" (`Bucket::Default`) or "a named side-file" (`Bucket::Named("backup")`). The generator resolves `Default` to `generated/hosts/<host>.nix` and `Named("backup")` to `generated/hosts/<host>-backup.nix`, and auto-wires the `imports = [ ./<host>-backup.nix ];` line into the main file. Multi-file is a generator decision driven by bucket names, not something a module hard-codes.

## Built-in vs declarative: the honest boundary

- **Built-in (Rust)** when the module needs logic a template cannot express. `postgres` is the canonical case: "force the override only if the user's input conflicts with the base preset" is conditional priority computation. See `builtin/postgres.rs`.
- **Declarative (KDL)** when it is straight-line substitution. `web-service` qualifies; its whole definition is data in `crates/knixl-modules/stdlib/web-service/knixl-module.kdl`, interpreted by one `DeclarativeModule` that impls the same trait.

State the boundary in contributor docs on day one, or declarative modules will quietly reach for logic the interpreter keeps having to grow to meet. A declarative module can only:

- substitute inputs into paths and values,
- repeat a child into a list (`collect`) or into structure (`for-each`),
- fold a repeated child into a list of attribute sets (`list ... from`),
- gate a block on an input flag (`when-flag`, generation-time),
- gate a block on a runtime `config.*` condition (`when-config`, emitted as `lib.mkIf`).

It cannot compute priorities from cross-module conflicts and only writes `Bucket::Default`. The moment a module needs either of those, it becomes a built-in. A runtime condition alone no longer forces the boundary (so `backups`, a built-in solely for its `when=` condition, could in principle be declarative; converting it is a separate decision).

## The `raw-nix` escape hatch

`raw-nix` (`crates/knixl-modules/src/builtin/raw_nix.rs`) is a built-in for Nix that has no KDL shape worth inventing. The KDL is unusual: each child node's *name* is the verbatim Nix source, not an argument, so a block such as

```kdl
raw-nix {
    #"""
    systemd.services.nginx.serviceConfig.MemoryMax = "512M";
    """#
}
```

(see `examples/hosts/web.kdl`) passes that string through unmodified into the generated file. The content still hashes into the file, so it is covered by drift detection like everything else (ADR 0004); what is different is that the oracle does not look inside it. `raw-nix` is opaque to option-path validation, the trade-off for an escape hatch that can express anything Nix can.

## Module sources and precedence

Modules come from four layers, ordered by precedence (highest to lowest):

1. **Built-in** (Rust modules compiled into the binary)
2. **Local** (`<project>/modules/*`, declarative KDL modules)
3. **Fetched** (declared in `knixl.kdl`, resolved and pinned at install/upgrade, see ADR 0010)
4. **Embedded stdlib** (curated declarative modules bundled in the binary via `include_dir`, see ADR 0010)

Layers register highest precedence first. The first layer to claim a node name wins; a lower layer claiming the same name is shadowed and does not register. When a shadow occurs, the generator emits a notice naming the winning layer, the shadowed layer, and the module name. This non-silent shadowing lets hand-readers and auditors see that the precedence choice is intentional.

## Registration

Startup registers built-ins first, then local `<project>/modules/`, then fetched modules (loaded from the lock-pinned cache), then the embedded stdlib. Each lower layer registers only the nodes a higher layer has not already claimed, so a higher layer wins on a name collision and the shadow is reported. Two modules claiming the same node name within one layer is a hard error, not last-wins. A third party ships a module by dropping a `knixl-module.kdl` into the project's `modules/`, or declares a flake-based fetched module in `knixl.kdl`: no recompile, no fork. That is the whole ecosystem argument.

## Payoff of a structured `schema()`

Because `schema()` is structured data rather than prose, `knixl doc <node>` renders a typed reference (args, props, children, required-ness, docs) with zero extra bookkeeping, and the same data validates inputs, so the docs cannot drift from what the module accepts.
