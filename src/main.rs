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

fn key_event_is_actionable(kind: KeyEventKind) -> bool {
    matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

fn format_tmux_error(status: &str, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr)
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|ch| !ch.is_control())
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if stderr.is_empty() {
        format!("tmux exited with {status}")
    } else {
        stderr
    }
}

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
            .stderr(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take();
        let write_result = match stdin.as_mut() {
            Some(stdin) => stdin.write_all(text.as_bytes()),
            None => Err(std::io::Error::other("tmux stdin unavailable")),
        };
        drop(stdin);
        let output = child.wait_with_output()?;
        if !output.status.success() {
            return Err(std::io::Error::other(format_tmux_error(
                &output.status.to_string(),
                &output.stderr,
            )));
        }
        return write_result;
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
            if !app.toggle_file_at(m.row as usize, width, height)
                && !app.begin_resize(m.column as usize, m.row as usize, width, height)
            {
                app.begin_selection(m.column as usize, m.row as usize, width, height);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if !app.drag_resize(m.column as usize, width) {
                app.drag_selection(m.column as usize, m.row as usize, width, height);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if !app.finish_resize() {
                return app.finish_selection();
            }
        }
        MouseEventKind::Down(MouseButton::Left) => app.cancel_interaction(),
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
                        app.cancel_interaction();
                        app.clear_status();
                        app.scroll_down(3, max);
                    }
                    MouseEventKind::ScrollUp => {
                        app.cancel_interaction();
                        app.clear_status();
                        app.scroll_up(3);
                    }
                    _ => {}
                }
            }
            Event::Key(k) => {
                if !key_event_is_actionable(k.kind) {
                    continue;
                }
                app.cancel_interaction();
                app.clear_status();
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char('c') if ctrl => return Ok(()),
                    KeyCode::Char('j') | KeyCode::Down => app.scroll_down(1, max),
                    KeyCode::Char('k') | KeyCode::Up => app.scroll_up(1),
                    KeyCode::Char('d') if ctrl => app.scroll_down(half, max),
                    KeyCode::Char('u') if ctrl => app.scroll_up(half),
                    KeyCode::PageDown => app.scroll_down(half, max),
                    KeyCode::PageUp => app.scroll_up(half),
                    KeyCode::Char('c') => {
                        app.toggle_current_file();
                    }
                    KeyCode::Char('g') => app.scroll = 0,
                    KeyCode::Char('G') => app.scroll = max,
                    KeyCode::Char('=') => app.reset_panes(),
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
            Event::Resize(_, _) => app.cancel_interaction(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_repeats_are_processed_but_releases_are_ignored() {
        assert!(key_event_is_actionable(KeyEventKind::Press));
        assert!(key_event_is_actionable(KeyEventKind::Repeat));
        assert!(!key_event_is_actionable(KeyEventKind::Release));
    }

    #[test]
    fn tmux_error_prefers_trimmed_stderr() {
        assert_eq!(
            format_tmux_error("exit status: 1", b"\n tmux: denied \n"),
            "tmux: denied"
        );
    }

    #[test]
    fn tmux_error_uses_status_when_stderr_is_blank() {
        assert_eq!(
            format_tmux_error("exit status: 1", b" \n\t"),
            "tmux exited with exit status: 1"
        );
    }

    #[test]
    fn tmux_error_strips_terminal_controls() {
        assert_eq!(
            format_tmux_error("exit status: 1", b"\x1b[31mdenied\x07\0"),
            "[31mdenied"
        );
    }

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

    #[test]
    fn divider_drag_is_consumed_without_copying() {
        let mut app = ui::App::new(vec![diff::Row::Line {
            left: Some(diff::Cell {
                no: 1,
                text: "left".into(),
                kind: diff::Kind::Ctx,
                emph: vec![],
            }),
            right: Some(diff::Cell {
                no: 1,
                text: "right".into(),
                kind: diff::Kind::Ctx,
                emph: vec![],
            }),
        }]);
        let event = |kind, column| MouseEvent {
            kind,
            column,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };

        handle_mouse_event(
            &mut app,
            event(MouseEventKind::Down(MouseButton::Left), 20),
            41,
            2,
        );
        assert!(app.finish_resize());

        handle_mouse_event(
            &mut app,
            event(MouseEventKind::Down(MouseButton::Left), 20),
            41,
            2,
        );
        handle_mouse_event(
            &mut app,
            event(MouseEventKind::Drag(MouseButton::Left), 30),
            41,
            2,
        );
        assert_eq!(
            handle_mouse_event(
                &mut app,
                event(MouseEventKind::Up(MouseButton::Left), 30),
                41,
                2,
            ),
            None
        );
        assert!(!app.finish_resize());
    }

    fn collapsible_app() -> ui::App {
        ui::App::new(vec![
            diff::Row::File("a.rs".into()),
            diff::Row::Line {
                left: Some(diff::Cell {
                    no: 1,
                    text: "alpha".into(),
                    kind: diff::Kind::Ctx,
                    emph: vec![],
                }),
                right: None,
            },
        ])
    }

    #[test]
    fn header_click_collapses_before_resize_or_selection() {
        let mut app = collapsible_app();
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(app.max_scroll(41, 2), 1);

        assert_eq!(handle_mouse_event(&mut app, event, 41, 2), None);
        assert_eq!(app.max_scroll(41, 2), 0);
        assert!(!app.finish_resize());
        assert_eq!(app.finish_selection(), None);
    }

    #[test]
    fn shift_click_on_header_keeps_native_selection_fallback() {
        let mut app = collapsible_app();
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: 0,
            modifiers: KeyModifiers::SHIFT,
        };

        assert_eq!(handle_mouse_event(&mut app, event, 41, 2), None);
        assert_eq!(app.max_scroll(41, 2), 1);
    }
}
