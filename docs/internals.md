# Internals

~1200 lines across three source files, five direct dependencies (ratatui,
crossterm, similar, unicode-width, unicode-segmentation):

- `src/diff.rs` — parses git-format diffs into rows: ANSI stripping, del/add
  pairing, intraline word emphasis (time-bounded via `similar`)
- `src/ui.rs` — ratatui rendering: split panes, gutters, header bars with
  per-file counts, selection/source mapping, the GitHub-dark palette
- `src/main.rs` — stdin/file input, terminal setup, mouse/clipboard handling,
  the event loop

The heavier reviewed design this was descoped from is archived in the
superstore ledger (`.agents/ledger.db`).
