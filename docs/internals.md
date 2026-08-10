# Internals

~1200 lines across three source files, three dependencies (ratatui, similar,
unicode-width):

- `src/diff.rs` — parses git-format diffs into rows: ANSI stripping, del/add
  pairing, intraline word emphasis (time-bounded via `similar`)
- `src/ui.rs` — ratatui rendering: split panes, gutters, header bars with
  per-file counts, the GitHub-dark palette
- `src/main.rs` — stdin/file input, terminal setup, the event loop

The heavier reviewed design this was descoped from is archived in the
superstore ledger (`.agents/ledger.db`).
