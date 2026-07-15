# knixl install TUI (Slice C) design

Date: 2026-07-14
Status: approved, ready for implementation plan

## Problem and goal

Slice A gave `knixl install` a plain textual preview and a `[y/N]` confirm. This slice
adds the colourful interactive preview from the original idea: on an interactive
terminal, show the drafted change, the generated Nix, and live verification status, let
the user switch the target host, and apply or cancel. It is a front-end over the Slice A
logic, not a reimplementation.

## Scope

In:

- A `tui` module in `knixl-cli` with a pure `render`, a pure `update` reducer, and a
  crossterm event loop.
- A host picker (switch the target and re-preview) plus apply/cancel.
- Live verify status (package resolves, file parses) reusing `NixEval`.
- TTY gating with a clean fallback to the Slice A plain path.

Out (later or never): inline text editing of the package name, version fields (Slice D),
mouse support, and theming beyond basic colour.

## Dependencies

Add `ratatui` and `crossterm` to `knixl-cli` only. The libraries (`knixl-*`) stay
free of TUI dependencies, so the reusable core is unchanged.

## Architecture

Three parts, kept separate so the logic is testable without a terminal:

- **State**

  ```
  struct InstallState {
      pkg: String,
      hosts: Vec<HostInfo>,   // from install::list_hosts
      selected: usize,        // index into hosts
      strict: bool,
      preview: Preview,       // draft KDL line + generated .nix for the selected host
      resolves: Resolve,      // Unknown | Yes | No | Skipped (host-independent, cached)
      parses: Verify,         // Idle | Running | Ok | Failed(String) | Skipped (per host)
  }
  ```

- **`render(&InstallState, &mut Frame)`** — pure. Draws a bordered panel: a host
  selector line (`host: ‹ web ›`, hint `←/→`), the `+ package "<pkg>"` edit, a verify
  line (`✓ resolves  ✓ parses`, a spinner while running, `✗` plus reason on failure,
  `skipped (no nix)` when absent), the generated `.nix` snippet in a scroll region, and
  the key hints (`[enter] apply   [q] cancel`). Snapshot-tested with `TestBackend`.

- **`update(&mut InstallState, KeyEvent) -> Action`** — pure reducer returning
  `Apply | Cancel | SwitchHost(i32) | Redraw | None`. Left/right change `selected`
  (clamped, no wrap past the ends); enter yields `Apply` (only when apply is allowed);
  `q`/Esc yields `Cancel`. Unit-tested with constructed key events, no terminal.

- **`run_loop<B, E>(terminal: &mut Terminal<B>, events: E, state, verify) -> Decision`** —
  the loop, generic over the ratatui `Backend` and an event source `E: Iterator<Item =
  io::Result<KeyEvent>>`. It renders, pulls the next key, applies `update`, and on
  `SwitchHost` recomputes `preview` synchronously (pure generate, no nix) and re-runs the
  per-host parse verify (`Running` shown meanwhile), until `update` yields `Apply` or
  `Cancel`, which it returns. It does not touch raw mode or the real terminal itself.

Because `run_loop` is generic over the backend and the event source, it is driven end to
end in tests: a `TestBackend` terminal plus a scripted vector of key events, with an
injected `NixEval` shim for verify. Production wires `run_loop` to a `CrosstermBackend`
over stdout and a real crossterm key reader; that wiring, plus entering/leaving raw mode,
is the only untested glue (a handful of lines behind the terminal guard).

## Integration with install

`install()` gains one branch near the confirm step:

- If stdout and stdin are interactive terminals (`std::io::IsTerminal`) and `--yes` was
  not passed, run the TUI. It returns `Apply(host_index)` or `Cancel`.
- Otherwise, the Slice A plain preview + `[y/N]` confirm, unchanged.

Either way the decision feeds the same draft -> verify -> write -> regenerate code that
Slice A already has. The TUI does not write files itself; it only chooses.

Because the host can change in the TUI, the draft/verify for the chosen host is computed
inside the loop, and the final apply uses the host the user settled on.

## Verify behaviour

Reuses `NixEval` (injectable via `KNIXL_NIX`).

- `resolves`: `pkgs.<pkg>` existence is host-independent, so it runs once when the TUI
  opens and is cached.
- `parses`: per host, re-run on a host switch. Instant preview first, `Running` shown
  until the parse returns.
- Apply is allowed only when `resolves == Yes` (or `Skipped` and not `--strict`) and the
  parse did not fail. When `resolves == No`, apply is disabled and the reason shows.
- nix absent: both read `skipped (no nix)`; apply is blocked under `--strict`, allowed
  otherwise, matching the Slice A policy.

## Terminal safety

Raw mode and the alternate screen are entered on start and always restored on exit,
including on error and panic, via a guard type whose `Drop` leaves the terminal clean. A
`SIGINT` during the loop cancels (no write) and restores the terminal.

## Testing

- `update()` reducer: switch clamping, apply, cancel, apply-blocked-when-unresolved,
  apply-blocked-under-strict-when-skipped.
- `render()`: `TestBackend` buffer snapshots for resolved, unresolved, and verifying
  states.
- `run_loop()` (the loop, end to end): driven over a `TestBackend` with scripted key
  sequences and a `NixEval` shim, asserting the returned `Decision` and the host it
  settled on. Cases: cancel on `q`; apply immediately; switch right twice then apply
  (settles on the third host, re-verifying each switch); apply refused while the package
  is unresolved. This covers the loop's control flow, host recompute, and verify
  re-runs, not just the pure functions.
- Verify wiring: the `NixEval` shim from Slice A.
- Fallback: the existing install e2e tests run non-TTY, so they exercise the plain path
  unchanged; a `--yes` run stays plain.

Only the production glue (enter/leave raw mode, the real crossterm key reader) stays
untested; the loop's behaviour is covered via `run_loop` over `TestBackend`.

## Files touched

- `crates/knixl-cli/Cargo.toml`: add `ratatui`, `crossterm`.
- `crates/knixl-cli/src/tui.rs` (new): state, `render`, `update`, the loop, the terminal
  guard, and tests.
- `crates/knixl-cli/src/main.rs`: the TTY-gated branch in `install()`; factor the draft
  and verify steps so both the TUI and the plain path call them.
- `docs/05-cli.md`: a note that `install` opens an interactive preview on a TTY.

## Out of scope (later slices)

Inline editing of the package name, version selection (Slice D), `nix build` verification
(Slice B), mouse support, and configurable themes.
