//! Interactive `gd config` editor.
//!
//! A short list of settings drawn two lines each (the row, then its help). ↑↓
//! moves between settings; ←/→ cycles the highlighted one through its allowed
//! values and writes the change to the DB immediately. Because the whole screen
//! re-reads the `language` setting on every frame, switching language updates
//! the labels live. Esc / q leaves.

use crate::commands::config::KNOWN;
use crate::i18n::Lang;
use crate::tui::theme::Theme;
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use gd_core::db::KeyStore;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::{Frame, Terminal};
use std::io;

/// Whether stderr is a terminal — the picker renders there, so this is the same
/// gate it uses to decide between interactive and plain output.
pub fn is_interactive() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(2) != 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

struct Editor<'a> {
    store: &'a mut KeyStore,
    cursor: usize,
}

impl Editor<'_> {
    fn lang(&self) -> Lang {
        Lang::resolve(self.store.get_setting("language").as_deref())
    }

    /// Current value of setting `i`, falling back to its effective default.
    fn value(&self, i: usize) -> String {
        let s = &KNOWN[i];
        self.store
            .get_setting(s.key)
            .unwrap_or_else(|| s.effective_default())
    }

    /// Whether setting `i` is still at its default (nothing stored yet).
    fn is_default(&self, i: usize) -> bool {
        self.store.get_setting(KNOWN[i].key).is_none()
    }

    /// Step the highlighted setting to its next (`forward`) or previous allowed
    /// value, wrapping around, and persist the change.
    fn cycle(&mut self, forward: bool) -> Result<()> {
        let s = &KNOWN[self.cursor];
        let current = self.value(self.cursor);
        let n = s.allowed.len();
        let pos = s.allowed.iter().position(|&v| v == current).unwrap_or(0);
        let next = if forward { (pos + 1) % n } else { (pos + n - 1) % n };
        self.store.set_setting(s.key, s.allowed[next])?;
        self.store.save()?;
        Ok(())
    }
}

pub fn edit(store: &mut KeyStore) -> Result<()> {
    let tty = std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    let tty_clone = tty.try_clone()?;

    terminal::enable_raw_mode()?;
    crossterm::execute!(io::stderr(), EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(tty);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let supports_truecolor = std::env::var("COLORTERM")
        .map(|v| v == "truecolor" || v == "24bit")
        .unwrap_or(false);
    let theme = if supports_truecolor {
        Theme::default_theme()
    } else {
        Theme::fallback()
    };

    let mut editor = Editor { store, cursor: 0 };
    let result = run_loop(&mut terminal, &mut editor, &theme);

    // Restore the terminal unconditionally, even if the loop errored.
    let _ = terminal.clear();
    let _ = crossterm::execute!(io::stderr(), LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    drop(tty_clone);

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::fs::File>>,
    editor: &mut Editor,
    theme: &Theme,
) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, editor, theme))?;

        let Event::Key(KeyEvent { code, modifiers, kind, .. }) = event::read()? else {
            continue;
        };
        // Some terminals report key-release too; act on the press only.
        if kind == KeyEventKind::Release {
            continue;
        }
        if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
            return Ok(());
        }
        match code {
            KeyCode::Esc | KeyCode::Char('q') => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => editor.cursor = editor.cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                if editor.cursor + 1 < KNOWN.len() {
                    editor.cursor += 1;
                }
            }
            KeyCode::Right | KeyCode::Enter | KeyCode::Char('l' | ' ') => editor.cycle(true)?,
            KeyCode::Left | KeyCode::Char('h') => editor.cycle(false)?,
            _ => {}
        }
    }
}

fn render(frame: &mut Frame, editor: &Editor, theme: &Theme) {
    let lang = editor.lang();
    let area = frame.area();

    let heights = [
        Constraint::Length(1),                       // title
        Constraint::Length(1),                       // blank
        Constraint::Length(u16::try_from(KNOWN.len() * 2).unwrap_or(u16::MAX)), // settings: row + help each
        Constraint::Min(0),                          // filler
        Constraint::Length(1),                       // footer
    ];
    let chunks = Layout::vertical(heights).split(area);

    let title = Line::from(vec![
        Span::styled(
            "  gd",
            Style::default().fg(theme.title_gd).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" config", Style::default().fg(theme.match_count)),
    ]);
    frame.render_widget(Paragraph::new(title), chunks[0]);

    render_settings(frame, chunks[2], editor, theme, lang);

    let footer = Line::from(vec![
        Span::styled("  ↑↓", Style::default().fg(theme.footer_key)),
        Span::styled(lang.pick(" move   ", " 選設定   "), Style::default().fg(theme.footer_desc)),
        Span::styled("←→", Style::default().fg(theme.footer_key)),
        Span::styled(lang.pick(" change   ", " 切換值   "), Style::default().fg(theme.footer_desc)),
        Span::styled("esc", Style::default().fg(theme.footer_key)),
        Span::styled(lang.pick(" done", " 離開"), Style::default().fg(theme.footer_desc)),
    ]);
    frame.render_widget(Paragraph::new(footer), chunks[4]);
}

fn render_settings(frame: &mut Frame, area: Rect, editor: &Editor, theme: &Theme, lang: Lang) {
    let constraints: Vec<Constraint> = (0..area.height).map(|_| Constraint::Length(1)).collect();
    let rows = Layout::vertical(constraints).split(area);

    for (i, s) in KNOWN.iter().enumerate() {
        let row_main = i * 2;
        if row_main >= rows.len() {
            break;
        }
        let selected = i == editor.cursor;

        let indicator = if selected { "  ▸ " } else { "    " };
        let key_style = if selected {
            Style::default().fg(theme.title_key).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.path_parent)
        };

        let mut spans = vec![
            Span::styled(indicator, Style::default().fg(theme.selected_indicator)),
            // Keys are ASCII, so byte-width padding lines the value column up.
            Span::styled(format!("{:<11}", s.key), key_style),
            Span::styled(
                editor.value(i),
                Style::default().fg(theme.path_basename).add_modifier(Modifier::BOLD),
            ),
        ];
        if editor.is_default(i) {
            spans.push(Span::styled(
                lang.pick("  (default)", "  （預設）"),
                Style::default().fg(theme.match_count),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), rows[row_main]);

        let row_help = row_main + 1;
        if row_help < rows.len() {
            let help = Line::from(vec![Span::styled(
                format!("      {}", s.help(lang)),
                Style::default().fg(theme.footer_desc),
            )]);
            frame.render_widget(Paragraph::new(help), rows[row_help]);
        }
    }
}

