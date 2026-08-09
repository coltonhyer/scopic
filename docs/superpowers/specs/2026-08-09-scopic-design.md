# scopic — design spec (v1)

Date: 2026-08-09
Status: design approved; pre-implementation. Reviewed twice by GPT-5.6-sol via Codex
(both rounds: approve-with-changes; all accepted changes are folded in below, skipped
ones are logged at the bottom).

## Purpose

An interactive terminal viewer for git and jj diffs, rendered side-by-side — the
GitHub split-view mental model. Success criterion: a diff that is easy to read and
navigate. Deliberately minimal.

**Non-goals for v1** (parking lot for later): syntax highlighting, file tree sidebar,
expandable context gaps (would require content fetching), search, mouse support, line
wrapping, staging/editing, Windows.

## Architecture in one paragraph

scopic runs **one subprocess** — `git diff` or `jj diff --git` — and renders the
patch. It never computes diffs between file contents and never resolves revisions
itself; all diff semantics (merge parents, root commits, renames, binary files,
submodules, untracked handling) are inherited from the VCS. A pure, byte-oriented
parser turns the patch into typed rows; the `similar` crate adds word-level intraline
highlight ranges on paired lines; ratatui renders the rows in one continuous
GitHub-style scroll.

## CLI contract

```
scopic              # jj: `jj diff` (@ vs parents) · git: `git diff HEAD` (staged+unstaged)
scopic REV          # jj: `jj diff -r REV` (any revset; jj's own errors pass through)
                    # git: `git diff REV` (REV vs working tree — git-native semantics)
scopic A B          # jj: `jj diff --from A --to B` · git: `git diff A B`
scopic A..B         # git: forwarded verbatim (git-native range syntax)
```

- Only revision operands are accepted: zero, one, or two. **No VCS options are ever
  forwarded** (no `--raw`, `--word-diff`, etc.).
- No `A:B` sugar — a colon collides with jj revset syntax such as `exact:"x"`.
- scopic's own flags: `--light`, `--help`, `--version`. Unknown flags are an error.
- Decided explicitly: zero-arg git mode is `git diff HEAD` (not index-vs-worktree),
  matching "show me my current changeset".

## VCS layer (src/vcs.rs)

- Detection at startup: walk up from cwd looking for a `.jj` directory → jj mode;
  else `git rev-parse --show-toplevel` succeeds → git mode; else exit 1 with a
  one-line message. jj wins in colocated repos.
- git invocation, one constant:
  `git --no-pager -c core.quotePath=false diff --no-color --no-ext-diff --no-textconv --find-renames --no-relative --default-prefix --unified=3 <args>`
- jj invocation: `jj diff --color never --git <mapped args>`
  (exact flag spelling verified against installed jj 0.43 in milestone 2).
- Capture stdout as **bytes**; cap at 50 MB — truncate after capture at the last
  complete file section, append a Truncated row.
  <!-- ponytail: cap-after-capture, no streaming child kill; add streaming if a real diff ever hits the cap -->
- Exit code is checked **before** parsing: nonzero → print the VCS's stderr
  verbatim, exit 1, never parse partial stdout, never enter the TUI.
  Exit 0 with empty stdout → print "no changes", exit 0.

## Parser and model (src/model.rs)

Input: patch bytes. Output: `Vec<FileDiff>`; each `FileDiff` = header info + `Vec<Row>`.

- Byte-oriented throughout; each displayed field/line is decoded individually with
  `from_utf8_lossy`. The parser never requires the whole patch to be valid UTF-8.
- Recognized per file section: `diff --git` line; extended headers (`new file mode`,
  `deleted file mode`, `rename from`/`rename to`, `similarity index`, `old mode`/
  `new mode`, `index`, `Binary files … differ`, `GIT binary patch`); hunk headers
  `@@ -a,b +c,d @@`; ` `/`-`/`+` body lines; `\ No newline at end of file`
  (rendered as a subtle end-of-file marker on the affected cell).
- Filenames come from rename headers or `---`/`+++` lines; C-quoted forms are
  unescaped (standard escape set, ~20 lines). The `diff --git a/… b/…` line is never
  split on spaces.
- **Fallback arm:** any file section that does not parse cleanly (combined
  `diff --cc` conflict diffs, exotic headers, unparseable names) becomes a
  `Badge("unsupported diff: <best-effort name>")` row and the parser skips to the
  next `diff --git`. A parse problem never aborts the launch.
- Row variants:
  `FileHeader { path, old_path, status, adds, dels }` · `HunkGap` ·
  `Line { left: Option<Cell>, right: Option<Cell> }` · `Badge(String)` · `Truncated`.
  `Cell { lineno, text, kind: Ctx|Add|Del, emph: Vec<Range<usize>> }`.
- Alignment: within a hunk, a run of `-` lines followed by a run of `+` lines pairs
  block-wise (`delete[i] ↔ insert[i]`); leftovers render one-sided.
- Intraline: for each paired row, `similar` word-level diff (`inline` feature)
  produces emphasis ranges for both cells. Skipped for lines over 1 KB.
  <!-- ponytail: size skip instead of a diff deadline; revisit if pathological lines show up -->
- Cell text is stored raw. Tab expansion (4 columns) and CRLF display-stripping
  happen at render prep only. Line-level comparison already happened in the VCS;
  the intraline comparison runs on raw text, and its emphasis ranges are byte
  ranges into that raw text which the renderer maps through tab expansion when
  drawing.

## TUI (src/ui.rs)

```
┌ scopic  @- → @   3 files  +42 −17 ────────────────────────────┐
│ src/main.rs                                          M +12 −3 │ ← sticky current-file bar
│  10 │ fn main() {           │  10 │ fn main() {               │
│  11 │ -   let x = 1;        │  11 │ +   let x = 2;            │
│     │                       │  12 │ +   let y = 3;            │
│ ··· (gap between hunks)                                       │
│ src/lib.rs                                          M +30 −14 │
└ j/k scroll · n/p file · f files · h/l pan · g/G · q quit ─────┘
```

- One continuous scroll of all rows; sticky top bar shows the file currently in
  view; footer shows key hints.
- Keys: `j/k`/arrows scroll · `ctrl-d/u` half-page · `n/p` jump between file
  headers · `f` overlay file picker (status + counts, enter jumps, esc closes) ·
  `h/l` horizontal pan (one shared offset for both panes) · `g/G` top/bottom · `q`/esc quit · ctrl-c always quits
  cleanly.
- Long lines truncate with `…`; panning is char-safe (never slices UTF-8 mid-char);
  all widths are computed in display columns via ratatui spans (unicode-width).
- Colors: dim red/green backgrounds for changed lines, brighter background + bold on
  intraline emphasis, dim gray line numbers, bold file headers. One palette struct;
  dark default, `--light` flips it, `NO_COLOR` falls back to `+`/`-` markers with no
  colors.
- Guard rails: minimum size check (below ~40×10 → centered "terminal too small"
  message); resize re-layouts and re-truncates; panic hook plus an RAII terminal
  guard restore the terminal on every exit path.

## Testing

- Parser: fixture patches committed under `tests/fixtures/`, captured from real git
  and jj: modify, add, delete, rename, binary, mode-only change, filename with
  spaces, C-quoted filename, CRLF file, CJK/emoji content, no trailing newline,
  combined diff (expects Badge), truncated patch. jj fixtures captured from the
  installed jj 0.43 in milestone 2 (verifies rename/copy/binary/conflict output
  and flag spellings).
- Model: block pairing including unequal runs, intraline ranges, oversized-line skip.
- UI: ratatui `TestBackend` snapshots at 80×24 and 40×12 asserting characters **and
  styles**; `NO_COLOR` mode; behavior across a simulated resize.
- One ignored-by-default end-to-end test that builds a scratch git repo and runs the
  real pipeline headless (subprocess → parse → model, no TUI).

## Milestones

1. **Walking skeleton — go/no-go gate (~30 min):** cargo project; hardcoded patch
   string → parser → rows → ratatui split render → one green TestBackend snapshot;
   the binary runs and quits cleanly. If this milestone fights back, stop and
   reassess the stack before any further investment.
2. VCS layer: detection, command constants, error/empty/truncation handling; capture
   git + jj 0.43 fixtures.
3. Parser + model complete against all fixtures; intraline emphasis.
4. TUI complete: keys, picker, colors, sticky header, guard rails, snapshots.
5. QA on real repos, `--help`, README.

## Dependencies

`ratatui` (pinned minor) · `similar` (pinned, `inline` feature) · `anyhow`.
crossterm is used via ratatui's re-export. No clap — hand-rolled args (~20 lines
including `--help`). Normal cargo build; static/musl linking is explicitly out of
scope.

## Decisions log (from the two Codex reviews)

Adopted:
- Consume VCS patch output; never recompute diffs. Resolves merge/root semantics,
  renames, non-regular files, and path resolution by inheritance.
- Drop `A:B` sugar (jj revset collision). Restrict CLI to revision operands only.
- Pin every parser-affecting git flag in one constant; `--no-pager` included.
- Byte-oriented parsing with per-line lossy decode.
- Fallback badge arm for anything unrecognized, including `diff --cc`.
- Distinguish VCS failure / empty diff / truncation as three separate outcomes.
- TUI guard rails, style-aware snapshots, explicit row variants, pinned deps.

Skipped deliberately (with ceilings marked in source):
- Streaming stdout with child kill at the byte cap, producer timeouts, intraline
  deadlines — defensive engineering for hostile input; scopic diffs your own repo.
  Cap-after-capture and a size skip cover v1.
- Filesystem-only detection micro-optimization (saving one subprocess at startup).
- Full git-quoting fuzz coverage — the fallback badge absorbs what the unescaper
  misses.
