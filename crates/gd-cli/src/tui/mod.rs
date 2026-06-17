pub mod app;
pub mod config;
pub mod theme;
pub mod ui;

pub use app::LayoutMode;
use crate::i18n::Lang;
use app::{Action, App};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use gd_core::db::Candidate;
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};
use std::io;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

/// Cap on candidate rows for the inline (bottom) picker. The block stays
/// compact near the prompt; extra matches scroll into view.
const MAX_INLINE_ROWS: usize = 12;

type TtyTerminal = Terminal<CrosstermBackend<std::fs::File>>;

pub fn pick(
    key: &str,
    candidates: &[Candidate],
    mode: LayoutMode,
    lang: Lang,
) -> io::Result<Option<PathBuf>> {
    let tty = std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .unwrap_or_else(|_| {
            panic!("cannot open /dev/tty for TUI rendering");
        });

    let tty_clone = tty.try_clone()?;

    terminal::enable_raw_mode()?;

    let theme = pick_theme();
    let result = if mode.is_inline() {
        run_inline(tty, key, candidates, mode, lang, &theme)
    } else {
        run_fullscreen(tty, key, candidates, mode, lang, &theme)
    };

    terminal::disable_raw_mode()?;
    drop(tty_clone);

    result
}

fn pick_theme() -> theme::Theme {
    let supports_truecolor =
        std::env::var("COLORTERM").is_ok_and(|v| v == "truecolor" || v == "24bit");
    if supports_truecolor {
        theme::Theme::default_theme()
    } else {
        theme::Theme::fallback()
    }
}

/// Classic full-screen picker (centered / traditional): the alternate buffer
/// takes over the whole terminal, restored on exit.
fn run_fullscreen(
    tty: std::fs::File,
    key: &str,
    candidates: &[Candidate],
    mode: LayoutMode,
    lang: Lang,
    theme: &theme::Theme,
) -> io::Result<Option<PathBuf>> {
    crossterm::execute!(io::stderr(), EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(tty);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let term_height = terminal.size()?.height as usize;
    // viewport = available rows for candidates (total - title - blank - blank - footer)
    let viewport_size = term_height.saturating_sub(4).max(1);

    let mut app = App::new(key.to_string(), candidates, viewport_size, mode);
    let result = run_loop(&mut terminal, &mut app, theme, lang);

    terminal.clear()?;
    crossterm::execute!(io::stderr(), LeaveAlternateScreen)?;

    result
}

/// Inline picker (bottom): a compact block drawn in place at the prompt via an
/// inline viewport. ratatui anchors the block's bottom on the line `gd` was
/// invoked from (scrolling the terminal up only if there isn't room), so the
/// query line lands where you typed and the list grows upward above it. The
/// block is erased on exit so the shell's next prompt redraws cleanly.
fn run_inline(
    tty: std::fs::File,
    key: &str,
    candidates: &[Candidate],
    mode: LayoutMode,
    lang: Lang,
    theme: &theme::Theme,
) -> io::Result<Option<PathBuf>> {
    // crossterm reads the size from /dev/tty, so this is correct even though
    // gd's stdout is a pipe (`cd "$(gd …)"`).
    let term_height = terminal::size().map_or(24, |(_, rows)| rows as usize);

    // Content-sized candidate rows, capped, always leaving 2 rows for the
    // footer and the query line.
    let viewport_size = candidates
        .len()
        .min(MAX_INLINE_ROWS)
        .min(term_height.saturating_sub(2))
        .max(1);
    let height = u16::try_from((viewport_size + 2).min(term_height.max(1))).unwrap_or(u16::MAX);

    let mut app = App::new(key.to_string(), candidates, viewport_size, mode);

    // The inline viewport finds the prompt row via `crossterm::cursor::position()`,
    // which writes ESC[6n to *stdout* and waits for the terminal's reply. gd's
    // stdout is a pipe (`cd "$(gd …)"`), so the query would vanish into the pipe
    // and the read would time out ("cursor position could not be read"). Point
    // fd 1 at the tty for the whole session (rendering goes through the backend's
    // own fd, so nothing else cares), and restore the pipe before returning so
    // jump.rs can still print the chosen path to the shell.
    let tty_fd = tty.as_raw_fd();
    let saved_stdout = unsafe { libc::dup(libc::STDOUT_FILENO) };
    if saved_stdout >= 0 {
        unsafe { libc::dup2(tty_fd, libc::STDOUT_FILENO) };
    }

    let backend = CrosstermBackend::new(tty);
    let result = match Terminal::with_options(
        backend,
        TerminalOptions { viewport: Viewport::Inline(height) },
    ) {
        Ok(mut terminal) => {
            let _ = terminal.hide_cursor();
            let r = run_loop(&mut terminal, &mut app, theme, lang);
            // Erase the inline block (cursor → viewport top, clear downward) so
            // nothing lingers above the shell's next prompt.
            let _ = terminal.clear();
            let _ = terminal.show_cursor();
            r
        }
        Err(e) => Err(e),
    };

    if saved_stdout >= 0 {
        unsafe {
            libc::dup2(saved_stdout, libc::STDOUT_FILENO);
            libc::close(saved_stdout);
        }
    }

    result
}

fn run_loop(
    terminal: &mut TtyTerminal,
    app: &mut App,
    theme: &theme::Theme,
    lang: Lang,
) -> io::Result<Option<PathBuf>> {
    loop {
        terminal.draw(|frame| ui::render(frame, app, theme, lang))?;

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
            KeyCode::Left => {
                app.hscroll_left();
                Action::Continue
            }
            KeyCode::Right => {
                app.hscroll_right();
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
        KeyCode::Left | KeyCode::Char('h') => {
            app.hscroll_left();
            Action::Continue
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.hscroll_right();
            Action::Continue
        }
        KeyCode::Char('/') => {
            app.enter_filter();
            Action::Continue
        }
        _ => Action::Continue,
    }
}
