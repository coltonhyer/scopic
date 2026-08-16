use std::collections::HashMap;
use std::ops::Range;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ScreenRow {
    source: usize,
    segment: usize,
}

#[derive(Clone, Copy, Debug)]
struct Selection {
    pane: Pane,
    anchor: Hit,
    head: Hit,
    dragged: bool,
}

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

struct VisualGlyph {
    columns: Range<usize>,
    bytes: Range<usize>,
}

struct CellSegment {
    spans: Vec<Span<'static>>,
    glyphs: Vec<VisualGlyph>,
}

#[derive(Debug)]
struct FileSection {
    header: usize,
    body: Range<usize>,
    collapsed: bool,
}

pub struct App {
    pub rows: Vec<Row>,
    pub scroll: usize,
    gutter_w: usize,
    /// File header row index → (added, deleted) line counts for its section
    stats: HashMap<usize, (u32, u32)>,
    sections: Vec<FileSection>,
    pane_ratio: Option<(usize, usize)>,
    resizing_divider: bool,
    preserve_scroll: Option<usize>,
    selection: Option<Selection>,
    status: Option<(String, Style)>,
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
        let mut sections: Vec<FileSection> = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if matches!(row, Row::File(_)) {
                if let Some(previous) = sections.last_mut() {
                    previous.body.end = i;
                }
                sections.push(FileSection {
                    header: i,
                    body: i + 1..rows.len(),
                    collapsed: false,
                });
            }
        }
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
            sections,
            pane_ratio: None,
            resizing_divider: false,
            preserve_scroll: None,
            selection: None,
            status: None,
        }
    }

    /// smallest scroll where the tail still fits the viewport (a wrapped last
    /// page may run short); needs the width to sum wrapped row heights
    pub fn max_scroll(&self, w: usize, viewport_h: usize) -> usize {
        let body_h = viewport_h.saturating_sub(1);
        let (left_w, right_w) = self.pane_widths(w);
        let mut height = 0;
        let mut next = self.visible_rows().next_back().unwrap_or(0);
        for row in self.visible_rows().rev() {
            height += row_height(&self.rows[row], left_w, right_w, self.gutter_w);
            let capacity =
                body_h.saturating_sub(usize::from(self.sticky_header_for(row).is_some()));
            if height > capacity {
                return next;
            }
            next = row;
        }
        0
    }

    /// index of the next/prev `Row::File` relative to current scroll
    pub fn file_jump(&self, forward: bool) -> Option<usize> {
        if forward {
            self.sections
                .iter()
                .map(|section| section.header)
                .find(|&header| header > self.scroll)
        } else {
            self.sections
                .iter()
                .map(|section| section.header)
                .rev()
                .find(|&header| header < self.scroll)
        }
    }

    pub fn set_status(&mut self, status: String) {
        self.status = Some((
            status,
            Style::default()
                .fg(Color::Rgb(88, 166, 255))
                .add_modifier(Modifier::BOLD),
        ));
    }

    pub fn set_error(&mut self, status: String) {
        self.status = Some((status, Style::default().fg(Color::Rgb(248, 81, 73))));
    }

    pub fn clear_status(&mut self) {
        self.status = None;
    }

    fn section_index_for_row(&self, row: usize) -> Option<usize> {
        let index = self
            .sections
            .partition_point(|section| section.header <= row)
            .checked_sub(1)?;
        (row < self.sections[index].body.end).then_some(index)
    }

    fn row_visible(&self, row: usize) -> bool {
        self.section_index_for_row(row).is_none_or(|index| {
            let section = &self.sections[index];
            row == section.header || !section.collapsed
        })
    }

    fn header_collapsed(&self, row: usize) -> bool {
        self.section_index_for_row(row).is_some_and(|index| {
            let section = &self.sections[index];
            section.header == row && section.collapsed
        })
    }

    fn visible_rows(&self) -> impl DoubleEndedIterator<Item = usize> + '_ {
        (0..self.rows.len()).filter(|&row| self.row_visible(row))
    }

    fn visible_rows_from(&self, start: usize) -> impl Iterator<Item = usize> + '_ {
        (start..self.rows.len()).filter(|&row| self.row_visible(row))
    }

    fn sticky_header(&self) -> Option<usize> {
        self.sticky_header_for(self.scroll)
    }

    fn sticky_header_for(&self, row: usize) -> Option<usize> {
        let index = self.section_index_for_row(row)?;
        let section = &self.sections[index];
        (!section.collapsed && row > section.header).then_some(section.header)
    }

    fn screen_row(&self, y: usize, width: usize, height: usize) -> Option<ScreenRow> {
        let body_h = height.saturating_sub(1);
        if y >= body_h {
            return None;
        }
        let sticky = self.sticky_header();
        if let Some(source) = sticky
            && y == 0
        {
            return Some(ScreenRow { source, segment: 0 });
        }
        let sticky_offset = usize::from(sticky.is_some());
        let mut remaining = y.saturating_sub(sticky_offset);
        let (left_w, right_w) = self.pane_widths(width);
        for source in self.visible_rows_from(self.scroll) {
            let height = row_height(&self.rows[source], left_w, right_w, self.gutter_w);
            if remaining < height {
                return Some(ScreenRow {
                    source,
                    segment: remaining,
                });
            }
            remaining -= height;
        }
        None
    }

    fn pane_widths(&self, width: usize) -> (usize, usize) {
        let available = width.saturating_sub(1);
        let min = (self.gutter_w + 1).min(available / 2);
        let preferred = self
            .pane_ratio
            .and_then(|(left, total)| (total > 0).then(|| available.saturating_mul(left) / total))
            .unwrap_or(available / 2);
        let left = preferred.clamp(min, available.saturating_sub(min));
        (left, available - left)
    }
}

impl App {
    pub fn toggle_current_file(&mut self) -> bool {
        let Some(index) = self.section_index_for_row(self.scroll) else {
            return false;
        };
        self.cancel_interaction();
        self.preserve_scroll = None;
        self.sections[index].collapsed = !self.sections[index].collapsed;
        self.scroll = self.sections[index].header;
        true
    }

    pub fn scroll_down(&mut self, count: usize, max: usize) {
        self.preserve_scroll = None;
        for _ in 0..count {
            let Some(next) = (self.scroll + 1..self.rows.len()).find(|&row| self.row_visible(row))
            else {
                break;
            };
            if next > max {
                self.scroll = max;
                break;
            }
            self.scroll = next;
        }
    }

    pub fn scroll_up(&mut self, count: usize) {
        self.preserve_scroll = None;
        for _ in 0..count {
            let Some(previous) = (0..self.scroll).rev().find(|&row| self.row_visible(row)) else {
                break;
            };
            self.scroll = previous;
        }
    }

    pub fn normalize_scroll(&mut self, max: usize) {
        if self.preserve_scroll != Some(self.scroll) {
            self.preserve_scroll = None;
        }
        if self.preserve_scroll.is_some() {
            return;
        }
        if self.scroll > max
            && !self
                .sections
                .iter()
                .any(|section| section.header == self.scroll)
        {
            self.scroll = max;
        }
    }

    pub fn toggle_file_at(&mut self, y: usize, width: usize, height: usize) -> bool {
        let Some(screen) = self.screen_row(y, width, height) else {
            return false;
        };
        let Some(index) = self.section_index_for_row(screen.source) else {
            return false;
        };
        if self.sections[index].header != screen.source {
            return false;
        }
        self.cancel_interaction();
        self.sections[index].collapsed = !self.sections[index].collapsed;
        if y == 0 {
            self.scroll = self.sections[index].header;
            self.preserve_scroll = None;
        } else {
            self.preserve_scroll = Some(self.scroll);
        }
        true
    }
}

impl App {
    pub fn begin_resize(&mut self, x: usize, y: usize, width: usize, height: usize) -> bool {
        let hit = width > 1 && y < height.saturating_sub(1) && x == self.pane_widths(width).0;
        self.resizing_divider = hit;
        if hit {
            self.preserve_scroll = None;
            self.selection = None;
        }
        hit
    }

    pub fn drag_resize(&mut self, x: usize, width: usize) -> bool {
        if !self.resizing_divider {
            return false;
        }
        let available = width.saturating_sub(1);
        if available > 0 {
            let min = (self.gutter_w + 1).min(available / 2);
            let left = x.min(available).clamp(min, available.saturating_sub(min));
            self.pane_ratio = Some((left, available));
        }
        true
    }

    pub fn finish_resize(&mut self) -> bool {
        let was_resizing = self.resizing_divider;
        self.resizing_divider = false;
        was_resizing
    }

    pub fn reset_panes(&mut self) {
        self.pane_ratio = None;
        self.resizing_divider = false;
        self.preserve_scroll = None;
    }

    pub fn clear_scroll_preservation(&mut self) {
        self.preserve_scroll = None;
    }

    pub fn cancel_interaction(&mut self) {
        self.selection = None;
        self.resizing_divider = false;
    }
}

const FOOTER: &str = " j/k · ctrl-d/u · n/p · c collapse · g/G · = center · q";

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

// ponytail: counts by building the spans; fine at diff sizes
fn row_height(row: &Row, left_w: usize, right_w: usize, gutter_w: usize) -> usize {
    match row {
        Row::Line { left, right } => cell_segments(left.as_ref(), left_w, gutter_w, None)
            .len()
            .max(cell_segments(right.as_ref(), right_w, gutter_w, None).len()),
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
    let (left_w, right_w) = app.pane_widths(w);

    let sticky = app.sticky_header();
    let mut lines = Vec::new();
    if let Some(header) = sticky.filter(|_| body_h > 0) {
        lines.extend(render_rows(
            &app.rows[header],
            left_w,
            right_w,
            app.gutter_w,
            app.stats.get(&header).copied(),
            app.selection,
            header,
            false,
        ));
    }
    let remaining = body_h as usize - lines.len();
    lines.extend(
        app.visible_rows_from(app.scroll)
            .flat_map(|source| {
                render_rows(
                    &app.rows[source],
                    left_w,
                    right_w,
                    app.gutter_w,
                    app.stats.get(&source).copied(),
                    app.selection,
                    source,
                    app.header_collapsed(source),
                )
            })
            .take(remaining),
    );

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
    let (footer_text, footer_style) = app
        .status
        .as_ref()
        .map(|(text, style)| (text.as_str(), *style))
        .unwrap_or((FOOTER, dim()));
    f.render_widget(
        Paragraph::new(Line::styled(footer_text, footer_style)),
        footer,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_rows(
    row: &Row,
    left_w: usize,
    right_w: usize,
    gutter_w: usize,
    stats: Option<(u32, u32)>,
    selection: Option<Selection>,
    row_index: usize,
    collapsed: bool,
) -> Vec<Line<'static>> {
    let w = left_w + 1 + right_w;
    match row {
        Row::File(t) => {
            let bar = Style::default().bg(Color::Indexed(236));
            let split = t.rfind('/').map_or(0, |i| i + 1);
            let marker = if collapsed { "▸ " } else { "▾ " };
            let mut spans = vec![
                Span::styled(marker, bar.fg(Color::DarkGray)),
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
            let used = marker.width()
                + t.width()
                + counts
                    .iter()
                    .map(|s| s.content.as_ref().width())
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
            let selected_left = selection.and_then(|selection| {
                left.as_ref()
                    .and_then(|cell| selection.range_for(Pane::Left, row_index, cell.text.len()))
            });
            let selected_right = selection.and_then(|selection| {
                right
                    .as_ref()
                    .and_then(|cell| selection.range_for(Pane::Right, row_index, cell.text.len()))
            });
            let ls = cell_segments(left.as_ref(), left_w, gutter_w, selected_left);
            let rs = cell_segments(right.as_ref(), right_w, gutter_w, selected_right);
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
fn cell_segments(
    cell: Option<&Cell>,
    width: usize,
    gutter_w: usize,
    selected: Option<Range<usize>>,
) -> Vec<CellSegment> {
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
    let col = |grapheme: &str| {
        if grapheme == "\t" {
            4
        } else {
            grapheme.width()
        }
    };

    // continuations hang at the line's own indent; dropped when the pane is too
    // narrow to fit the indent plus the widest char (tab, 4), which would
    // overflow the pane and shift the divider
    let indent: usize = c
        .text
        .graphemes(true)
        .take_while(|grapheme| matches!(*grapheme, " " | "\t"))
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
    let mut cur_selected = false;
    let flush =
        |spans: &mut Vec<Span<'static>>, cur: &mut String, cur_emph: bool, cur_selected: bool| {
            if !cur.is_empty() {
                let style = if cur_emph { emph } else { base };
                spans.push(Span::styled(
                    std::mem::take(cur),
                    if cur_selected {
                        style.add_modifier(Modifier::REVERSED)
                    } else {
                        style
                    },
                ));
            }
        };
    for (bidx, grapheme) in c.text.grapheme_indices(true) {
        let cw = col(grapheme);
        if cw > budget {
            // ponytail: grapheme wider than the whole pane (tab in a sliver); drop it
            continue;
        }
        if used + cw > budget && used > 0 {
            // segment full: flush and start a continuation line
            flush(&mut spans, &mut cur, cur_emph, cur_selected);
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
        let in_selected = selected.as_ref().is_some_and(|range| range.contains(&bidx));
        if (in_emph, in_selected) != (cur_emph, cur_selected) && !cur.is_empty() {
            flush(&mut spans, &mut cur, cur_emph, cur_selected);
        }
        cur_emph = in_emph;
        cur_selected = in_selected;
        if cw == 0 {
            if let Some(glyph) = glyphs.last_mut() {
                glyph.bytes.end = bidx + grapheme.len();
            }
        } else {
            glyphs.push(VisualGlyph {
                columns: gutter_w + used..gutter_w + used + cw,
                bytes: bidx..bidx + grapheme.len(),
            });
        }
        if grapheme == "\t" {
            cur.push_str("    ");
        } else {
            cur.push_str(grapheme);
        }
        used += cw;
    }
    flush(&mut spans, &mut cur, cur_emph, cur_selected);
    if used < budget {
        spans.push(Span::styled(" ".repeat(budget - used), base));
    }
    segs.push(CellSegment { spans, glyphs });
    segs
}

impl App {
    fn hit_test(&self, pane: Pane, x: usize, y: usize, width: usize, height: usize) -> Option<Hit> {
        let (left_w, right_w) = self.pane_widths(width);
        let (pane_w, local_x) = match pane {
            Pane::Left if left_w > 0 => (left_w, x.min(left_w - 1)),
            Pane::Right if right_w > 0 => (right_w, x.saturating_sub(left_w + 1).min(right_w - 1)),
            _ => return None,
        };

        let screen = self.screen_row(y, width, height)?;
        let row_index = screen.source;
        let row_y = screen.segment;
        let cell = match &self.rows[row_index] {
            Row::Line { left, right } => match pane {
                Pane::Left => left.as_ref(),
                Pane::Right => right.as_ref(),
            }?,
            _ => return None,
        };
        let segments = cell_segments(Some(cell), pane_w, self.gutter_w, None);
        let segment = segments.get(row_y)?;
        let glyph = segment
            .glyphs
            .iter()
            .find(|glyph| glyph.columns.contains(&local_x))
            .or_else(|| {
                segment
                    .glyphs
                    .first()
                    .filter(|glyph| local_x < glyph.columns.start)
            })
            .or_else(|| segment.glyphs.last())?;
        Some(Hit {
            row: row_index,
            start: glyph.bytes.start,
            end: glyph.bytes.end,
        })
    }

    fn selection_text(&self, selection: Selection) -> String {
        let mut parts = Vec::new();
        for row_index in selection.anchor.row.min(selection.head.row)
            ..=selection.anchor.row.max(selection.head.row)
        {
            if !self.row_visible(row_index) {
                continue;
            }
            let Some(cell) = (match &self.rows[row_index] {
                Row::Line { left, right } => match selection.pane {
                    Pane::Left => left.as_ref(),
                    Pane::Right => right.as_ref(),
                },
                _ => None,
            }) else {
                continue;
            };
            if let Some(range) = selection.range_for(selection.pane, row_index, cell.text.len()) {
                parts.push(&cell.text[range]);
            }
        }
        parts.join("\n")
    }

    pub fn begin_selection(&mut self, x: usize, y: usize, width: usize, height: usize) {
        self.resizing_divider = false;
        let (left_w, _) = self.pane_widths(width);
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
            selection.dragged = hit != selection.anchor;
            selection.head = hit;
            self.selection = Some(selection);
        }
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

    fn cell(no: u32, text: &str, kind: Kind) -> Option<Cell> {
        Some(Cell {
            no,
            text: text.into(),
            kind,
            emph: vec![],
        })
    }

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

    fn section_rows() -> Vec<Row> {
        vec![
            Row::Raw("preamble".into()),
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "alpha", Kind::Ctx),
                right: cell(1, "alpha", Kind::Ctx),
            },
            Row::File("empty.rs".into()),
            Row::File("c.rs".into()),
            Row::Line {
                left: cell(1, "charlie", Kind::Ctx),
                right: cell(1, "charlie", Kind::Ctx),
            },
        ]
    }

    #[test]
    fn file_sections_include_empty_bodies_and_leave_preamble_unsectioned() {
        let app = App::new(section_rows());

        assert_eq!(
            app.sections
                .iter()
                .map(|section| (section.header, section.body.clone(), section.collapsed))
                .collect::<Vec<_>>(),
            vec![(1, 2..3, false), (3, 4..4, false), (4, 5..6, false)]
        );
        assert_eq!(app.section_index_for_row(0), None);
        assert_eq!(app.section_index_for_row(2), Some(0));
        assert_eq!(app.section_index_for_row(3), Some(1));
    }

    #[test]
    fn collapse_hides_only_the_current_file_body() {
        let mut app = App::new(section_rows());
        app.scroll = 2;

        assert!(app.toggle_current_file());
        assert_eq!(app.scroll, 1);
        assert_eq!(app.visible_rows().collect::<Vec<_>>(), vec![0, 1, 3, 4, 5]);
        let lines = render(41, 7, &app);
        assert!(lines[0].starts_with("▸ a.rs"), "{lines:?}");
        assert!(lines[1].starts_with("▾ empty.rs"), "{lines:?}");

        assert!(app.toggle_current_file());
        assert_eq!(
            app.visible_rows().collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn visible_scrolling_skips_collapsed_bodies() {
        let mut app = App::new(section_rows());
        app.scroll = 2;
        assert!(app.toggle_current_file());

        app.scroll_down(1, 5);
        assert_eq!(app.scroll, 3);
        app.scroll_down(1, 5);
        assert_eq!(app.scroll, 4);
        app.scroll_up(2);
        assert_eq!(app.scroll, 1);
    }

    #[test]
    fn copied_text_excludes_collapsed_file_bodies() {
        let mut app = App::new(vec![
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "alpha", Kind::Ctx),
                right: None,
            },
            Row::File("hidden.rs".into()),
            Row::Line {
                left: cell(1, "hidden", Kind::Ctx),
                right: None,
            },
            Row::File("c.rs".into()),
            Row::Line {
                left: cell(1, "charlie", Kind::Ctx),
                right: None,
            },
        ]);
        app.scroll = 2;
        assert!(app.toggle_current_file());
        let selection = Selection {
            pane: Pane::Left,
            anchor: Hit {
                row: 1,
                start: 0,
                end: 5,
            },
            head: Hit {
                row: 5,
                start: 0,
                end: 7,
            },
            dragged: true,
        };

        assert_eq!(app.selection_text(selection), "alpha\ncharlie");
    }

    #[test]
    fn collapsed_body_no_longer_contributes_to_max_scroll() {
        let mut app = App::new(vec![
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "one", Kind::Ctx),
                right: cell(1, "one", Kind::Ctx),
            },
            Row::Line {
                left: cell(2, "two", Kind::Ctx),
                right: cell(2, "two", Kind::Ctx),
            },
        ]);
        assert_eq!(app.max_scroll(41, 2), 2);

        assert!(app.toggle_current_file());
        assert_eq!(app.max_scroll(41, 2), 0);
    }

    #[test]
    fn sticky_header_appears_without_obscuring_the_top_content_row() {
        let mut app = App::new(vec![
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "one", Kind::Ctx),
                right: cell(1, "one", Kind::Ctx),
            },
            Row::Line {
                left: cell(2, "two", Kind::Ctx),
                right: cell(2, "two", Kind::Ctx),
            },
        ]);
        app.scroll = 1;

        let lines = render(41, 4, &app);
        assert!(lines[0].starts_with("▾ a.rs"), "{lines:?}");
        assert!(lines[1].contains("one"), "{lines:?}");
        assert!(lines[2].contains("two"), "{lines:?}");
    }

    #[test]
    fn real_header_is_not_duplicated_and_next_file_replaces_sticky_header() {
        let mut app = App::new(vec![
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "a", Kind::Ctx),
                right: cell(1, "a", Kind::Ctx),
            },
            Row::File("b.rs".into()),
            Row::Line {
                left: cell(1, "b", Kind::Ctx),
                right: cell(1, "b", Kind::Ctx),
            },
        ]);

        assert_eq!(
            render(41, 4, &app)
                .iter()
                .filter(|line| line.starts_with("▾ a.rs"))
                .count(),
            1
        );
        app.scroll = 3;
        assert!(render(41, 3, &app)[0].starts_with("▾ b.rs"));
    }

    #[test]
    fn clicking_sticky_header_collapses_its_file_and_anchors_real_header() {
        let mut app = App::new(vec![
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "one", Kind::Ctx),
                right: cell(1, "one", Kind::Ctx),
            },
        ]);
        app.scroll = 1;

        assert!(app.toggle_file_at(0, 41, 3));
        assert_eq!(app.scroll, 0);
        assert!(app.sections[0].collapsed);
    }

    #[test]
    fn real_header_toggle_preserves_screen_position_across_normalization() {
        let mut app = App::new(vec![
            Row::Raw("preamble".into()),
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "alpha", Kind::Ctx),
                right: None,
            },
            Row::Line {
                left: cell(2, "alpha two", Kind::Ctx),
                right: None,
            },
            Row::File("b.rs".into()),
            Row::Line {
                left: cell(1, "beta", Kind::Ctx),
                right: None,
            },
            Row::Line {
                left: cell(2, "beta two", Kind::Ctx),
                right: None,
            },
        ]);
        let (width, height) = (41, 6);
        app.scroll = 2;
        let starting_scroll = app.scroll;
        let before = render(width, height, &app);
        let header_y = before
            .iter()
            .position(|line| line.starts_with("▾ b.rs"))
            .unwrap();
        assert_eq!(header_y, 3);
        let above = before[..header_y].to_vec();

        assert!(app.toggle_file_at(header_y, width as usize, height as usize));
        let max = app.max_scroll(width as usize, height as usize);
        assert!(max < starting_scroll);
        app.normalize_scroll(max);
        app.cancel_interaction();
        app.normalize_scroll(app.max_scroll(width as usize, height as usize));
        let collapsed = render(width, height, &app);
        assert_eq!(
            collapsed.iter().position(|line| line.starts_with("▸ b.rs")),
            Some(header_y)
        );
        assert_eq!(&collapsed[..header_y], &above);

        assert!(app.toggle_file_at(header_y, width as usize, height as usize));
        app.normalize_scroll(app.max_scroll(width as usize, height as usize));
        app.cancel_interaction();
        app.normalize_scroll(app.max_scroll(width as usize, height as usize));
        let expanded = render(width, height, &app);
        assert_eq!(
            expanded.iter().position(|line| line.starts_with("▾ b.rs")),
            Some(header_y)
        );
        assert_eq!(&expanded[..header_y], &above);

        assert!(app.toggle_file_at(header_y, width as usize, height as usize));
        let max = app.max_scroll(width as usize, height as usize);
        app.scroll_down(1, max);
        assert!(app.preserve_scroll.is_none());
    }

    #[test]
    fn sticky_header_offset_maps_selection_to_the_content_below_it() {
        let mut app = App::new(vec![
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "abc", Kind::Ctx),
                right: None,
            },
        ]);
        app.scroll = 1;

        app.begin_selection(5, 1, 41, 3);
        app.drag_selection(6, 1, 41, 3);
        assert_eq!(app.finish_selection(), Some("ab".into()));
    }

    #[test]
    fn sticky_header_capacity_keeps_wrapped_tail_reachable() {
        let app = App::new(vec![
            Row::File("a.rs".into()),
            Row::Line {
                left: cell(1, "abcdefghij", Kind::Ctx),
                right: cell(1, "abcdefghij", Kind::Ctx),
            },
            Row::Line {
                left: cell(2, "tail", Kind::Ctx),
                right: cell(2, "tail", Kind::Ctx),
            },
        ]);

        assert_eq!(app.max_scroll(21, 4), 2);
    }

    #[test]
    fn renders_split_view_with_gutters() {
        let app = App::new(crate::diff::parse(SMALL.as_bytes()));
        let lines = render(40, 8, &app);
        assert_eq!(
            lines,
            vec![
                format!("▾ f.rs{}+1 -1", " ".repeat(28)),
                "@@ -1,2 +1,2 @@".to_string(),
                "   1 ctx           │   1 ctx".to_string(),
                "   2 old line      │   2 new line".to_string(),
                "".to_string(),
                "".to_string(),
                "".to_string(),
                " j/k · ctrl-d/u · n/p · c collapse · g/G".to_string(),
            ]
        );
    }

    #[test]
    fn active_selection_is_highlighted() {
        let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
        app.begin_selection(25, 2, 40, 8);
        app.drag_selection(27, 2, 40, 8);
        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer();
        assert!(
            buf[(25u16, 2u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
        assert!(
            !buf[(20u16, 2u16)]
                .style()
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn success_status_replaces_footer_in_accent_blue() {
        let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
        app.set_status(" copied 2 lines".into());
        assert_eq!(render(40, 8, &app)[7], " copied 2 lines");

        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let style = term.backend().buffer()[(1u16, 7u16)].style();
        assert_eq!(style.fg, Some(Color::Rgb(88, 166, 255)));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn error_status_replaces_footer_in_bright_red() {
        let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
        app.set_error(" copy failed: tmux exited".into());
        assert_eq!(render(40, 8, &app)[7], " copy failed: tmux exited");

        let mut term = Terminal::new(TestBackend::new(40, 8)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        assert_eq!(
            term.backend().buffer()[(1u16, 7u16)].style().fg,
            Some(Color::Rgb(248, 81, 73))
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
    fn same_cell_drag_does_not_copy() {
        let mut app = App::new(crate::diff::parse(SMALL.as_bytes()));
        app.begin_selection(25, 2, 40, 8);
        app.drag_selection(25, 2, 40, 8);
        assert_eq!(app.finish_selection(), None);
    }

    #[test]
    fn selection_includes_combining_marks() {
        let mut app = App::new(vec![Row::Line {
            left: cell(1, "x a\u{301}", Kind::Ctx),
            right: None,
        }]);
        app.begin_selection(5, 0, 40, 2);
        app.drag_selection(7, 0, 40, 2);
        assert_eq!(app.finish_selection().as_deref(), Some("x a\u{301}"));
    }

    #[test]
    fn selection_includes_entire_zwj_grapheme() {
        let mut app = App::new(vec![Row::Line {
            left: cell(1, "x👩‍💻y", Kind::Ctx),
            right: None,
        }]);
        app.begin_selection(5, 0, 40, 2);
        app.drag_selection(6, 0, 40, 2);
        assert_eq!(app.finish_selection().as_deref(), Some("x👩‍💻"));
    }

    #[test]
    fn trailing_zero_width_grapheme_hits_visible_glyph() {
        let app = App::new(vec![Row::Line {
            left: cell(1, "a\u{200b}", Kind::Ctx),
            right: None,
        }]);
        assert_eq!(
            app.hit_test(Pane::Left, 6, 0, 40, 2),
            Some(Hit {
                row: 0,
                start: 0,
                end: 4,
            })
        );
    }

    #[test]
    fn divider_resize_preserves_ratio_across_terminal_widths() {
        let mut app = App::new(vec![Row::Line {
            left: cell(1, "left", Kind::Ctx),
            right: cell(1, "right", Kind::Ctx),
        }]);

        assert!(app.begin_resize(20, 0, 41, 2));
        assert!(app.drag_resize(30, 41));
        assert!(app.finish_resize());
        assert_eq!(app.pane_widths(41), (30, 10));
        assert_eq!(app.pane_widths(81), (60, 20));
    }

    #[test]
    fn divider_resize_clamps_both_panes_to_readable_width() {
        let mut app = App::new(vec![Row::Line {
            left: cell(1, "left", Kind::Ctx),
            right: cell(1, "right", Kind::Ctx),
        }]);

        assert!(app.begin_resize(20, 0, 41, 2));
        assert!(app.drag_resize(0, 41));
        assert!(app.finish_resize());
        assert_eq!(app.pane_widths(41), (6, 34));

        assert!(app.begin_resize(6, 0, 41, 2));
        assert!(app.drag_resize(40, 41));
        assert!(app.finish_resize());
        assert_eq!(app.pane_widths(41), (34, 6));
    }

    #[test]
    fn pane_reset_restores_equal_split() {
        let mut app = App::new(vec![Row::Line {
            left: cell(1, "left", Kind::Ctx),
            right: cell(1, "right", Kind::Ctx),
        }]);

        assert!(app.begin_resize(20, 0, 41, 2));
        assert!(app.drag_resize(30, 41));
        assert!(app.finish_resize());
        app.reset_panes();
        assert_eq!(app.pane_widths(41), (20, 20));
    }

    #[test]
    fn uneven_split_moves_the_rendered_divider() {
        let mut app = App::new(vec![Row::Line {
            left: cell(1, "left", Kind::Ctx),
            right: cell(1, "right", Kind::Ctx),
        }]);

        assert!(app.begin_resize(20, 0, 41, 2));
        assert!(app.drag_resize(30, 41));
        assert!(app.finish_resize());
        assert_eq!(render(41, 2, &app)[0].chars().nth(30), Some('│'));
    }

    #[test]
    fn wide_header_keeps_counts_at_right_edge() {
        let app = App::new(vec![
            Row::File("界界界界.rs".into()),
            Row::Line {
                left: cell(1, "gone", Kind::Del),
                right: cell(1, "new", Kind::Add),
            },
        ]);
        let lines = render(20, 3, &app);
        assert!(lines[0].ends_with("+1 -1"), "{:?}", lines[0]);
    }

    #[test]
    fn continuation_gutter_hits_current_segment_start() {
        let app = App::new(vec![Row::Line {
            left: cell(1, "abcdefghijklmnop", Kind::Ctx),
            right: None,
        }]);
        assert_eq!(
            app.hit_test(Pane::Left, 0, 1, 40, 3),
            Some(Hit {
                row: 0,
                start: 14,
                end: 15,
            })
        );
    }

    #[test]
    fn wrapped_segment_padding_hits_current_segment_end() {
        let app = App::new(vec![Row::Line {
            left: cell(1, "abcdefghijklm界x", Kind::Ctx),
            right: None,
        }]);
        assert_eq!(
            app.hit_test(Pane::Left, 18, 0, 40, 3),
            Some(Hit {
                row: 0,
                start: 12,
                end: 13,
            })
        );
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
            left: cell(1, text, Kind::Del),
            right: None,
        }]);
        app.begin_selection(0, 0, 40, 5);
        app.drag_selection(18, 3, 40, 5);
        assert_eq!(app.finish_selection().as_deref(), Some(text));
    }

    #[test]
    fn selection_preserves_tab_and_unicode() {
        let mut app = App::new(vec![Row::Line {
            left: cell(1, "\té", Kind::Ctx),
            right: None,
        }]);
        app.begin_selection(5, 0, 40, 2);
        app.drag_selection(9, 0, 40, 2);
        assert_eq!(app.finish_selection().as_deref(), Some("\té"));
    }

    #[test]
    fn selection_skips_metadata_rows() {
        let mut app = App::new(vec![
            Row::Line {
                left: cell(1, "one", Kind::Ctx),
                right: None,
            },
            Row::Hunk("@@ -1 +1 @@".into()),
            Row::Raw("metadata".into()),
            Row::Line {
                left: cell(2, "two", Kind::Ctx),
                right: None,
            },
        ]);
        app.begin_selection(5, 0, 40, 5);
        app.drag_selection(7, 3, 40, 5);
        assert_eq!(app.finish_selection().as_deref(), Some("one\ntwo"));
    }

    #[test]
    fn selection_skips_absent_side_rows() {
        let mut app = App::new(vec![
            Row::Line {
                left: cell(1, "one", Kind::Ctx),
                right: None,
            },
            Row::Line {
                left: None,
                right: cell(1, "right only", Kind::Ctx),
            },
            Row::Line {
                left: cell(2, "two", Kind::Ctx),
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
            left: cell(1, "left", Kind::Del),
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
                left: cell(1, "left", Kind::Ctx),
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
                " j/k · ctrl-d/u · n/p · c collapse · g/G".to_string(),
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
            lines[5].starts_with("▾ g.rs") && lines[5].ends_with("+1 -1"),
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
        assert_eq!(buf[(2u16, 0u16)].style().fg, Some(Color::DarkGray));
        assert!(
            !buf[(2u16, 0u16)]
                .style()
                .add_modifier
                .contains(Modifier::BOLD)
        );
        // "ui.rs" bold
        assert!(
            buf[(6u16, 0u16)]
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
