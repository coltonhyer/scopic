#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ctx,
    Del,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub no: u32,
    pub text: String,
    pub kind: Kind,
    /// byte ranges of intraline emphasis within `text`
    pub emph: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// file header: "path" or "old → new"
    File(String),
    /// hunk header line, verbatim
    Hunk(String),
    /// anything we don't understand, shown dim
    Raw(String),
    Line {
        left: Option<Cell>,
        right: Option<Cell>,
    },
}

pub fn parse(input: &[u8]) -> Vec<Row> {
    let clean = strip_ansi(input);
    let text = String::from_utf8_lossy(&clean);

    let mut p = Parser::default();
    for line in text.lines() {
        p.line(line);
    }
    p.flush_pairs();
    p.flush_status();
    p.rows
}

fn strip_ansi(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if bytes.get(i + 1) == Some(&b'[') {
                i += 2;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                i += 1; // final byte
            } else {
                i += 2; // ESC + one char
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

#[derive(Default)]
struct Parser {
    rows: Vec<Row>,
    in_header: bool,
    in_hunk: bool,
    old_no: u32,
    new_no: u32,
    dels: Vec<Cell>,
    adds: Vec<Cell>,
    file_row: Option<usize>,
    title_fixed: bool, // rename title set; don't let +++ override
    minus_path: Option<String>,
    pending_status: Vec<String>,
    section_start: usize,
}

impl Parser {
    fn line(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            self.flush_pairs();
            self.flush_status();
            if !self.rows.is_empty() {
                self.rows.push(Row::Raw(String::new())); // blank gap between file sections
            }
            let name = rest.rfind(" b/").map(|p| &rest[p + 3..]).unwrap_or(rest);
            self.file_row = Some(self.rows.len());
            self.rows.push(Row::File(name.to_string()));
            self.section_start = self.rows.len();
            self.in_header = true;
            self.in_hunk = false;
            self.title_fixed = false;
            self.minus_path = None;
            return;
        }
        if line.starts_with("@@ ") && (self.in_header || self.in_hunk) {
            self.flush_pairs();
            self.in_header = false;
            match hunk_starts(line) {
                Some((old, new)) => {
                    self.rows.push(Row::Hunk(line.to_string()));
                    self.old_no = old;
                    self.new_no = new;
                    self.in_hunk = true;
                }
                None => {
                    // malformed hunk header: degrade to visible Raw, never swallow the file
                    self.rows.push(Row::Raw(line.to_string()));
                    self.in_hunk = false;
                }
            }
            return;
        }
        if self.in_header {
            self.header_line(line);
            return;
        }
        if self.in_hunk {
            let mut chars = line.chars();
            match chars.next() {
                Some(' ') | None => {
                    self.flush_pairs();
                    let text: String = chars.collect();
                    self.rows.push(Row::Line {
                        left: Some(Cell {
                            no: self.old_no,
                            text: text.clone(),
                            kind: Kind::Ctx,
                            emph: vec![],
                        }),
                        right: Some(Cell {
                            no: self.new_no,
                            text,
                            kind: Kind::Ctx,
                            emph: vec![],
                        }),
                    });
                    self.old_no = self.old_no.saturating_add(1);
                    self.new_no = self.new_no.saturating_add(1);
                }
                Some('-') => {
                    self.dels.push(Cell {
                        no: self.old_no,
                        text: chars.collect(),
                        kind: Kind::Del,
                        emph: vec![],
                    });
                    self.old_no = self.old_no.saturating_add(1);
                }
                Some('+') => {
                    self.adds.push(Cell {
                        no: self.new_no,
                        text: chars.collect(),
                        kind: Kind::Add,
                        emph: vec![],
                    });
                    self.new_no = self.new_no.saturating_add(1);
                }
                Some('\\') => {} // "\ No newline at end of file"
                _ => {
                    self.flush_pairs();
                    self.in_hunk = false;
                    self.rows.push(Row::Raw(line.to_string()));
                }
            }
            return;
        }
        self.rows.push(Row::Raw(line.to_string()));
    }

    fn header_line(&mut self, line: &str) {
        if let Some(p) = line.strip_prefix("--- ") {
            self.minus_path = Some(strip_prefix_ab(p).to_string());
        } else if let Some(p) = line.strip_prefix("+++ ") {
            if !self.title_fixed {
                let name = if p == "/dev/null" {
                    self.minus_path.clone().unwrap_or_default()
                } else {
                    strip_prefix_ab(p).to_string()
                };
                self.set_title(name);
            }
        } else if let Some(from) = line.strip_prefix("rename from ") {
            self.set_title(from.to_string());
        } else if let Some(to) = line.strip_prefix("rename to ") {
            let current = self.title();
            self.set_title(format!("{current} → {to}"));
            self.title_fixed = true;
        } else if line.starts_with("Binary files ") {
            self.rows.push(Row::Raw(line.to_string()));
        } else if ["new file mode", "deleted file mode", "old mode", "new mode"]
            .iter()
            .any(|p| line.starts_with(p))
        {
            // shown only if the section ends up hunk-less — then they're the whole story
            self.pending_status.push(line.to_string());
        }
        // everything else (index, similarity, GIT binary patch…) is swallowed
    }

    /// at section end: if nothing followed the File row, the mode headers are the content
    fn flush_status(&mut self) {
        let pending = std::mem::take(&mut self.pending_status);
        if self.rows.len() == self.section_start {
            self.rows.extend(pending.into_iter().map(Row::Raw));
        }
    }

    fn title(&self) -> String {
        match self.file_row.and_then(|i| self.rows.get(i)) {
            Some(Row::File(t)) => t.clone(),
            _ => String::new(),
        }
    }

    fn set_title(&mut self, t: String) {
        if let Some(i) = self.file_row {
            self.rows[i] = Row::File(t);
        }
    }

    fn flush_pairs(&mut self) {
        let dels = std::mem::take(&mut self.dels);
        let mut adds = std::mem::take(&mut self.adds);
        let n = dels.len().max(adds.len());
        let mut adds_iter = adds.drain(..);
        let mut dels_iter = dels.into_iter();
        for _ in 0..n {
            let mut left = dels_iter.next();
            let mut right = adds_iter.next();
            if let (Some(l), Some(r)) = (&mut left, &mut right) {
                let (ea, eb) = word_emph(&l.text, &r.text);
                l.emph = ea;
                r.emph = eb;
            }
            self.rows.push(Row::Line { left, right });
        }
    }
}

fn strip_prefix_ab(p: &str) -> &str {
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
}

/// "@@ -a[,b] +c[,d] @@ …" → (a, c)
fn hunk_starts(line: &str) -> Option<(u32, u32)> {
    let mut it = line.split_whitespace();
    it.next()?; // @@
    let old = it
        .next()?
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    let new = it
        .next()?
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse()
        .ok()?;
    Some((old, new))
}

type Ranges = Vec<(usize, usize)>;

fn word_emph(a: &str, b: &str) -> (Ranges, Ranges) {
    use similar::ChangeTag;
    // bounded: unminified-bundle-sized lines would otherwise freeze the TUI for minutes
    let td = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(5))
        .diff_unicode_words(a, b);
    let (mut ai, mut bi) = (0usize, 0usize);
    let (mut ea, mut eb) = (Vec::new(), Vec::new());
    for ch in td.iter_all_changes() {
        let len = ch.value().len();
        match ch.tag() {
            ChangeTag::Equal => {
                ai += len;
                bi += len;
            }
            ChangeTag::Delete => {
                ea.push((ai, ai + len));
                ai += len;
            }
            ChangeTag::Insert => {
                eb.push((bi, bi + len));
                bi += len;
            }
        }
    }
    (ea, eb)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASIC: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,3 +10,4 @@ fn main() {
 fn main() {
-    let x = 1;
+    let x = 2;
+    let y = 3;
 }
";

    fn cell(no: u32, text: &str, kind: Kind) -> Option<Cell> {
        Some(Cell {
            no,
            text: text.into(),
            kind,
            emph: vec![],
        })
    }

    // like `cell` but ignores emph when comparing; tests that don't care about
    // intraline strip emph from actual rows first
    fn without_emph(rows: Vec<Row>) -> Vec<Row> {
        rows.into_iter()
            .map(|r| match r {
                Row::Line { left, right } => Row::Line {
                    left: left.map(|mut c| {
                        c.emph.clear();
                        c
                    }),
                    right: right.map(|mut c| {
                        c.emph.clear();
                        c
                    }),
                },
                other => other,
            })
            .collect()
    }

    #[test]
    fn strip_ansi_removes_csi() {
        let colored = b"\x1b[31m-old line\x1b[0m\n";
        assert_eq!(strip_ansi(colored), b"-old line\n");
    }

    #[test]
    fn parses_single_hunk_with_pairing() {
        let rows = without_emph(parse(BASIC.as_bytes()));
        assert_eq!(
            rows,
            vec![
                Row::File("src/main.rs".into()),
                Row::Hunk("@@ -10,3 +10,4 @@ fn main() {".into()),
                Row::Line {
                    left: cell(10, "fn main() {", Kind::Ctx),
                    right: cell(10, "fn main() {", Kind::Ctx)
                },
                Row::Line {
                    left: cell(11, "    let x = 1;", Kind::Del),
                    right: cell(11, "    let x = 2;", Kind::Add)
                },
                Row::Line {
                    left: None,
                    right: cell(12, "    let y = 3;", Kind::Add)
                },
                Row::Line {
                    left: cell(12, "}", Kind::Ctx),
                    right: cell(13, "}", Kind::Ctx)
                },
            ]
        );
    }

    #[test]
    fn intraline_marks_changed_word() {
        let rows = parse(BASIC.as_bytes());
        let Row::Line {
            left: Some(l),
            right: Some(r),
        } = &rows[3]
        else {
            panic!("expected paired line, got {:?}", rows[3]);
        };
        assert_eq!(l.emph, vec![(12, 13)]); // the "1"
        assert_eq!(r.emph, vec![(12, 13)]); // the "2"
    }

    #[test]
    fn unequal_runs_leave_one_sided() {
        let input = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,2 +1,1 @@
-one
-two
+uno
";
        let rows = without_emph(parse(input.as_bytes()));
        assert_eq!(
            rows[2..],
            vec![
                Row::Line {
                    left: cell(1, "one", Kind::Del),
                    right: cell(1, "uno", Kind::Add)
                },
                Row::Line {
                    left: cell(2, "two", Kind::Del),
                    right: None
                },
            ]
        );
    }

    #[test]
    fn added_file_uses_new_path() {
        let input = "\
diff --git a/n.txt b/n.txt
new file mode 100644
--- /dev/null
+++ b/n.txt
@@ -0,0 +1 @@
+hi
";
        let rows = without_emph(parse(input.as_bytes()));
        assert_eq!(
            rows,
            vec![
                Row::File("n.txt".into()),
                Row::Hunk("@@ -0,0 +1 @@".into()),
                Row::Line {
                    left: None,
                    right: cell(1, "hi", Kind::Add)
                },
            ]
        );
    }

    #[test]
    fn rename_headers_merge_title() {
        let input = "\
diff --git a/old.rs b/new.rs
similarity index 90%
rename from old.rs
rename to new.rs
";
        assert_eq!(
            parse(input.as_bytes()),
            vec![Row::File("old.rs → new.rs".into())]
        );
    }

    #[test]
    fn binary_and_junk_become_raw() {
        let input = "\
garbage before anything
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
";
        assert_eq!(
            parse(input.as_bytes()),
            vec![
                Row::Raw("garbage before anything".into()),
                Row::Raw("".into()),
                Row::File("img.png".into()),
                Row::Raw("Binary files a/img.png and b/img.png differ".into()),
            ]
        );
    }

    #[test]
    fn malformed_hunk_header_degrades_to_raw_not_silence() {
        let input = "\
diff --git a/g b/g
--- a/g
+++ b/g
@@ -x,1 +1,1 @@
-a
+b
";
        let rows = parse(input.as_bytes());
        assert_eq!(
            rows,
            vec![
                Row::File("g".into()),
                Row::Raw("@@ -x,1 +1,1 @@".into()),
                Row::Raw("-a".into()),
                Row::Raw("+b".into()),
            ]
        );
    }

    #[test]
    fn huge_line_pair_completes_fast_with_valid_ranges() {
        let a: String = (0..5000).map(|i| format!("a{}.b{};", i, i % 97)).collect();
        let b: String = (0..5000).map(|i| format!("a{}.b{};", i, i % 89)).collect();
        let start = std::time::Instant::now();
        let (ea, eb) = word_emph(&a, &b);
        assert!(
            start.elapsed().as_secs() < 2,
            "intraline diff not time-bounded"
        );
        for (ranges, s) in [(&ea, &a), (&eb, &b)] {
            for &(lo, hi) in ranges.iter() {
                assert!(hi <= s.len() && s.is_char_boundary(lo) && s.is_char_boundary(hi));
            }
            // cell_spans' take_while cap silently depends on ascending, non-overlapping ranges
            for w in ranges.windows(2) {
                assert!(
                    w[0].1 <= w[1].0,
                    "ranges out of order: {:?} then {:?}",
                    w[0],
                    w[1]
                );
            }
        }
    }

    #[test]
    fn statusonly_sections_keep_mode_headers() {
        let input = "\
diff --git a/e b/e
new file mode 100644
index 0000000..e69de29
diff --git a/x b/x
old mode 100644
new mode 100755
";
        assert_eq!(
            parse(input.as_bytes()),
            vec![
                Row::File("e".into()),
                Row::Raw("new file mode 100644".into()),
                Row::Raw("".into()),
                Row::File("x".into()),
                Row::Raw("old mode 100644".into()),
                Row::Raw("new mode 100755".into()),
            ]
        );
    }

    #[test]
    fn sections_with_hunks_suppress_mode_headers() {
        let input = "\
diff --git a/n.txt b/n.txt
new file mode 100644
--- /dev/null
+++ b/n.txt
@@ -0,0 +1 @@
+hi
";
        let rows = parse(input.as_bytes());
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, Row::Raw(t) if t.contains("new file mode")))
        );
    }

    #[test]
    fn huge_hunk_start_numbers_saturate_instead_of_panicking() {
        let input = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -4294967294,3 +1,3 @@
 a
 b
 c
";
        let rows = parse(input.as_bytes());
        let Row::Line { left: Some(l), .. } = &rows[4] else {
            panic!("expected line row, got {:?}", rows[4]);
        };
        assert_eq!(l.no, u32::MAX);
    }

    #[test]
    fn empty_input_no_rows() {
        assert_eq!(parse(b""), vec![]);
        assert_eq!(parse(b"\n"), vec![Row::Raw("".into())]);
    }

    #[test]
    fn colored_input_parses_same() {
        let colored: String = BASIC
            .lines()
            .map(|l| format!("\x1b[1m\x1b[32m{l}\x1b[0m\n"))
            .collect();
        assert_eq!(parse(colored.as_bytes()), parse(BASIC.as_bytes()));
    }
}
