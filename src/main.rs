mod diff;
mod ui;

use std::io::Read;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const HELP: &str = "scopic — side-by-side diff viewer

usage:
  jj diff --git | scopic
  git diff | scopic
  scopic <file.diff>
";

fn main() -> Result<()> {
    let input = match std::env::args().nth(1).as_deref() {
        Some("--help" | "-h") => {
            print!("{HELP}");
            return Ok(());
        }
        Some(path) => std::fs::read(path)?,
        None => {
            let mut buf = Vec::new();
            std::io::stdin().read_to_end(&mut buf)?;
            buf
        }
    };

    let rows = diff::parse(&input);
    if rows.is_empty() {
        return Ok(()); // empty diff: quiet exit, like a well-behaved pager
    }

    let mut app = ui::App::new(rows);
    let mut term = ratatui::init(); // installs a terminal-restoring panic hook
    let res = run(&mut term, &mut app);
    ratatui::restore();
    res
}

fn run(term: &mut ratatui::DefaultTerminal, app: &mut ui::App) -> Result<()> {
    loop {
        let h = term.size()?.height as usize;
        let half = h / 2;
        let max = app.max_scroll(h);
        app.clamp_scroll(h);
        term.draw(|f| ui::draw(f, app))?;
        if let Event::Key(k) = event::read()? {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            match k.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Char('c') if ctrl => return Ok(()),
                KeyCode::Char('j') | KeyCode::Down => app.scroll = (app.scroll + 1).min(max),
                KeyCode::Char('k') | KeyCode::Up => app.scroll = app.scroll.saturating_sub(1),
                KeyCode::Char('d') if ctrl => app.scroll = (app.scroll + half).min(max),
                KeyCode::Char('u') if ctrl => app.scroll = app.scroll.saturating_sub(half),
                KeyCode::PageDown => app.scroll = (app.scroll + half).min(max),
                KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(half),
                KeyCode::Char('g') => app.scroll = 0,
                KeyCode::Char('G') => app.scroll = max,
                KeyCode::Char('n') => {
                    if let Some(i) = app.file_jump(true) {
                        app.scroll = i;
                    }
                }
                KeyCode::Char('p') => {
                    if let Some(i) = app.file_jump(false) {
                        app.scroll = i;
                    }
                }
                _ => {}
            }
        }
    }
}
