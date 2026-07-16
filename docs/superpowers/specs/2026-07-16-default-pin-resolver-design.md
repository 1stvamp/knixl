# Built-in default version resolver design

Date: 2026-07-16
Status: approved, ready for implementation plan
Issue: #21
Builds on: docs/adr/0005-package-version-pinning.md, docs/superpowers/specs/2026-07-16-install-version-pinning-design.md

## Problem and goal

`knixl install pkg@version` resolves a version to a nixpkgs commit + sha256 through
`PinResolver`. Today `PinResolver::resolve()` defaults to an external `knixl-pin-resolve`
command that is never shipped, so pinning always refuses with "version resolver is not
available". Ship a built-in default so `install pkg@version` works with no extra setup,
while keeping `KNIXL_PIN_RESOLVER` as the override seam.

## Approach

`PinResolver` gains two modes:

```
pub enum PinResolver {
    External(PathBuf),   // KNIXL_PIN_RESOLVER is set: the existing `<bin> <name> <version>` protocol
    Builtin,             // unset: resolve internally
}
```

`resolve()`: `KNIXL_PIN_RESOLVER` set to a path => `External(path)`, else `Builtin`.
`lookup(name, version)` dispatches: `External` keeps the exact current shell-out protocol and
error mapping (unchanged, still shim-tested); `Builtin` resolves internally.

### Builtin resolution

Host-independent, run only at pin time (install/upgrade), so a network call here is fine
(the result is locked; generate/check stay offline).

1. Resolve name+version to a nixpkgs commit via the nixhub/devbox resolve API over HTTPS
   with `ureq` (version 3):
   `ureq::get("https://search.devbox.sh/v1/resolve?name=<name>&version=<version>").call()`
   (name and version percent-encoded). This endpoint takes the name AND version and returns
   `{ "commit_hash": "...", ... }` for a match, or HTTP 404 when there is none, so no
   client-side release filtering is needed. ureq is used rather than shelling out to `curl`
   so there is no runtime binary dependency, and its default `Agent` automatically honours
   the `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and `NO_PROXY` environment variables (parity
   with curl). It ships rustls by default (no system OpenSSL) and is blocking, so it fits
   synchronous `knixl-nix` with no async runtime.
2. On HTTP 200, parse the body with `serde_json` (already a workspace dependency) and read
   the top-level `commit_hash` string.
3. Prefetch the sha256 with `nix-prefetch-url --unpack
   https://github.com/NixOS/nixpkgs/archive/<commit>.tar.gz`; its base32 output is used
   verbatim as the `sha256` value (`builtins.fetchTarball` accepts it). This stays a `nix`
   shell-out (nix is already required by knixl).
4. Return `Resolved { nixpkgs_rev: commit, sha256 }`.

### Error mapping (Builtin), matching the External semantics

- A ureq transport error (DNS, connection, proxy, TLS), or an HTTP status that is neither
  200 nor 404 => `Unavailable` (the index was unreachable; a pin cannot be created, so
  install still refuses, but the message says so).
- HTTP 404 => `NotFound` (no commit ships that version).
- HTTP 200 with a missing/non-string `commit_hash`, unparseable JSON, or a non-zero
  `nix-prefetch-url` => `Failed`. `nix-prefetch-url` not spawnable => `Unavailable`.

## Testability

The JSON-to-commit step is a pure function, unit tested against a committed sample of the
resolve-API response shape:

```
fn commit_hash(resolve_json: &str) -> Option<String>
```

Tests: a real 200 body (returns its `commit_hash`), a body missing `commit_hash` (returns
None), and a malformed/empty body (returns None). The `ureq` fetch and the
`nix-prefetch-url` shell-out are untested glue, as with the other nix shell-outs. The
`External` path keeps its existing shim tests unchanged.

## Wiring and docs

- `ureq` (version 3, default features: rustls + gzip) is added to
  `crates/knixl-nix/Cargo.toml`. This is a new dependency (the rustls TLS stack),
  deliberately taken so HTTPS resolution is robust and proxy-aware without a `curl` runtime
  dependency.
- `serde_json` is added to `crates/knixl-nix/Cargo.toml` (already in the lock via
  knixl-oracle; no new external dependency).
- No CLI or lock changes: the CLI already builds `PinResolver::resolve()` and locks the
  result; it now gets a working default.
- `docs/05-cli.md`: note that `pkg@version` resolves via nixhub.io by default (HTTPS is
  built in and honours `*_PROXY`; the sha prefetch needs `nix` present), overridable with
  `KNIXL_PIN_RESOLVER`.
- Closes #21.

## Out of scope

Caching nixhub responses; a nix-native (non-nixhub) index; lazamar/other providers;
ret/backoff on transient HTTP errors. `KNIXL_PIN_RESOLVER` remains the escape hatch for any
of these.
