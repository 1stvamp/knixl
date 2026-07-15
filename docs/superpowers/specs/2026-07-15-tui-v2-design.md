# knixl TUI v2 design

Date: 2026-07-15
Status: approved, ready for implementation plan

## Problem and goal

The Slice C install TUI is a hand-rolled `render -> String` plus a manual key loop: no
focus model, no widgets, no layout engine, no resize. This redesign rebuilds it as a
proper interactive app on bubbletea-rs + lipgloss: a movable focus selector across real
controls, responsive layout, richer options, and two new screens (browse modules,
scaffold a module). Delivered as one PR (explicit "everything in one go").

## Framework and MSRV

- `bubbletea-rs` (Elm architecture: model / message / `update` / `view`), `lipgloss` for
  styling, `bubbletea-widgets` for text input, list, viewport, and spinner.
- MSRV rises 1.85 -> 1.87 (`rust-toolchain.toml` channel and workspace `rust-version`).
  `bubbletea-widgets` needs 1.87 via `lipgloss-table`; 1.87 is below the 1.95 that kdl 6.7
  would need, so the kdl 6.5 pin stays. `bubbletea-widgets` is added without default
  features (no clipboard, so no X11/xcb build dependency). Verified: the stack compiles on
  1.87.
- Only `knixl-cli` gains these deps; the libraries stay TUI-free.

## Architecture

Elm loop, one `App` model:

```
struct App {
    size: (u16, u16),          // from WindowSizeMsg
    ctx: TuiCtx,               // registry, hosts, root, formatter, nixeval (injectable)
    screen: Screen,            // Home | Install | Browse | Author
}
enum Screen { Home(HomeModel), Install(InstallModel), Browse(BrowseModel), Author(AuthorModel) }
```

- `update(&mut self, Msg) -> Option<Cmd>` and `view(&self) -> String` are pure over the
  model; the terminal is only touched by the bubbletea runtime. Screens are tested by
  feeding messages and asserting on the model and the rendered string.
- Messages: `Key`, `WindowSize` (resize), optional `Mouse`, and custom async results
  (`VerifyDone`, `GeneratedPreview`, `Written`). The slow nix verify runs as an async
  `Cmd` that resolves to `VerifyDone`, so the spinner is real (non-blocking).
- Navigation: each screen owns a focus index over its controls; Tab / Shift-Tab and arrows
  move it, Enter activates the focused control, Esc/`q` backs out (to Home, or quits from
  Home). The focused control renders with an accent border/ring.
- `TuiCtx.nixeval` is injected so tests use the `NixEval` shim (no nix).

## Screens

### Home
A `list` of entries: Install a package, Browse modules, New module, Quit. Enter opens the
chosen screen. `knixl tui` starts here.

### Install
Supersedes the Slice C TUI. Controls, top to bottom, in focus order:
- host `list` (the target; contributes to the KDL edit path),
- package name `textinput` (editable; defaults to the CLI arg when entered via `install`),
- a `--strict` toggle,
- verify status line (async: spinner while running, then resolves/parses with the same
  apply-gating as Slice A),
- a `viewport` scrolling the generated `.nix` for the drafted host,
- Apply / Cancel buttons (focusable styled spans we own).

Editing the package or switching host recomputes the preview (in memory, no disk writes)
and re-runs verify as a `Cmd`. Apply commits through the existing draft -> verify -> write
path; Cancel/Esc leaves without changes.

### Browse
- left: a `list` of available modules, built-in and declarative, tagged by kind,
- right: the selected module's schema in a `viewport` (from `NodeSchema::render_doc`),
- action `i`: insert the module's node into a chosen host's KDL (a small host picker),
  format-preserving, like the package splice.

### Author
A form of `textinput`s that scaffolds a new declarative module:
- name, claimed node, summary, and a small starting set of schema fields
  (`name : type`, required?), plus a starter `emit` line.
- Scaffold writes `modules/<name>/knixl-module.kdl` with a valid skeleton (it must load
  through `DeclarativeModule::from_kdl` and pass the dry type-pass), which the user then
  edits.

A full in-TUI schema/emit editor is out of scope (tracked as a GitHub issue); this is a
starter-scaffold only.

## Entry points

- `knixl tui` opens Home.
- `knixl install <pkg>` opens the Install screen directly with the package prefilled,
  when interactive; non-interactive / `--yes` keeps the Slice A plain path unchanged.

## Responsive layout

`WindowSize` updates `App.size`; `view` lays out with lipgloss, choosing stacked vs
side-by-side by width and sizing viewports to the height. A sensible minimum size falls
back to a single column; below that, a short "resize" hint.

## Terminal safety

The bubbletea runtime owns raw mode and the alternate screen and restores them on exit;
we add a guard so a panic still restores the terminal. Non-TTY (piped / CI) never launches
the TUI: `knixl install` falls back to the plain path, and `knixl tui` errors with a clear
message rather than hanging.

## Testing

Per-screen `update`/`view` are pure, so:

- focus: Tab/arrow messages move the selector across controls (assert the focus index and
  that `view` marks the right control focused).
- resize: a `WindowSize` message reflows the layout (assert the view adapts).
- install: edit the package, toggle strict, a `VerifyDone` message updates status, Enter on
  Apply yields the write; unresolved blocks Apply.
- browse: selecting a module renders its schema; insert adds the node (assert the KDL edit).
- author: filling the form and Scaffold writes a `knixl-module.kdl` that re-loads via
  `DeclarativeModule::from_kdl` (assert it parses and dry-checks).

nix verify is injected (the `NixEval` shim), so no nix is needed; the async path is covered
by asserting the `VerifyDone`/`GeneratedPreview` handlers. The bubbletea runtime glue
(spawning the program, real key reads) stays untested.

## Files

- `crates/knixl-cli/Cargo.toml`: add bubbletea-rs, bubbletea-widgets (no default features),
  keep lipgloss.
- `rust-toolchain.toml`, root `Cargo.toml`: MSRV 1.87.
- `crates/knixl-cli/src/tui/{mod,install,browse,author,theme}.rs` (replacing the single
  `tui.rs`): the app, screens, shared styles.
- `crates/knixl-cli/src/main.rs`: a `tui` subcommand; `install` opens the Install screen.
- `docs/05-cli.md`: note the `tui` command and the interactive install.

## Out of scope (tracked as issues)

Full in-TUI schema/emit editor; module distribution/stdlib; `nix build` verification;
version pinning. Mouse support and theming beyond the base palette are also deferred.
