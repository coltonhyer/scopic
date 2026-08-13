mod diff;
mod ui;

use std::{
    io::{Read, Write},
    process::{Command, Stdio},
};

use ratatui::crossterm::{
    clipboard::CopyToClipboard,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
};

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
        Some(path) => match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("scopic: {path}: {e}");
                std::process::exit(1);
            }
        },
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
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    // ratatui's panic hook doesn't know about mouse capture; release it first
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        prev(info);
    }));
    let res = run(&mut term, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    res
}

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

    execute!(std::io::stdout(), CopyToClipboard::to_clipboard_from(text))
}

fn run(term: &mut ratatui::DefaultTerminal, app: &mut ui::App) -> Result<()> {
    loop {
        let size = term.size()?;
        let (w, h) = (size.width as usize, size.height as usize);
        let half = h / 2;
        // file jumps and shrinking resizes can leave scroll past the bound
        let max = app.max_scroll(w, h);
        app.scroll = app.scroll.min(max);
        term.draw(|f| ui::draw(f, app))?;
        match event::read()? {
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
            Event::Key(k) => {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                app.cancel_selection();
                app.clear_status();
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
            Event::Resize(_, _) => app.cancel_selection(),
            _ => {}
        }
    }
}
