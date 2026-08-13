# Pane-Aware Mouse Copying Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a mouse drag copy only the logical source text from one Scopic diff pane, including through wrapped lines, and send it to the host clipboard automatically on release.

**Architecture:** Keep the feature in the existing `ui.rs` and `main.rs` boundaries. `ui.rs` will extend its current per-character wrapping pass with source-byte/visual-column metadata, own active selection state, render the highlight, and return exact selected text; `main.rs` will translate mouse events and deliver that text through tmux or OSC 52. No new source module or crate is needed.

**Tech Stack:** Rust 2024, Ratatui 0.30, Crossterm 0.29 with `use-dev-tty` and `osc52`, `unicode-width`, Jujutsu 0.44 in a Git-colocated repository.

**Implements:** Superstore document `b99e0554-3e6b-4acc-8277-0cacfbdf7a0c`, “Pane-aware mouse copying design,” archived from `.agents/2026-08-13-pane-aware-copy-design.md`.

## Global Constraints

- A drag is locked to the source pane where it starts; it never includes the other pane.
- Copy `Cell.text`, preserving tabs and UTF-8, and rejoin visual wraps into logical lines separated by `\n` with no trailing newline.
- Omit gutters, divider, padding, dead cells, file headers, hunk headers, and raw metadata.
- Auto-copy on left-button release; keep wheel and keyboard navigation and Shift-drag fallback working.
- Under a non-empty `$TMUX`, use `tmux load-buffer -w -`; support tmux 3.2 or newer and do not require `set-clipboard on`.
- Outside tmux, use Crossterm's existing OSC 52 support; add no new crate.
- Copy failures stay inside the TUI and appear in the footer; they never terminate Scopic.
- Do not implement edge auto-scroll, keyboard selection, cross-pane selection, nested-tmux routing, or platform clipboard executables.
- Preserve the existing unrelated `BACKLOG.md` working-copy change and the Superstore-owned `.agents/ledger.db` change; every Jujutsu commit below names only its intended files.
- Use Conventional Commit descriptions and Jujutsu, not Git, for local changes.

---

## File Structure

- Modify `src/ui.rs`: selection model, visual-to-source mapping, extraction, highlight, footer state, and focused unit/render tests.
- Modify `src/main.rs`: clipboard transport and mouse/key/resize event wiring.
- Modify `Cargo.toml`: enable Crossterm's `osc52` feature.
- Modify `Cargo.lock`: record Crossterm's now-enabled optional `base64` dependency edge.
- Modify `README.md`: document mouse copying and the tmux 3.2 floor.
- No new source or test files: the existing UI test module already owns rendering behavior.

### Task 1: Map mouse drags to exact pane source text

**Files:**
- Modify: `src/ui.rs:1-330`
- Test: `src/ui.rs:332-631`

**Interfaces:**
- Consumes: `App.rows`, `App.scroll`, `panes`, `cell_segments`, `Cell.text`, and the existing wrap rules.
- Produces: `App::begin_selection(x, y, width, height)`, `App::drag_selection(x, y, width, height)`, `App::finish_selection() -> Option<String>`, and `App::cancel_selection()` for Task 3.
- Produces internally: `Pane`, `Hit`, `Selection`, `VisualGlyph`, and `CellSegment`; Task 2 uses the same `Selection` and `CellSegment` values for highlighting.

- [ ] **Step 1: Add a failing right-pane extraction test**

Add this test beside the existing UI tests:

```rust
#[test]
fn selects_right_pane_text_only() {
    let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
    // At 40 columns the divider is x=19 and right-pane text starts at x=25.
    app.begin_selection(25, 2, 40, 8);
    app.drag_selection(32, 3, 40, 8);
    assert_eq!(app.finish_selection().as_deref(), Some("ctx\nnew line"));
}
```

- [ ] **Step 2: Run the focused test and verify the missing API failure**

Run:

```bash
cargo test ui::tests::selects_right_pane_text_only -- --exact
```

Expected: compilation fails because `begin_selection`, `drag_selection`, and `finish_selection` do not exist.

- [ ] **Step 3: Add the minimal selection and visual-layout types**

Import `std::ops::Range`, then add these private types above `App` and fields to `App`:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Pane {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Hit {
    row: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, Debug)]
struct Selection {
    pane: Pane,
    anchor: Hit,
    head: Hit,
    dragged: bool,
}

#[derive(Clone, Debug)]
struct VisualGlyph {
    columns: Range<usize>,
    bytes: Range<usize>,
}

#[derive(Clone)]
struct CellSegment {
    spans: Vec<Span<'static>>,
    glyphs: Vec<VisualGlyph>,
}

pub struct App {
    pub rows: Vec<Row>,
    pub scroll: usize,
    gutter_w: usize,
    stats: HashMap<usize, (u32, u32)>,
    selection: Option<Selection>,
}
```

Initialize `selection` to `None` in `App::new`.

- [ ] **Step 4: Make the existing wrapping pass record glyph coordinates**

Change `cell_segments` to return `Vec<CellSegment>`. Keep the existing wrapping loop and span output intact. Alongside each rendered character, record its source bytes and pane-relative columns:

```rust
let mut glyphs = Vec::new();

// Inside the existing character loop, after any wrap flush and before used += cw:
glyphs.push(VisualGlyph {
    columns: gutter_w + used..gutter_w + used + cw,
    bytes: bidx..bidx + ch.len_utf8(),
});
```

Whenever the existing loop flushes a wrapped line, move both `spans` and `glyphs` into one segment:

```rust
segs.push(CellSegment {
    spans: std::mem::take(&mut spans),
    glyphs: std::mem::take(&mut glyphs),
});
```

Finish the last line the same way. For `None`, return one `CellSegment` whose `spans` are `dead_fill(width)` and whose `glyphs` are empty. Update existing callers as follows:

```rust
cell_segments(cell, width, gutter_w)
```

In `render_rows`, read `segment.spans.clone()` instead of cloning the old span vector. This preserves all current snapshots while giving hit-testing the exact layout generated by rendering.

- [ ] **Step 5: Add pane hit-testing from the shared segments**

Add helpers that choose complete UTF-8 character ranges and never slice a code point:

```rust
fn first_char(text: &str) -> (usize, usize) {
    text.char_indices()
        .next()
        .map(|(start, ch)| (start, start + ch.len_utf8()))
        .unwrap_or((0, 0))
}

fn last_char(text: &str) -> (usize, usize) {
    text.char_indices()
        .last()
        .map(|(start, ch)| (start, start + ch.len_utf8()))
        .unwrap_or((0, 0))
}
```

Implement a private `App::hit_test(pane, x, y, width, height) -> Option<Hit>` with this exact traversal:

1. Reject `y >= height.saturating_sub(1)` and zero-width panes.
2. Compute `(left_w, right_w) = panes(width)`. Clamp `x` into the requested pane; right-pane local x is `x.saturating_sub(left_w + 1).min(right_w - 1)`.
3. Walk rows from `self.scroll`, adding `row_height` until the requested visual y falls inside one row.
4. Reject every row except `Row::Line` and reject `None` on the selected side.
5. Build that side with `cell_segments(Some(cell), pane_w, self.gutter_w)` and select the visual segment at the row-relative y; reject a missing overhang segment.
6. If local x is inside a `VisualGlyph.columns`, return its byte range. If local x is in the real gutter, return `first_char(&cell.text)`. If it is before the first glyph on a continuation, return that glyph. Otherwise return `last_char(&cell.text)`.

Construct the result only from verified boundaries:

```rust
Some(Hit {
    row: row_index,
    start,
    end,
})
```

- [ ] **Step 6: Add selection lifecycle and exact extraction**

Choose a pane only when mouse-down is inside it; `x == left_w` is the divider and starts nothing:

```rust
pub fn begin_selection(&mut self, x: usize, y: usize, width: usize, height: usize) {
    let (left_w, _) = panes(width);
    let pane = if x < left_w {
        Pane::Left
    } else if x > left_w {
        Pane::Right
    } else {
        self.selection = None;
        return;
    };
    self.selection = self.hit_test(pane, x, y, width, height).map(|hit| Selection {
        pane,
        anchor: hit,
        head: hit,
        dragged: false,
    });
}

pub fn drag_selection(&mut self, x: usize, y: usize, width: usize, height: usize) {
    let Some(mut selection) = self.selection else {
        return;
    };
    if let Some(hit) = self.hit_test(selection.pane, x, y, width, height) {
        selection.head = hit;
        selection.dragged = true;
        self.selection = Some(selection);
    }
}

pub fn cancel_selection(&mut self) {
    self.selection = None;
}
```

Add a private `selection_text(&self, selection: Selection) -> String`. Normalize `(anchor, head)` by `Hit` ordering, walk the inclusive row interval, take the selected pane's `Cell`, slice `start.start..cell.text.len()` on the first row, `0..end.end` on the last row, and the full text between them, then join every contributing logical cell with `"\n"`. Preserve empty intermediate cells as empty strings so blank source lines remain blank.

Finish only a real drag and clear the state before returning:

```rust
pub fn finish_selection(&mut self) -> Option<String> {
    let selection = self.selection.take()?;
    if !selection.dragged {
        return None;
    }
    let text = self.selection_text(selection);
    (!text.is_empty()).then_some(text)
}
```

- [ ] **Step 7: Run the focused test and verify it passes**

Run:

```bash
cargo test ui::tests::selects_right_pane_text_only -- --exact
```

Expected: PASS.

- [ ] **Step 8: Add edge-case tests for wrapping, direction, pane clamping, and invalid starts**

Add focused tests using the public mouse-coordinate methods:

```rust
#[test]
fn reverse_drag_matches_forward_drag() {
    let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
    app.begin_selection(32, 3, 40, 8);
    app.drag_selection(25, 2, 40, 8);
    assert_eq!(app.finish_selection().as_deref(), Some("ctx\nnew line"));
}

#[test]
fn partial_endpoints_include_both_characters() {
    let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
    app.begin_selection(26, 2, 40, 8);
    app.drag_selection(27, 3, 40, 8);
    assert_eq!(app.finish_selection().as_deref(), Some("tx\nnew"));
}

#[test]
fn wrapped_selection_copies_original_line() {
    let text = "a very long line that cannot fit in the pane";
    let mut app = App::new(vec![Row::Line {
        left: Some(Cell { no: 1, text: text.into(), kind: Kind::Del, emph: vec![] }),
        right: None,
    }]);
    app.begin_selection(0, 0, 40, 5);
    app.drag_selection(18, 3, 40, 5);
    assert_eq!(app.finish_selection().as_deref(), Some(text));
}

#[test]
fn selection_preserves_tab_and_unicode() {
    let mut app = App::new(vec![Row::Line {
        left: Some(Cell { no: 1, text: "\té".into(), kind: Kind::Ctx, emph: vec![] }),
        right: None,
    }]);
    app.begin_selection(5, 0, 40, 2);
    app.drag_selection(9, 0, 40, 2);
    assert_eq!(app.finish_selection().as_deref(), Some("\té"));
}

#[test]
fn selection_skips_metadata_rows() {
    let cell = |no, text| Some(Cell { no, text: text.into(), kind: Kind::Ctx, emph: vec![] });
    let mut app = App::new(vec![
        Row::Line { left: cell(1, "one"), right: None },
        Row::Hunk("@@ -1 +1 @@".into()),
        Row::Raw("metadata".into()),
        Row::Line { left: cell(2, "two"), right: None },
    ]);
    app.begin_selection(5, 0, 40, 5);
    app.drag_selection(7, 3, 40, 5);
    assert_eq!(app.finish_selection().as_deref(), Some("one\ntwo"));
}

#[test]
fn selection_skips_absent_side_rows() {
    let cell = |no, text| Some(Cell { no, text: text.into(), kind: Kind::Ctx, emph: vec![] });
    let mut app = App::new(vec![
        Row::Line { left: cell(1, "one"), right: None },
        Row::Line { left: None, right: cell(1, "right only") },
        Row::Line { left: cell(2, "two"), right: None },
    ]);
    app.begin_selection(5, 0, 40, 4);
    app.drag_selection(7, 2, 40, 4);
    assert_eq!(app.finish_selection().as_deref(), Some("one\ntwo"));
}

#[test]
fn crossing_divider_stays_in_starting_pane() {
    let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
    app.begin_selection(25, 2, 40, 8);
    app.drag_selection(0, 3, 40, 8);
    assert_eq!(app.finish_selection().as_deref(), Some("ctx\nn"));
}

#[test]
fn dead_cell_and_footer_do_not_start_selection() {
    let mut app = App::new(vec![Row::Line {
        left: Some(Cell { no: 1, text: "left".into(), kind: Kind::Del, emph: vec![] }),
        right: None,
    }]);
    app.begin_selection(25, 0, 40, 2);
    app.drag_selection(30, 0, 40, 2);
    assert_eq!(app.finish_selection(), None);
    app.begin_selection(5, 1, 40, 2);
    app.drag_selection(8, 1, 40, 2);
    assert_eq!(app.finish_selection(), None);
    app.begin_selection(19, 0, 40, 2);
    app.drag_selection(18, 0, 40, 2);
    assert_eq!(app.finish_selection(), None);
}

#[test]
fn metadata_does_not_start_selection() {
    let mut app = App::new(vec![
        Row::Hunk("@@ -1 +1 @@".into()),
        Row::Line {
            left: Some(Cell { no: 1, text: "left".into(), kind: Kind::Ctx, emph: vec![] }),
            right: None,
        },
    ]);
    app.begin_selection(5, 0, 40, 3);
    app.drag_selection(8, 1, 40, 3);
    assert_eq!(app.finish_selection(), None);
}
```

- [ ] **Step 9: Run all UI tests**

Run:

```bash
cargo test ui::tests
```

Expected: all existing rendering tests and all new selection tests pass.

- [ ] **Step 10: Commit only the core UI selection change**

Inspect and commit only `src/ui.rs`:

```bash
jj diff --summary
jj diff -- src/ui.rs
jj commit src/ui.rs -m "feat(ui): add pane-aware text selection"
```

Expected: `.agents/ledger.db` and `BACKLOG.md` remain in the new working-copy change.

### Task 2: Render the active highlight and footer feedback

**Files:**
- Modify: `src/ui.rs:89-329`
- Test: `src/ui.rs` test module

**Interfaces:**
- Consumes: `Selection`, `CellSegment`, and selection lifecycle methods from Task 1.
- Produces: selection-aware `cell_segments`, `App::set_status(String)`, and `App::clear_status()` for Task 3.

- [ ] **Step 1: Add a failing selection-highlight test**

```rust
#[test]
fn active_selection_is_highlighted() {
    let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
    app.begin_selection(25, 2, 40, 8);
    app.drag_selection(27, 2, 40, 8);
    let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
    term.draw(|f| draw(f, &app)).unwrap();
    let buf = term.backend().buffer();
    assert!(buf[(25u16, 2u16)]
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
    assert!(!buf[(20u16, 2u16)]
        .style()
        .add_modifier
        .contains(Modifier::REVERSED));
}

```

- [ ] **Step 2: Run the highlight test and verify it fails**

Run:

```bash
cargo test ui::tests::active_selection_is_highlighted -- --exact
```

Expected: FAIL because the selected cells do not have `Modifier::REVERSED`.

- [ ] **Step 3: Compute the selected byte range for each logical row**

Add a private helper on `Selection`:

```rust
impl Selection {
    fn range_for(self, pane: Pane, row: usize, len: usize) -> Option<Range<usize>> {
        if pane != self.pane {
            return None;
        }
        let (start, end) = if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        };
        if row < start.row || row > end.row {
            return None;
        }
        let from = if row == start.row { start.start } else { 0 };
        let to = if row == end.row { end.end } else { len };
        (from <= to).then_some(from..to)
    }
}
```

Change `cell_segments` to accept `selected: Option<Range<usize>>`, update height and hit-test callers to pass `None`, then pass row index and `app.selection` through `draw` and `render_rows`. For each real left/right cell, call `range_for` and pass the result into `cell_segments`.

- [ ] **Step 4: Apply reverse video only to selected source glyphs**

Track selection alongside the existing emphasis run in `cell_segments`:

```rust
let mut cur_selected = false;

let in_selected = selected
    .as_ref()
    .is_some_and(|range| range.contains(&bidx));
if (in_emph, in_selected) != (cur_emph, cur_selected) && !cur.is_empty() {
    let style = if cur_emph { emph } else { base };
    spans.push(Span::styled(
        std::mem::take(&mut cur),
        if cur_selected {
            style.add_modifier(Modifier::REVERSED)
        } else {
            style
        },
    ));
}
cur_emph = in_emph;
cur_selected = in_selected;
```

Use the same style choice for every wrap flush and the final buffered run. Do not apply `REVERSED` to gutters, padding, the divider, or dead fill.

- [ ] **Step 5: Run the highlight test and verify it passes**

Run:

```bash
cargo test ui::tests::active_selection_is_highlighted -- --exact
```

Expected: PASS.

- [ ] **Step 6: Add a failing footer-status test**

```rust
#[test]
fn status_replaces_footer() {
    let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
    app.set_status(" copied 2 lines".into());
    assert_eq!(render(40, 8, &app)[7], " copied 2 lines");
}
```

Run:

```bash
cargo test ui::tests::status_replaces_footer -- --exact
```

Expected: compilation fails because `set_status` does not exist.

- [ ] **Step 7: Add footer status state, methods, and rendering**

Add `status: Option<String>` to `App` and initialize it to `None` in `App::new`, then add:

```rust
pub fn set_status(&mut self, status: String) {
    self.status = Some(status);
}

pub fn clear_status(&mut self) {
    self.status = None;
}
```

Render status in place of the normal key footer:

```rust
let footer_text = app.status.as_deref().unwrap_or(FOOTER);
f.render_widget(Paragraph::new(Line::styled(footer_text, dim())), footer);
```

- [ ] **Step 8: Run UI tests and formatting**

Run:

```bash
cargo fmt --check
cargo test ui::tests
```

Expected: all tests pass and formatting reports no diff. If formatting is needed, run `cargo fmt`, then repeat both commands.

- [ ] **Step 9: Commit only the rendering/status change**

```bash
jj diff -- src/ui.rs
jj commit src/ui.rs -m "feat(ui): render selection and copy status"
```

### Task 3: Auto-copy on mouse release through tmux or OSC 52

**Files:**
- Modify: `Cargo.toml:7-13`
- Modify: `Cargo.lock`
- Modify: `src/main.rs:4-111`
- Modify: `README.md:21-24`

**Interfaces:**
- Consumes: all public `App` selection/status methods from Tasks 1 and 2.
- Produces: private `copy_to_clipboard(text: &str) -> std::io::Result<()>` and the complete user-visible feature.

- [ ] **Step 1: Enable Crossterm's installed OSC 52 implementation**

Update the existing dependency without adding a crate:

```toml
crossterm = { version = "0.29", features = ["use-dev-tty", "osc52"] }
```

Run:

```bash
cargo check
```

Expected: PASS and `Cargo.lock` adds `base64` to Crossterm's enabled dependency list; the package is already present in the lockfile.

- [ ] **Step 2: Add the two-path clipboard function**

Add imports:

```rust
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

use ratatui::crossterm::clipboard::CopyToClipboard;
```

Add this private function above `run`:

```rust
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    if std::env::var_os("TMUX").is_some_and(|value| !value.is_empty()) {
        let mut child = Command::new("tmux")
            .args(["load-buffer", "-w", "-"])
            .stdin(Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("tmux stdin unavailable"))?;
        stdin.write_all(text.as_bytes())?;
        drop(stdin);
        let status = child.wait()?;
        return status
            .success()
            .then_some(())
            .ok_or_else(|| std::io::Error::other(format!("tmux exited with {status}")));
    }

    execute!(
        std::io::stdout(),
        CopyToClipboard::to_clipboard_from(text)
    )
}
```

- [ ] **Step 3: Wire left-button selection into the existing mouse match**

Import `MouseButton`, then replace the current mouse match with:

```rust
Event::Mouse(m) => match m.kind {
    MouseEventKind::Down(MouseButton::Left)
        if !m.modifiers.contains(KeyModifiers::SHIFT) =>
    {
        app.clear_status();
        app.begin_selection(m.column as usize, m.row as usize, w, h);
    }
    MouseEventKind::Drag(MouseButton::Left)
        if !m.modifiers.contains(KeyModifiers::SHIFT) =>
    {
        app.drag_selection(m.column as usize, m.row as usize, w, h);
    }
    MouseEventKind::Up(MouseButton::Left)
        if !m.modifiers.contains(KeyModifiers::SHIFT) =>
    {
        if let Some(text) = app.finish_selection() {
            let lines = text.split('\n').count();
            match copy_to_clipboard(&text) {
                Ok(()) => app.set_status(format!(
                    " copied {lines} {}",
                    if lines == 1 { "line" } else { "lines" }
                )),
                Err(error) => app.set_status(format!(" copy failed: {error}")),
            }
        }
    }
    MouseEventKind::ScrollDown => {
        app.cancel_selection();
        app.clear_status();
        app.scroll = app.scroll.saturating_add(3);
    }
    MouseEventKind::ScrollUp => {
        app.cancel_selection();
        app.clear_status();
        app.scroll = app.scroll.saturating_sub(3);
    }
    MouseEventKind::Down(MouseButton::Left)
    | MouseEventKind::Drag(MouseButton::Left)
    | MouseEventKind::Up(MouseButton::Left) => app.cancel_selection(),
    _ => {}
},
```

At the start of each accepted key press, before the key match, call:

```rust
app.cancel_selection();
app.clear_status();
```

Cancel stale coordinates on resize without changing scrolling:

```rust
Event::Resize(_, _) => app.cancel_selection(),
```

- [ ] **Step 4: Run the complete automated suite and lints**

Run:

```bash
cargo fmt
cargo test
cargo clippy --all-targets -- -D warnings
```

Expected: all tests pass and Clippy exits successfully with no warnings.

- [ ] **Step 5: Document the user-visible mouse behavior**

Change the README key line to:

```markdown
Mouse drag copies within one pane · `j/k` scroll · `ctrl-d/u` half-page · `n/p` next/prev file · `g/G` top/bottom · `q` quit
```

Add immediately below it:

```markdown
Inside tmux, clipboard copying requires tmux 3.2 or newer. Scopic uses tmux's
paste buffer and host-clipboard forwarding; no `set-clipboard on` setting is
required.
```

- [ ] **Step 6: Perform direct-terminal and tmux smoke checks**

Build once:

```bash
cargo build
```

Outside tmux, open a sample diff, drag a partial multi-line selection in the right pane, and paste it into a text editor. Expected: only right-pane source text appears, wrapped rows are joined, and the footer reports the copied line count.

Inside tmux 3.2 or newer with its default `set-clipboard external`, run Scopic, make the same selection, then check:

```bash
tmux show-buffer
```

Expected: `show-buffer` and the host clipboard contain identical pane-only text. Also verify the wheel still scrolls and Shift-drag still invokes terminal/tmux selection rather than Scopic's selection.

- [ ] **Step 7: Commit the transport, event wiring, and user documentation**

```bash
jj diff --summary
jj diff -- Cargo.toml Cargo.lock src/main.rs README.md
jj commit Cargo.toml Cargo.lock src/main.rs README.md -m "feat: copy pane selections to clipboard"
```

- [ ] **Step 8: Verify the completed Jujutsu stack and preserved dirty paths**

Run:

```bash
jj status
jj log -r '@ | @- | @-- | @---' --no-graph
jj diff --summary
```

Expected: the three feature changes have Conventional Commit descriptions; the active working-copy change contains only the pre-existing `.agents/ledger.db` and `BACKLOG.md` modifications, with no feature files left uncommitted.
