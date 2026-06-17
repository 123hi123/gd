#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use crate::i18n::Lang;
use crate::tui::app::{App, LayoutMode};
use crate::tui::theme::Theme;
use gd_core::db::ResultSource;
use gd_core::path::display_with_tilde;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const INDICATOR_WIDTH: usize = 5;

pub fn render(frame: &mut Frame, app: &App, theme: &Theme, lang: Lang) {
    let area = frame.area();

    // Inline is the compact, prompt-anchored layout — its own top-down block
    // (query on top where the cursor is, list flowing down, footer last).
    if app.mode == LayoutMode::Inline {
        render_inline(frame, area, app, theme, lang);
        return;
    }

    // Centered fills the whole middle (so the spread can sit dead center);
    // traditional sizes the list to its entries so the footer hugs the bottom
    // of the list, exactly like the classic top-down picker.
    let candidates = match app.mode {
        LayoutMode::Centered => Constraint::Min(0),
        LayoutMode::Traditional | LayoutMode::Inline => {
            Constraint::Length(app.visible_count() as u16)
        }
    };

    let heights = [
        Constraint::Length(1), // title
        Constraint::Length(1), // blank
        candidates,            // candidates
        Constraint::Length(1), // blank
        Constraint::Length(1), // footer
    ];

    let chunks = Layout::vertical(heights).split(area);

    render_title(frame, chunks[0], app, theme);
    render_candidates(frame, chunks[2], app, theme);
    render_footer(frame, chunks[4], app, theme, lang);
}

/// Inline layout (fzf `--height --layout=reverse` style): the query echo on the
/// top row — the line the cursor is on, so the eye doesn't move — then the
/// best-first list flowing downward, with the key hints on the last row.
fn render_inline(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, lang: Lang) {
    let heights = [
        Constraint::Length(1), // title / query — on the prompt line
        Constraint::Min(0),    // candidates — fill the block, best first
        Constraint::Length(1), // footer (keys)
    ];
    let chunks = Layout::vertical(heights).split(area);

    // Candidates first: rendering them sets `sel_max_hscroll`, which the footer
    // reads to decide whether to advertise ←→. Order here is draw order, not
    // screen position (the rects fix that), so the hint stays current.
    render_candidates(frame, chunks[1], app, theme);
    render_title(frame, chunks[0], app, theme);
    render_footer(frame, chunks[2], app, theme, lang);
}

fn render_title(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let mut spans = vec![
        Span::styled("  gd", Style::default().fg(theme.title_gd).add_modifier(Modifier::BOLD)),
        Span::styled(" › ", Style::default().fg(theme.match_count)),
        Span::styled(&app.query, Style::default().fg(theme.title_key).add_modifier(Modifier::BOLD)),
    ];

    if app.filter_mode && !app.filter_query.is_empty() {
        spans.push(Span::styled("     filter: ", Style::default().fg(theme.match_count)));
        spans.push(Span::styled(&app.filter_query, Style::default().fg(theme.filter_prompt)));
    }

    // Position indicator. Centered: which rank is highlighted, e.g. "1/120"
    // (opens on the best match). Traditional: the visible window, e.g. "1-20/120".
    let total = app.total_matches();
    let count_text = match app.mode {
        LayoutMode::Centered | LayoutMode::Inline if total > 1 => {
            format!("{}/{total}", app.selected_rank())
        }
        LayoutMode::Traditional if total > app.viewport_size => format!(
            "{}-{}/{total}",
            app.scroll_offset + 1,
            (app.scroll_offset + app.viewport_size).min(total)
        ),
        _ => format!("{total} match{}", if total == 1 { "" } else { "es" }),
    };

    let left_len: usize = spans.iter().map(|s| s.content.len()).sum();
    let padding = (area.width as usize).saturating_sub(left_len + count_text.len());

    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled(count_text, Style::default().fg(theme.match_count)));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_candidates(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let rows_n = area.height as usize;
    let visible = app.visible_rows();
    let term_width = area.width as usize;

    let constraints: Vec<Constraint> = (0..rows_n).map(|_| Constraint::Length(1)).collect();
    let rows = Layout::vertical(constraints).split(area);

    for (row_idx, slot) in visible.iter().take(rows_n).enumerate() {
        // Empty padding row around the centered spread — leave it blank.
        let Some((is_selected, candidate)) = slot else {
            continue;
        };
        let selected = *is_selected;
        let path_str = display_with_tilde(&candidate.path);

        let (parent_part, base_part) = split_path(&path_str);

        let source_tag = match candidate.source {
            ResultSource::Link => ("link", theme.selected_indicator),
            ResultSource::History => ("hist", theme.title_key),
            ResultSource::Filesystem => ("scan", theme.match_count),
        };

        // The whole selected row sits on the highlight background — gutter
        // included — so it reads as one solid bar. A left half-block ▌ gives a
        // crisp accent edge that survives even when the bg barely shows (tmux /
        // 16-colour), where it becomes the load-bearing "this row" marker.
        let bg = if selected {
            Style::default().bg(theme.selected_bg)
        } else {
            Style::default()
        };
        let indicator = if selected { "▌    " } else { "     " };
        let indicator_style = if selected {
            bg.fg(theme.selected_indicator)
        } else {
            Style::default()
        };

        // Dim the other rows so the selected one is the lone bright singleton,
        // while keeping them readable (the user scans neighbours to compare).
        // The selected parent is lifted too, so the *whole* path reads bright.
        let (parent_fg, base_fg, base_style) = if selected {
            (theme.path_parent_bright, theme.path_basename, bg.add_modifier(Modifier::BOLD))
        } else {
            (theme.path_parent, theme.path_basename_dim, Style::default())
        };

        let invalid_prefix = if candidate.valid { "" } else { "✗ " };
        let tag_text = format!(" [{tag}]", tag = source_tag.0);

        // All widths below are *display columns*, not bytes. CJK dir names
        // (文件 / 下載 / 桌面) are 3 bytes but 2 columns each, so byte math
        // misaligned the tag column and the highlight bar exactly on the
        // user's most-used paths.
        let invalid_w = invalid_prefix.width();
        let tag_w = tag_text.width();
        let available = term_width.saturating_sub(INDICATOR_WIDTH + invalid_w + tag_w + 2);

        // Horizontal scroll only applies to the highlighted row; ←/→ pan it
        // toward the start to reveal a hidden prefix. The renderer alone knows
        // the available width, so it reports the max scroll back to the App.
        let max_scroll = path_str.width().saturating_sub(available);
        let eff_scroll = if selected {
            app.sel_max_hscroll.set(max_scroll);
            app.h_scroll.min(max_scroll)
        } else {
            0
        };

        let mut spans = vec![Span::styled(indicator, indicator_style)];
        if !invalid_prefix.is_empty() {
            spans.push(Span::styled(invalid_prefix, bg.fg(theme.invalid_mark)));
        }

        // path_w is the rendered display width of the path portion (ellipses
        // included), used to right-align the tag regardless of how it is built.
        let path_w;
        if eff_scroll > 0 {
            // Scrolled peek: a column window over the full path, splitting the
            // visible slice at the basename boundary so colours still hold.
            let win = h_window(&path_str, available, eff_scroll);
            let base_start = path_str.rfind('/').map_or(0, |p| p + 1);
            let cut = base_start.clamp(win.start, win.end);
            let parent_vis = &path_str[win.start..cut];
            let base_vis = &path_str[cut..win.end];

            let mut w = 0;
            if win.lead {
                spans.push(Span::styled("…", bg.fg(parent_fg)));
                w += 1;
            }
            if !parent_vis.is_empty() {
                spans.push(Span::styled(parent_vis.to_string(), bg.fg(parent_fg)));
                w += parent_vis.width();
            }
            if !base_vis.is_empty() {
                spans.push(Span::styled(base_vis.to_string(), base_style.fg(base_fg)));
                w += base_vis.width();
            }
            if win.trail {
                spans.push(Span::styled("…", bg.fg(parent_fg)));
                w += 1;
            }
            path_w = w;
        } else {
            // Default view: keep the basename whole, left-truncate the parent.
            let display_parent = truncate_parent(parent_part, base_part, available);
            path_w = display_parent.width() + base_part.width();
            spans.push(Span::styled(display_parent, bg.fg(parent_fg)));
            spans.push(Span::styled(base_part.to_string(), base_style.fg(base_fg)));
        }

        let content_w = INDICATOR_WIDTH + invalid_w + path_w + tag_w;
        let padding = term_width.saturating_sub(content_w);
        spans.push(Span::styled(" ".repeat(padding.max(1)), bg));
        // Only the selected row's tag keeps its bright source colour; the rest
        // recede to grey so the lone bright [hist] no longer competes with the
        // selection for the eye (one exclusive emphasis colour).
        let tag_fg = if selected { source_tag.1 } else { theme.match_count };
        spans.push(Span::styled(tag_text, bg.fg(tag_fg)));

        frame.render_widget(Paragraph::new(Line::from(spans)), rows[row_idx]);
    }
}

/// Default path view: keep the basename whole and left-truncate the parent
/// with a leading '…' so it fits `available` columns. Never splits a wide
/// (CJK) glyph; returns "" when there is no room for any parent at all.
fn truncate_parent(parent: &str, base: &str, available: usize) -> String {
    if parent.width() + base.width() <= available {
        return parent.to_string();
    }
    let base_w = available.min(base.width());
    let parent_budget = available.saturating_sub(base_w);
    if parent_budget <= 4 {
        return String::new();
    }
    let target_w = parent_budget - 1; // -1 for '…'
    let mut w = 0;
    let mut start = parent.len();
    for (i, ch) in parent.char_indices().rev() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > target_w {
            break;
        }
        w += cw;
        start = i;
    }
    format!("…{}", &parent[start..])
}

/// A horizontal window over `full`, `avail` columns wide, panned `scroll`
/// columns toward the start (0 = the default right-pinned view). Returns the
/// visible byte range plus whether a leading / trailing '…' is needed. One
/// column at each truncated edge is given to the ellipsis.
struct HWindow {
    start: usize,
    end: usize,
    lead: bool,
    trail: bool,
}

fn h_window(full: &str, avail: usize, scroll: usize) -> HWindow {
    let total = full.width();
    let max_scroll = total.saturating_sub(avail);
    let scroll = scroll.min(max_scroll);
    let win_start_col = max_scroll - scroll;
    let win_end_col = win_start_col + avail;
    let lead = win_start_col > 0;
    let trail = win_end_col < total;
    let text_lo = win_start_col + usize::from(lead);
    let text_hi = win_end_col.saturating_sub(usize::from(trail));

    let mut start = None;
    let mut end = 0usize;
    let mut col = 0usize;
    for (b, ch) in full.char_indices() {
        let next = col + ch.width().unwrap_or(0);
        if col >= text_lo && next <= text_hi {
            if start.is_none() {
                start = Some(b);
            }
            end = b + ch.len_utf8();
        }
        col = next;
    }
    let start = start.unwrap_or(end);
    HWindow { start, end, lead, trail }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, lang: Lang) {
    let key = |s: &'static str| Span::styled(s, Style::default().fg(theme.footer_key));
    let desc = |s: &'static str| Span::styled(s, Style::default().fg(theme.footer_desc));

    let mut spans = if app.filter_mode {
        vec![
            key("  ⏎"),
            desc(lang.pick(" jump   ", " 跳轉   ")),
            key("esc"),
            desc(lang.pick(" clear filter", " 清除篩選")),
        ]
    } else {
        vec![
            key("  ↑↓"),
            desc(lang.pick(" select   ", " 選擇   ")),
            key("⏎"),
            desc(lang.pick(" jump   ", " 跳轉   ")),
            key("/"),
            desc(lang.pick(" filter   ", " 篩選   ")),
            key("esc"),
            desc(lang.pick(" cancel", " 取消")),
        ]
    };

    // Only advertise ←→ when the highlighted row actually has a hidden prefix
    // to reveal — otherwise the keys would look dead.
    if app.sel_max_hscroll.get() > 0 {
        spans.push(key("   ←→"));
        spans.push(desc(lang.pick(" path", " 路徑")));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn split_path(path_str: &str) -> (&str, &str) {
    match path_str.rfind('/') {
        Some(pos) => (&path_str[..=pos], &path_str[pos + 1..]),
        None => ("", path_str),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, LayoutMode};
    use gd_core::db::{Candidate, ResultSource};
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;
    use std::path::PathBuf;

    /// Render the picker to an in-memory buffer over real (existing) dirs so
    /// `valid` is true and no ✗ prefix shifts the columns.
    fn render_rows(width: u16) -> (Vec<String>, ratatui::buffer::Buffer) {
        let root = std::env::temp_dir().join(format!("gd-render-{}", std::process::id()));
        let dirs = [
            "文件/dev/qwen2api-rs",     // CJK parent, selected (rank 0)
            "文件/docker/qwen2api-rs",  // CJK parent
            "下載/qwen2api",            // CJK parent, short
        ];
        let paths: Vec<PathBuf> = dirs.iter().map(|d| root.join(d)).collect();
        for p in &paths {
            std::fs::create_dir_all(p).unwrap();
        }
        let candidates: Vec<Candidate> = paths
            .iter()
            .map(|p| Candidate { path: p.clone(), score: 1.0, source: ResultSource::History })
            .collect();

        let theme = Theme::default_theme();
        let app = App::new("qwen2api".into(), &candidates, 5, LayoutMode::Traditional);
        let mut terminal = Terminal::new(TestBackend::new(width, 9)).unwrap();
        terminal.draw(|f| render(f, &app, &theme, Lang::En)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let mut lines = Vec::new();
        for y in 0..buf.area.height {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            lines.push(s);
        }
        std::fs::remove_dir_all(&root).ok();
        (lines, buf)
    }

    /// The source tag must be flush-right on every candidate row even when the
    /// path holds double-width CJK chars — the byte-vs-column bug used to shove
    /// it left by 2 columns per CJK pair, breaking alignment with ASCII rows.
    #[test]
    fn cjk_rows_keep_tag_flush_right() {
        let width = 56u16;
        let (lines, _) = render_rows(width);
        // Candidate rows are 2,3,4 (after title + blank); all three end in a tag.
        for y in 2..=4 {
            assert!(
                lines[y].trim_end().ends_with("[hist]"),
                "row {y} should end with the flush-right tag: {:?}",
                lines[y]
            );
        }
    }

    /// Read a buffer row back as the terminal would render it: advance past
    /// wide-char continuation cells (their stale symbol is covered by the glyph).
    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        let mut s = String::new();
        let mut x = 0u16;
        while x < buf.area.width {
            let sym = buf.cell((x, y)).unwrap().symbol();
            s.push_str(sym);
            x += u16::try_from(sym.width().max(1)).unwrap();
        }
        s
    }

    /// A long path scrolls horizontally on the highlighted row: ← reveals a
    /// hidden prefix (and pushes the basename out behind a trailing …), ←/→
    /// clamp, and moving the selection resets the scroll.
    #[test]
    fn highlighted_row_scrolls_to_reveal_prefix() {
        let root = std::env::temp_dir().join(format!("gd-hscroll-{}", std::process::id()));
        let deep = root.join("文件/projects/dev/docker/qwen2api-rs");
        std::fs::create_dir_all(&deep).unwrap();
        let candidates = vec![Candidate { path: deep, score: 1.0, source: ResultSource::History }];
        let theme = Theme::default_theme();
        let mut app = App::new("qwen2api".into(), &candidates, 5, LayoutMode::Traditional);
        let mut terminal = Terminal::new(TestBackend::new(56, 9)).unwrap();

        // Frame 1: default view — basename whole, leading … for the hidden left.
        terminal.draw(|f| render(f, &app, &theme, Lang::Zh)).unwrap();
        let default_row = row_text(terminal.backend().buffer(), 2);
        assert!(default_row.contains("qwen2api-rs"), "basename shown: {default_row:?}");
        assert!(default_row.trim_start_matches('▌').trim_start().starts_with('…'));
        let max = app.sel_max_hscroll.get();
        assert!(max > 0, "long path must be scrollable");

        // Footer advertises ←→ once the row is known to be scrollable.
        assert!(row_text(terminal.backend().buffer(), 4).contains("路徑"));

        // Scroll left to the start: the very first segment becomes visible.
        for _ in 0..10 {
            app.hscroll_left();
            terminal.draw(|f| render(f, &app, &theme, Lang::Zh)).unwrap();
        }
        assert_eq!(app.h_scroll, max, "← clamps at max");
        let scrolled = row_text(terminal.backend().buffer(), 2);
        assert!(scrolled.contains("~/"), "prefix revealed: {scrolled:?}");
        assert!(scrolled.contains('…'), "basename pushed out behind a trailing …");

        // Moving the selection resets the horizontal scroll.
        app.move_down();
        assert_eq!(app.h_scroll, 0, "scroll resets on selection change");

        std::fs::remove_dir_all(&root).ok();
    }

    /// The selected row carries a left ▌ gutter and a full-width highlight bar;
    /// other rows carry neither — the lone-bright-singleton contrast.
    #[test]
    fn selected_row_has_gutter_and_bar() {
        let (_, buf) = render_rows(56);
        // Row 2 = first (selected) candidate in traditional mode.
        assert_eq!(buf.cell((0, 2)).unwrap().symbol(), "▌", "selected gutter bar");
        assert_ne!(buf.cell((0, 2)).unwrap().bg, Color::Reset, "selected bg fill");
        // A non-selected candidate row has no gutter and no background.
        assert_eq!(buf.cell((0, 3)).unwrap().symbol(), " ", "no gutter on other rows");
        assert_eq!(buf.cell((0, 3)).unwrap().bg, Color::Reset, "no bg on other rows");

        // The [hist] tag is bright (source colour) only on the selected row;
        // other rows' tags recede to grey so they don't compete for the eye.
        let theme = Theme::default_theme();
        let tag_fg = |y| {
            let x = (0..buf.area.width).find(|&x| buf.cell((x, y)).unwrap().symbol() == "[").unwrap();
            buf.cell((x, y)).unwrap().fg
        };
        assert_eq!(tag_fg(2), theme.title_key, "selected tag keeps its bright source colour");
        assert_eq!(tag_fg(3), theme.match_count, "other tags dimmed to grey");
    }

    /// Inline layout: the query/title on the *first* row (where the cursor is,
    /// so the eye doesn't move), the best match right below it, and the key
    /// hints on the last row — top-down, fzf `--layout=reverse` style.
    #[test]
    fn inline_layout_puts_query_and_best_on_top() {
        let root = std::env::temp_dir().join(format!("gd-inline-{}", std::process::id()));
        let dirs = ["文件/dev/qwen2api-rs", "下載/qwen2api"];
        let paths: Vec<PathBuf> = dirs.iter().map(|d| root.join(d)).collect();
        for p in &paths {
            std::fs::create_dir_all(p).unwrap();
        }
        let candidates: Vec<Candidate> = paths
            .iter()
            .map(|p| Candidate { path: p.clone(), score: 1.0, source: ResultSource::History })
            .collect();

        let theme = Theme::default_theme();
        // viewport 2 (two matches) → block of 4 rows: query, 2 candidates, footer.
        let app = App::new("qwen2api".into(), &candidates, 2, LayoutMode::Inline);
        let mut terminal = Terminal::new(TestBackend::new(56, 4)).unwrap();
        terminal.draw(|f| render(f, &app, &theme, Lang::En)).unwrap();
        let buf = terminal.backend().buffer().clone();

        let line = |y: u16| {
            let mut s = String::new();
            for x in 0..buf.area.width {
                s.push_str(buf.cell((x, y)).unwrap().symbol());
            }
            s
        };

        assert!(
            line(0).contains("gd") && line(0).contains("qwen2api"),
            "query/title on the first row: {:?}",
            line(0)
        );
        // Best match (rank 0, selected) is the first candidate, right below the query.
        assert_eq!(buf.cell((0, 1)).unwrap().symbol(), "▌", "best match carries the gutter");
        assert!(line(1).contains("qwen2api-rs"), "best basename on row 1: {:?}", line(1));
        assert!(line(3).contains("select"), "footer on the last row: {:?}", line(3));

        std::fs::remove_dir_all(&root).ok();
    }
}
