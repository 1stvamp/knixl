# examples: the behaviour contract

These are golden tests, not decoration. `hosts/*.kdl` are inputs. `expected/*.nix` is what
the pipeline must reproduce (post-nixfmt). `knixl.lock.kdl` is the lock shape.

Wire them early (see CLAUDE.md): parse the inputs, generate, format, compare byte-for-byte
against `expected/`, and diff the produced lock against `knixl.lock.kdl`.

Notes:
- Hashes in `knixl.lock.kdl` and the `Source:` header lines are placeholders. Recompute once
  the emitter and formatter are real. Everything else is the intended output.
- `db` exercises multi-file output (host imports db-backup.nix), a runtime `lib.mkIf` from the
  `backups` module (Rust-only condition), and a conditional `lib.mkForce` from `postgres`.
- `web` exercises the declarative `web-service` module, `when-flag` (hardened), and the
  `raw-nix` escape hatch.
- `backups` is referenced but not yet sketched as a module. It is a built-in candidate
  (the `when` condition off config.* is Rust-only). Add it in phase 3.
