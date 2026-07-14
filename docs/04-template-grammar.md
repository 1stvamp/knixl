# 04: EmitTemplate grammar

The substitution grammar for declarative modules. Parsed once from a module's `emit { ... }` block into a small AST, then interpreted per-node against a bindings tree built from the validated input. Full types in `crates/knixl-modules/src/template.rs`.

## Three statement forms, and no more

Matching exactly the boundary in docs/03 (substitute, repeat-into-list, gate-on-flag):

- `set <path> <value>` : assign a value into an option path.
- `when-flag "<flag>" { ... }` : generation-time gate on a bool input. Includes or drops its body.
- `for-each "<var>" in "<repeated-child>" { ... }` : iterate a repeated child, binding `<var>` per item, in KDL source order.

## Values

- scalars: `#true`, `16`, `"literal"`
- interpolated string: `"{upstream}"`, parts are literal or `{lookup}`
- indent string: `(indent-str #""" ... """#)`, interpolated, emits a `'' ... ''` block
- `(collect "child")` : fold a repeated child's first arg into a `List`. The only value form that reads a repeated child directly into a flat list. Use `for-each` when each item must produce distinct structure (a path per item) rather than a flat list.

## Paths

A dotted option path where each segment is literal or interpolated:

- bare word (`services`, `nginx`, `forceSSL`) -> `AttrKey::Ident`
- quoted literal (`"/"`) -> `AttrKey::Quoted`, may itself interpolate
- interpolation (`{host}`, `{loc.match}`) -> `AttrKey::Quoted` (a dynamic name)

This is where the oracle's `to_option_key()` promise is kept: every `Quoted` segment collapses to `<name>` for option lookup, so `services.nginx.virtualHosts."example.com".forceSSL` matches the option `services.nginx.virtualHosts.<name>.forceSSL`.

## Bindings tree

`bind()` walks the *schema*, not the raw node, so resolution is total and typed (validation already ran, so every referenceable name is present). Three shapes:

- `Scalar` : `"example.com"`, `16`, `true`
- `Scope` : a structured child, e.g. `acme -> { email }`, resolved with a dotted lookup `{acme.email}`
- `List` : a repeated child, in KDL source order

How each schema field maps:

- arg field -> `Scalar` (positional value)
- prop field -> `Scalar` (key=value on the node)
- flag child -> `Scalar::Bool` (present-and-true)
- scalar child -> `Scalar` (child's first arg)
- structured child -> `Scope` over its own args/props
- repeated child -> `List` of the above

## Lookup resolution

`{host}` resolves top-level. `{acme.email}` walks `Scope("acme")` then `Scalar("email")`. `{loc.match}` resolves the loop var `loc` (a `Scope` pushed by `for-each`) then `Scalar("match")`.

Loop variables live in a separate `LoopScopes` stack, checked before top-level bindings, so `for-each "host" in ...` shadows the node's `host` rather than colliding (the loader warns on shadow). Resolving a lookup to a non-`Scalar` in value position is a template authoring error, caught at module-load time by a dry type-pass, not at generation time.

## Worked expansion (both list forms)

Input:

```kdl
web-service "example.com" {
    upstream "http://127.0.0.1:3000"
    acme email="ops@example.com"
    alias "www.example.com"
    alias "example.org"
    location "/api" upstream="http://127.0.0.1:4000"
    location "/metrics" upstream="http://127.0.0.1:9090"
}
```

Template fragment:

```kdl
set "services.nginx.virtualHosts.{host}.serverAliases" (collect "alias")
for-each "loc" in "location" {
    set "services.nginx.virtualHosts.{host}.locations.{loc.match}.proxyPass" "{loc.upstream}"
}
```

Emits (post-nixfmt):

```nix
services.nginx.virtualHosts."example.com".serverAliases = [
  "www.example.com"
  "example.org"
];
services.nginx.virtualHosts."example.com".locations."/api".proxyPass = "http://127.0.0.1:4000";
services.nginx.virtualHosts."example.com".locations."/metrics".proxyPass = "http://127.0.0.1:9090";
```

`collect` gives the flat list, `for-each` gives one dynamic-keyed path per item. Both iterate in KDL source order, so the output hash is a pure function of the input, which is the property the lock depends on.
