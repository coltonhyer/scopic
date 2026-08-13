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
        MouseButton, MouseEvent, MouseEventKind,
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
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let write_result = match child.stdin.take() {
            Some(mut stdin) => stdin.write_all(text.as_bytes()),
            None => Err(std::io::Error::other("tmux stdin unavailable")),
        };
        let status = child.wait()?;
        if !status.success() {
            return Err(std::io::Error::other(format!("tmux exited with {status}")));
        }
        write_result?;
        return Ok(());
    }

    execute!(std::io::stdout(), CopyToClipboard::to_clipboard_from(text))
}

fn handle_mouse_event(
    app: &mut ui::App,
    m: MouseEvent,
    width: usize,
    height: usize,
) -> Option<String> {
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) if !m.modifiers.contains(KeyModifiers::SHIFT) => {
            app.clear_status();
            app.begin_selection(m.column as usize, m.row as usize, width, height);
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            app.drag_selection(m.column as usize, m.row as usize, width, height);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            return app.finish_selection();
        }
        MouseEventKind::Down(MouseButton::Left) => app.cancel_selection(),
        _ => {}
    }
    None
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
            Event::Mouse(m) => {
                if let Some(text) = handle_mouse_event(app, m, w, h) {
                    let lines = text.split('\n').count();
                    match copy_to_clipboard(&text) {
                        Ok(()) => app.set_status(format!(
                            " copied {lines} {}",
                            if lines == 1 { "line" } else { "lines" }
                        )),
                        Err(error) => app.set_error(format!(" copy failed: {error}")),
                    }
                }
                match m.kind {
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
                    _ => {}
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_drag_survives_shift_on_drag_and_release() {
        let mut app = ui::App::new(vec![diff::Row::Line {
            left: Some(diff::Cell {
                no: 1,
                text: "abc".into(),
                kind: diff::Kind::Ctx,
                emph: vec![],
            }),
            right: None,
        }]);
        let event = |kind, column, modifiers| MouseEvent {
            kind,
            column,
            row: 0,
            modifiers,
        };

        handle_mouse_event(
            &mut app,
            event(
                MouseEventKind::Down(MouseButton::Left),
                5,
                KeyModifiers::NONE,
            ),
            40,
            2,
        );
        handle_mouse_event(
            &mut app,
            event(
                MouseEventKind::Drag(MouseButton::Left),
                6,
                KeyModifiers::SHIFT,
            ),
            40,
            2,
        );
        assert_eq!(
            handle_mouse_event(
                &mut app,
                event(
                    MouseEventKind::Up(MouseButton::Left),
                    6,
                    KeyModifiers::SHIFT
                ),
                40,
                2,
            ),
            Some("ab".into())
        );
    }
}
