use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthChar;

use crate::diff::{Cell, Kind, Row};

pub struct App {
    pub rows: Vec<Row>,
    pub scroll: usize,
    gutter_w: usize,
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
        Self {
            rows,
            scroll: 0,
            gutter_w: digits + 1,
        }
    }

    pub fn max_scroll(&self, viewport_h: usize) -> usize {
        self.rows.len().saturating_sub(viewport_h.saturating_sub(1))
    }

    /// file jumps and shrinking resizes can leave `scroll` past the bound
    pub fn clamp_scroll(&mut self, viewport_h: usize) {
        self.scroll = self.scroll.min(self.max_scroll(viewport_h));
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

fn base_style(kind: Kind) -> Style {
    match kind {
        Kind::Ctx => Style::default(),
        Kind::Del => Style::default().bg(Color::Indexed(52)),
        Kind::Add => Style::default().bg(Color::Indexed(22)),
    }
}

fn emph_style(kind: Kind) -> Style {
    match kind {
        Kind::Ctx => Style::default(),
        Kind::Del => Style::default()
            .bg(Color::Indexed(88))
            .add_modifier(Modifier::BOLD),
        Kind::Add => Style::default()
            .bg(Color::Indexed(28))
            .add_modifier(Modifier::BOLD),
    }
}

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    if area.height == 0 {
        return;
    }
    let body_h = area.height.saturating_sub(1);
    let w = area.width as usize;
    let left_w = w.saturating_sub(1) / 2;
    let right_w = w.saturating_sub(1 + left_w);

    let lines: Vec<Line> = app
        .rows
        .iter()
        .skip(app.scroll)
        .take(body_h as usize)
        .map(|row| render_row(row, left_w, right_w, app.gutter_w))
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

fn render_row(row: &Row, left_w: usize, right_w: usize, gutter_w: usize) -> Line<'static> {
    match row {
        Row::File(t) => {
            let w = left_w + 1 + right_w;
            Line::styled(
                format!("{t:<w$}"),
                Style::default()
                    .bg(Color::Indexed(236))
                    .add_modifier(Modifier::BOLD),
            )
        }
        Row::Hunk(h) => Line::styled(h.clone(), dim()),
        Row::Raw(r) => Line::styled(r.clone(), dim()),
        Row::Line { left, right } => {
            let mut spans = cell_spans(left.as_ref(), left_w, gutter_w);
            spans.push(Span::styled("│", dim()));
            spans.extend(cell_spans(right.as_ref(), right_w, gutter_w));
            Line::from(spans)
        }
    }
}

fn cell_spans(cell: Option<&Cell>, width: usize, gutter_w: usize) -> Vec<Span<'static>> {
    let Some(c) = cell else {
        return vec![Span::raw(" ".repeat(width))];
    };
    let base = base_style(c.kind);
    let emph = emph_style(c.kind);
    let gutter_w = gutter_w.min(width);
    let g = format!("{:>w$} ", c.no, w = gutter_w.saturating_sub(1));
    let mut spans = vec![Span::styled(
        g.chars().take(gutter_w).collect::<String>(),
        dim(),
    )];
    let budget = width - gutter_w;

    // does the expanded text fit? (tab = 4 cols)
    // ponytail: per-char widths; ZWJ emoji clusters (👩‍💻) over-count and shift the
    // divider on that row only — switch to grapheme segmentation if it ever matters
    let col = |ch: char| {
        if ch == '\t' {
            4
        } else {
            ch.width().unwrap_or(0)
        }
    };
    let full: usize = c.text.chars().map(col).sum();
    let fits = full <= budget;
    let budget_eff = if fits {
        budget
    } else {
        budget.saturating_sub(1)
    };

    let mut used = 0usize;
    let mut cur = String::new();
    let mut cur_emph = false;
    for (bidx, ch) in c.text.char_indices() {
        let cw = col(ch);
        if used + cw > budget_eff {
            break;
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
    if !fits && budget > 0 {
        spans.push(Span::styled("…", dim()));
        used += 1;
    }
    if used < budget {
        spans.push(Span::styled(" ".repeat(budget - used), base));
    }
    spans
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
                "f.rs".to_string(),
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
    fn scroll_skips_rows_and_truncates_long_lines() {
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
        assert_eq!(
            lines,
            vec![
                "   3 a very long l…│".to_string(),
                "".to_string(),
                " j/k · ctrl-d/u · n/p · g/G · q".to_string(),
            ]
        );
    }

    #[test]
    fn clamp_scroll_respects_viewport() {
        let rows: Vec<Row> = (0..10).map(|i| Row::Raw(format!("r{i}"))).collect();
        let mut app = App::new(rows);
        app.scroll = 9; // e.g. `n` jumped to a header near the end
        app.clamp_scroll(5); // 5-row terminal: 4 body rows -> max scroll 6
        assert_eq!(app.scroll, 6);
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
    fn file_header_bar_spans_full_width() {
        let app = App::new(crate::diff::parse(SMALL.as_bytes()));
        let mut term = Terminal::new(TestBackend::new(40, 3)).unwrap();
        term.draw(|f| draw(f, &app)).unwrap();
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(39u16, 0u16)].style().bg, Some(Color::Indexed(236)));
    }
}
