pub mod app;
pub mod theme;
pub mod ui;

use app::{Action, App};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use gd_core::db::Candidate;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;

pub fn pick(key: &str, candidates: &[Candidate]) -> io::Result<Option<PathBuf>> {
    let tty = std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .unwrap_or_else(|_| {
            panic!("cannot open /dev/tty for TUI rendering");
        });

    let tty_clone = tty.try_clone()?;

    terminal::enable_raw_mode()?;
    crossterm::execute!(io::stderr(), EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(tty);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let term_height = terminal.size()?.height as usize;
    // viewport = available rows for candidates (total - title - blank - blank - footer)
    let viewport_size = term_height.saturating_sub(4).max(1);

    let supports_truecolor = std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false);

    let theme = if supports_truecolor {
        theme::Theme::default_theme()
    } else {
        theme::Theme::fallback()
    };

    let mut app = App::new(key.to_string(), candidates, viewport_size);

    let result = run_loop(&mut terminal, &mut app, &theme);

    terminal.clear()?;
    crossterm::execute!(io::stderr(), LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    drop(tty_clone);

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::fs::File>>,
    app: &mut App,
    theme: &theme::Theme,
) -> io::Result<Option<PathBuf>> {
    loop {
        terminal.draw(|frame| ui::render(frame, app, theme))?;

        if let Event::Key(KeyEvent { code, modifiers, .. }) = event::read()? {
            let action = handle_key(app, code, modifiers);
            match action {
                Action::Select(path) => return Ok(Some(path)),
                Action::Cancel => return Ok(None),
                Action::Continue => {}
            }
        }
    }
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Action {
    if modifiers.contains(KeyModifiers::CONTROL) {
        return match code {
            KeyCode::Char('c') => Action::Cancel,
            KeyCode::Char('u') if app.filter_mode => {
                app.filter_clear();
                Action::Continue
            }
            _ => Action::Continue,
        };
    }

    if app.filter_mode {
        return match code {
            KeyCode::Esc => {
                app.exit_filter();
                Action::Continue
            }
            KeyCode::Enter => app.select_current(),
            KeyCode::Backspace => {
                app.filter_pop();
                Action::Continue
            }
            KeyCode::Char(c) => {
                app.filter_push(c);
                Action::Continue
            }
            KeyCode::Up => {
                app.move_up();
                Action::Continue
            }
            KeyCode::Down => {
                app.move_down();
                Action::Continue
            }
            _ => Action::Continue,
        };
    }

    match code {
        KeyCode::Esc => Action::Cancel,
        KeyCode::Enter => app.select_current(),
        KeyCode::Up | KeyCode::Char('k') => {
            app.move_up();
            Action::Continue
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.move_down();
            Action::Continue
        }
        KeyCode::Char('/') => {
            app.enter_filter();
            Action::Continue
        }
        _ => Action::Continue,
    }
}
