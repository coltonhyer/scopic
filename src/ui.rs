use std::collections::HashMap;
use std::ops::Range;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthChar;

use crate::diff::{Cell, Kind, Row};

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
    /// File header row index → (added, deleted) line counts for its section
    stats: HashMap<usize, (u32, u32)>,
    selection: Option<Selection>,
}

impl App {
    pub fn new(rows: Vec<Row>) -> Self {
        let max_no = rows
            .iter()
            .filter_map(|r| match r {
                Row::Line { left, right } => left.iter().chain(right.iter()).map(|c| c.no).max(),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let digits = (max_no.max(1).ilog10() as usize + 1).max(4);
        let mut stats = HashMap::new();
        let mut cur = None;
        for (i, r) in rows.iter().enumerate() {
            match r {
                Row::File(_) => {
                    stats.insert(i, (0, 0));
                    cur = Some(i);
                }
                Row::Line { left, right } => {
                    if let Some(e) = cur.and_then(|f| stats.get_mut(&f)) {
                        if matches!(left, Some(c) if c.kind == Kind::Del) {
                            e.1 += 1;
                        }
                        if matches!(right, Some(c) if c.kind == Kind::Add) {
                            e.0 += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        Self {
            rows,
            scroll: 0,
            gutter_w: digits + 1,
            stats,
            selection: None,
        }
    }

    /// smallest scroll where the tail still fits the viewport (a wrapped last
    /// page may run short); needs the width to sum wrapped row heights
    pub fn max_scroll(&self, w: usize, viewport_h: usize) -> usize {
        let body_h = viewport_h.saturating_sub(1);
        let (lw, rw) = panes(w);
        let mut h = 0usize;
        for i in (0..self.rows.len()).rev() {
            h += row_height(&self.rows[i], lw, rw, self.gutter_w);
            if h > body_h {
                // a tail row taller than the body must stay reachable, clipped
                return (i + 1).min(self.rows.len() - 1);
            }
        }
        0
    }

    /// index of the next/prev `Row::File` relative to current scroll
    pub fn file_jump(&self, forward: bool) -> Option<usize> {
        let is_file = |i: &usize| matches!(self.rows[*i], Row::File(_));
        if forward {
            (self.scroll + 1..self.rows.len()).find(is_file)
        } else {
            (0..self.scroll).rev().find(is_file)
        }
    }
}

const FOOTER: &str = " j/k · ctrl-d/u · n/p · g/G · q";

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

// GitHub-dark tints: red/green at 15% (lines) and 40% (word emph) over the page bg.
// ponytail: truecolor only — non-truecolor terminals quantize these to near-gray
// and add/del become hard to tell apart; revert to Indexed(52/22/88/28) if that bites
fn base_style(kind: Kind) -> Style {
    match kind {
        Kind::Ctx => Style::default(),
        Kind::Del => Style::default().bg(Color::Rgb(48, 27, 31)),
        Kind::Add => Style::default().bg(Color::Rgb(18, 38, 30)),
    }
}

fn emph_style(kind: Kind) -> Style {
    match kind {
        Kind::Ctx => Style::default(),
        Kind::Del => Style::default()
            .bg(Color::Rgb(107, 43, 43))
            .add_modifier(Modifier::BOLD),
        Kind::Add => Style::default()
            .bg(Color::Rgb(26, 74, 41))
            .add_modifier(Modifier::BOLD),
    }
}

/// left/right pane widths for a terminal width
fn panes(w: usize) -> (usize, usize) {
    let left = w.saturating_sub(1) / 2;
    (left, w.saturating_sub(1 + left))
}

// ponytail: counts by building the spans; fine at diff sizes
fn row_height(row: &Row, left_w: usize, right_w: usize, gutter_w: usize) -> usize {
    match row {
        Row::Line { left, right } => cell_segments(left.as_ref(), left_w, gutter_w)
            .len()
            .max(cell_segments(right.as_ref(), right_w, gutter_w).len()),
        _ => 1,
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.height == 0 {
        return;
    }
    let body_h = area.height.saturating_sub(1);
    let w = area.width as usize;
    let (left_w, right_w) = panes(w);

    let lines: Vec<Line> = app
        .rows
        .iter()
        .enumerate()
        .skip(app.scroll)
        .flat_map(|(i, row)| {
            render_rows(
                row,
                left_w,
                right_w,
                app.gutter_w,
                app.stats.get(&i).copied(),
            )
        })
        .take(body_h as usize)
        .collect();

    let body = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: body_h,
    };
    f.render_widget(Paragraph::new(Text::from(lines)), body);

    let footer = Rect {
        x: area.x,
        y: area.y + body_h,
        width: area.width,
        height: 1,
    };
    f.render_widget(Paragraph::new(Line::styled(FOOTER, dim())), footer);
}

fn render_rows(
    row: &Row,
    left_w: usize,
    right_w: usize,
    gutter_w: usize,
    stats: Option<(u32, u32)>,
) -> Vec<Line<'static>> {
    let w = left_w + 1 + right_w;
    match row {
        Row::File(t) => {
            let bar = Style::default().bg(Color::Indexed(236));
            let split = t.rfind('/').map_or(0, |i| i + 1);
            let mut spans = vec![
                Span::styled(t[..split].to_string(), bar.fg(Color::DarkGray)),
                Span::styled(t[split..].to_string(), bar.add_modifier(Modifier::BOLD)),
            ];
            let counts = match stats {
                Some((a, d)) if a + d > 0 => vec![
                    Span::styled(format!("+{a} "), bar.fg(Color::Green)),
                    Span::styled(format!("-{d} "), bar.fg(Color::Red)),
                ],
                _ => vec![],
            };
            let used = t.chars().count()
                + counts
                    .iter()
                    .map(|s| s.content.chars().count())
                    .sum::<usize>();
            spans.push(Span::styled(" ".repeat(w.saturating_sub(used)), bar));
            spans.extend(counts);
            vec![Line::from(spans)]
        }
        // muted GitHub-dark hunk tint; non-truecolor terminals approximate to a dark gray
        Row::Hunk(h) => vec![Line::styled(
            format!("{h:<w$}"),
            dim().bg(Color::Rgb(20, 34, 56)),
        )],
        Row::Raw(r) => vec![Line::styled(r.clone(), dim())],
        Row::Line { left, right } => {
            let ls = cell_segments(left.as_ref(), left_w, gutter_w);
            let rs = cell_segments(right.as_ref(), right_w, gutter_w);
            (0..ls.len().max(rs.len()))
                .map(|i| {
                    let mut spans = ls
                        .get(i)
                        .map(|segment| segment.spans.clone())
                        .unwrap_or_else(|| dead_fill(left_w));
                    spans.push(Span::styled("│", dim()));
                    spans.extend(
                        rs.get(i)
                            .map(|segment| segment.spans.clone())
                            .unwrap_or_else(|| dead_fill(right_w)),
                    );
                    Line::from(spans)
                })
                .collect()
        }
    }
}

// GitHub's "dead cell": the absent side of a one-sided change, and the
// shorter side's overhang when the other pane wraps taller
fn dead_fill(width: usize) -> Vec<Span<'static>> {
    vec![Span::styled(
        " ".repeat(width),
        Style::default().bg(Color::Indexed(234)),
    )]
}

/// one pane of a row as visual lines, wrapped at the pane budget; the first
/// segment carries the line number, continuations get a blank gutter
fn cell_segments(cell: Option<&Cell>, width: usize, gutter_w: usize) -> Vec<CellSegment> {
    let Some(c) = cell else {
        return vec![CellSegment {
            spans: dead_fill(width),
            glyphs: vec![],
        }];
    };
    let base = base_style(c.kind);
    let emph = emph_style(c.kind);
    let gutter_w = gutter_w.min(width);
    let budget = width - gutter_w;
    let gutter_style = base.fg(Color::DarkGray);
    let g = format!("{:>w$} ", c.no, w = gutter_w.saturating_sub(1));

    // tab = 4 cols
    // ponytail: per-char widths; ZWJ emoji clusters (👩‍💻) over-count and wrap
    // early on that row only — switch to grapheme segmentation if it ever matters
    let col = |ch: char| {
        if ch == '\t' {
            4
        } else {
            ch.width().unwrap_or(0)
        }
    };

    // continuations hang at the line's own indent; dropped when the pane is too
    // narrow to fit the indent plus the widest char (tab, 4), which would
    // overflow the pane and shift the divider
    let indent: usize = c
        .text
        .chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .map(col)
        .sum();
    let indent = if indent + 4 <= budget { indent } else { 0 };

    let mut segs = Vec::new();
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        g.chars().take(gutter_w).collect::<String>(),
        gutter_style,
    )];
    let mut glyphs = Vec::new();
    let mut used = 0usize;
    let mut cur = String::new();
    let mut cur_emph = false;
    for (bidx, ch) in c.text.char_indices() {
        let cw = col(ch);
        if cw > budget {
            // ponytail: char wider than the whole pane (tab in a sliver); drop it
            continue;
        }
        if used + cw > budget && used > 0 {
            // segment full: flush and start a continuation line
            if !cur.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut cur),
                    if cur_emph { emph } else { base },
                ));
            }
            if used < budget {
                spans.push(Span::styled(" ".repeat(budget - used), base));
            }
            segs.push(CellSegment {
                spans: std::mem::take(&mut spans),
                glyphs: std::mem::take(&mut glyphs),
            });
            spans.push(Span::styled(" ".repeat(gutter_w + indent), gutter_style));
            used = indent;
        }
        let in_emph = c
            .emph
            .iter()
            .take_while(|&&(s, _)| s <= bidx)
            .any(|&(s, e)| bidx >= s && bidx < e);
        if in_emph != cur_emph && !cur.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut cur),
                if cur_emph { emph } else { base },
            ));
        }
        cur_emph = in_emph;
        glyphs.push(VisualGlyph {
            columns: gutter_w + used..gutter_w + used + cw,
            bytes: bidx..bidx + ch.len_utf8(),
        });
        if ch == '\t' {
            cur.push_str("    ");
        } else {
            cur.push(ch);
        }
        used += cw;
    }
    if !cur.is_empty() {
        spans.push(Span::styled(cur, if cur_emph { emph } else { base }));
    }
    if used < budget {
        spans.push(Span::styled(" ".repeat(budget - used), base));
    }
    segs.push(CellSegment { spans, glyphs });
    segs
}

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

impl App {
    fn hit_test(&self, pane: Pane, x: usize, y: usize, width: usize, height: usize) -> Option<Hit> {
        if y >= height.saturating_sub(1) {
            return None;
        }
        let (left_w, right_w) = panes(width);
        let (pane_w, local_x) = match pane {
            Pane::Left if left_w > 0 => (left_w, x.min(left_w - 1)),
            Pane::Right if right_w > 0 => (right_w, x.saturating_sub(left_w + 1).min(right_w - 1)),
            _ => return None,
        };

        let mut row_y = y;
        let mut selected = None;
        for row_index in self.scroll..self.rows.len() {
            let row_h = row_height(&self.rows[row_index], left_w, right_w, self.gutter_w);
            if row_y < row_h {
                selected = Some((row_index, row_y));
                break;
            }
            row_y -= row_h;
        }
        let (row_index, row_y) = selected?;
        let cell = match &self.rows[row_index] {
            Row::Line { left, right } => match pane {
                Pane::Left => left.as_ref(),
                Pane::Right => right.as_ref(),
            }?,
            _ => return None,
        };
        let segments = cell_segments(Some(cell), pane_w, self.gutter_w);
        let segment = segments.get(row_y)?;
        let (start, end) = segment
            .glyphs
            .iter()
            .find(|glyph| glyph.columns.contains(&local_x))
            .map(|glyph| (glyph.bytes.start, glyph.bytes.end))
            .or_else(|| {
                if local_x < self.gutter_w.min(pane_w) {
                    Some(first_char(&cell.text))
                } else {
                    segment
                        .glyphs
                        .first()
                        .filter(|glyph| local_x < glyph.columns.start)
                        .map(|glyph| (glyph.bytes.start, glyph.bytes.end))
                        .or_else(|| Some(last_char(&cell.text)))
                }
            })?;
        Some(Hit {
            row: row_index,
            start,
            end,
        })
    }

    fn selection_text(&self, selection: Selection) -> String {
        let (start, end) = if selection.anchor <= selection.head {
            (selection.anchor, selection.head)
        } else {
            (selection.head, selection.anchor)
        };
        let mut parts = Vec::new();
        for row_index in start.row..=end.row {
            let Some(cell) = (match &self.rows[row_index] {
                Row::Line { left, right } => match selection.pane {
                    Pane::Left => left.as_ref(),
                    Pane::Right => right.as_ref(),
                },
                _ => None,
            }) else {
                continue;
            };
            let text = if start.row == end.row {
                &cell.text[start.start..end.end]
            } else if row_index == start.row {
                &cell.text[start.start..]
            } else if row_index == end.row {
                &cell.text[..end.end]
            } else {
                &cell.text
            };
            parts.push(text);
        }
        parts.join("\n")
    }

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
        self.selection = self
            .hit_test(pane, x, y, width, height)
            .map(|hit| Selection {
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

    pub fn finish_selection(&mut self) -> Option<String> {
        let selection = self.selection.take()?;
        if !selection.dragged {
            return None;
        }
        let text = self.selection_text(selection);
        (!text.is_empty()).then_some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    const SMALL: &str = "\
diff --git a/f.rs b/f.rs
--- a/f.rs
+++ b/f.rs
@@ -1,2 +1,2 @@
 ctx
-old line
+new line
";

    fn render(width: u16, height: u16, app: &App) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn renders_split_view_with_gutters() {
        let app = App::new(crate::diff::parse(SMALL.as_bytes()));
        let lines = render(40, 8, &app);
        assert_eq!(
            lines,
            vec![
                format!("f.rs{}+1 -1", " ".repeat(30)),
                "@@ -1,2 +1,2 @@".to_string(),
                "   1 ctx           │   1 ctx".to_string(),
                "   2 old line      │   2 new line".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                " j/k · ctrl-d/u · n/p · g/G · q".to_string(),
            ]
        );
    }

    #[test]
    fn selects_right_pane_text_only() {
        let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
        // At 40 columns the divider is x=19 and right-pane text starts at x=25.
        app.begin_selection(25, 2, 40, 8);
        app.drag_selection(32, 3, 40, 8);
        assert_eq!(app.finish_selection().as_deref(), Some("ctx\nnew line"));
    }

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
            left: Some(Cell {
                no: 1,
                text: text.into(),
                kind: Kind::Del,
                emph: vec![],
            }),
            right: None,
        }]);
        app.begin_selection(0, 0, 40, 5);
        app.drag_selection(18, 3, 40, 5);
        assert_eq!(app.finish_selection().as_deref(), Some(text));
    }

    #[test]
    fn selection_preserves_tab_and_unicode() {
        let mut app = App::new(vec![Row::Line {
            left: Some(Cell {
                no: 1,
                text: "\té".into(),
                kind: Kind::Ctx,
                emph: vec![],
            }),
            right: None,
        }]);
        app.begin_selection(5, 0, 40, 2);
        app.drag_selection(9, 0, 40, 2);
        assert_eq!(app.finish_selection().as_deref(), Some("\té"));
    }

    #[test]
    fn selection_skips_metadata_rows() {
        let cell = |no: u32, text: &str| {
            Some(Cell {
                no,
                text: text.into(),
                kind: Kind::Ctx,
                emph: vec![],
            })
        };
        let mut app = App::new(vec![
            Row::Line {
                left: cell(1, "one"),
                right: None,
            },
            Row::Hunk("@@ -1 +1 @@".into()),
            Row::Raw("metadata".into()),
            Row::Line {
                left: cell(2, "two"),
                right: None,
            },
        ]);
        app.begin_selection(5, 0, 40, 5);
        app.drag_selection(7, 3, 40, 5);
        assert_eq!(app.finish_selection().as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn selection_skips_absent_side_rows() {
        let cell = |no: u32, text: &str| {
            Some(Cell {
                no,
                text: text.into(),
                kind: Kind::Ctx,
                emph: vec![],
            })
        };
        let mut app = App::new(vec![
            Row::Line {
                left: cell(1, "one"),
                right: None,
            },
            Row::Line {
                left: None,
                right: cell(1, "right only"),
            },
            Row::Line {
                left: cell(2, "two"),
                right: None,
            },
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
            left: Some(Cell {
                no: 1,
                text: "left".into(),
                kind: Kind::Del,
                emph: vec![],
            }),
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
                left: Some(Cell {
                    no: 1,
                    text: "left".into(),
                    kind: Kind::Ctx,
                    emph: vec![],
                }),
                right: None,
            },
        ]);
        app.begin_selection(5, 0, 40, 3);
        app.drag_selection(8, 1, 40, 3);
        assert_eq!(app.finish_selection(), None);
    }

    #[test]
    fn scroll_skips_rows_and_wraps_long_lines() {
        let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
        app.rows.push(Row::Line {
            left: Some(crate::diff::Cell {
                no: 3,
                text: "a very long line that cannot fit in the pane".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: None,
        });
        app.scroll = 4;
        let lines = render(40, 3, &app);
        // 14-col budget: the line wraps, continuation gets a blank gutter
        assert_eq!(
            lines,
            vec![
                "   3 a very long li│".to_string(),
                "     ne that cannot│".to_string(),
                " j/k · ctrl-d/u · n/p · g/G · q".to_string(),
            ]
        );
    }

    #[test]
    fn wrapped_continuations_keep_indent() {
        let app = App::new(vec![Row::Line {
            left: Some(crate::diff::Cell {
                no: 1,
                text: "    abcdefghij0123456789".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: None,
        }]);
        let lines = render(40, 3, &app);
        assert_eq!(lines[0], "   1     abcdefghij│");
        assert_eq!(lines[1], "         0123456789│"); // blank gutter + 4-col hang
    }

    #[test]
    fn max_scroll_accounts_for_wrapped_heights() {
        let mut rows: Vec<Row> = (0..5).map(|i| Row::Raw(format!("r{i}"))).collect();
        rows.push(Row::Line {
            left: Some(crate::diff::Cell {
                no: 1,
                text: "a very long line that cannot fit in the pane".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: None,
        });
        let app = App::new(rows);
        // 40x5 terminal, 4 body lines: the tail row alone wraps to 4, so G stops on it
        assert_eq!(app.max_scroll(40, 5), 5);
    }

    #[test]
    fn max_scroll_bounds_flat_rows() {
        let rows: Vec<Row> = (0..10).map(|i| Row::Raw(format!("r{i}"))).collect();
        let app = App::new(rows);
        // 5-row terminal: 4 body rows -> max scroll 6
        assert_eq!(app.max_scroll(40, 5), 6);
    }

    #[test]
    fn max_scroll_never_scrolls_past_a_tall_tail_row() {
        let mut app = App::new(vec![Row::Line {
            left: Some(crate::diff::Cell {
                no: 1,
                text: "a very long line that cannot fit in the pane".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: None,
        }]);
        // the only row wraps to 4 lines, body is 3: G must land on it, not past it
        app.scroll = app.max_scroll(40, 4);
        assert_eq!(app.scroll, 0);
        let lines = render(40, 4, &app);
        assert!(!lines[0].is_empty(), "blank body at max scroll");
    }

    #[test]
    fn emph_survives_wrap_boundary() {
        let app = App::new(vec![Row::Line {
            left: Some(crate::diff::Cell {
                no: 1,
                text: "aaaaaaaaaaaaXXXX".into(), // budget 14 splits the emph run
                kind: crate::diff::Kind::Del,
                emph: vec![(12, 16)],
            }),
            right: None,
        }]);
        let mut term = Terminal::new(TestBackend::new(40, 3)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        // emph bg on the last cell before the wrap and the first text cell after
        assert_eq!(buf[(17u16, 0u16)].style().bg, Some(Color::Rgb(107, 43, 43)));
        assert_eq!(buf[(5u16, 1u16)].style().bg, Some(Color::Rgb(107, 43, 43)));
    }

    #[test]
    fn uneven_two_sided_wrap_keeps_divider_and_fills_short_side() {
        let app = App::new(vec![Row::Line {
            left: Some(crate::diff::Cell {
                no: 1,
                text: "left side text that wraps to three lines here".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: Some(crate::diff::Cell {
                no: 1,
                text: "short right".into(),
                kind: crate::diff::Kind::Add,
                emph: vec![],
            }),
        }]);
        let mut term = Terminal::new(TestBackend::new(40, 5)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        // left wraps to 4 lines; the divider must not drift on any of them
        for y in 0..4u16 {
            assert_eq!(buf[(19u16, y)].symbol(), "│", "divider drifted on line {y}");
        }
        // the right pane exists but is shorter: its continuations are dead-filled
        assert_eq!(buf[(20u16, 1u16)].style().bg, Some(Color::Indexed(234)));
    }

    #[test]
    fn zero_budget_pane_never_overflows_into_divider() {
        let app = App::new(vec![Row::Line {
            left: Some(crate::diff::Cell {
                no: 1,
                text: "long enough to overflow".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: None,
        }]);
        // width 11: left pane 5 == gutter width, so the text budget is exactly 0
        let mut term = Terminal::new(TestBackend::new(11, 2)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(5u16, 0u16)].symbol(), "│");
    }

    #[test]
    fn six_digit_line_numbers_render_in_full() {
        let app = App::new(vec![Row::Line {
            left: Some(crate::diff::Cell {
                no: 100000,
                text: "hello".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: None,
        }]);
        let lines = render(40, 2, &app);
        assert!(
            lines[0].starts_with("100000 hello"),
            "gutter truncated the line number: {:?}",
            lines[0]
        );
    }

    #[test]
    fn file_jump_finds_next_header() {
        let two = format!("{SMALL}{}", SMALL.replace("f.rs", "g.rs"));
        let mut app = App::new(crate::diff::parse(two.as_bytes()));
        assert_eq!(app.file_jump(true), Some(5)); // second File row, after the blank gap
        app.scroll = 5;
        assert_eq!(app.file_jump(false), Some(0));
    }

    #[test]
    fn header_counts_reset_per_file() {
        let two = format!("{SMALL}{}", SMALL.replace("f.rs", "g.rs"));
        let app = App::new(crate::diff::parse(two.as_bytes()));
        let lines = render(40, 8, &app);
        // a running total would show +2 -2 on the second header
        assert!(
            lines[5].starts_with("g.rs") && lines[5].ends_with("+1 -1"),
            "{:?}",
            lines[5]
        );
    }

    #[test]
    fn file_header_dims_directory_bolds_basename() {
        let app = App::new(vec![Row::File("src/ui.rs".into())]);
        let mut term = Terminal::new(TestBackend::new(20, 2)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        // "src/" dim, not bold
        assert_eq!(buf[(0u16, 0u16)].style().fg, Some(Color::DarkGray));
        assert!(
            !buf[(0u16, 0u16)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // "ui.rs" bold
        assert!(
            buf[(4u16, 0u16)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn absent_side_gets_faint_fill() {
        let app = App::new(vec![Row::Line {
            left: Some(crate::diff::Cell {
                no: 1,
                text: "gone".into(),
                kind: crate::diff::Kind::Del,
                emph: vec![],
            }),
            right: None,
        }]);
        let mut term = Terminal::new(TestBackend::new(40, 2)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(20u16, 0u16)].style().bg, Some(Color::Indexed(234)));
    }

    #[test]
    fn gutter_matches_line_background() {
        let app = App::new(crate::diff::parse(SMALL.as_bytes()));
        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        // row 3 pairs the del/add line: both gutters carry their row's bg
        assert_eq!(buf[(0u16, 3u16)].style().bg, Some(Color::Rgb(48, 27, 31)));
        assert_eq!(buf[(20u16, 3u16)].style().bg, Some(Color::Rgb(18, 38, 30)));
    }

    #[test]
    fn file_header_bar_spans_full_width() {
        let app = App::new(crate::diff::parse(SMALL.as_bytes()));
        let mut term = Terminal::new(TestBackend::new(40, 3)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(39u16, 0u16)].style().bg, Some(Color::Indexed(236)));
        // hunk band right below it, also full width
        assert_eq!(buf[(39u16, 1u16)].style().bg, Some(Color::Rgb(20, 34, 56)));
    }
}
