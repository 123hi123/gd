#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use crate::tui::app::App;
use crate::tui::theme::Theme;
use gd_core::db::ResultSource;
use gd_core::path::display_with_tilde;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

const INDICATOR_WIDTH: usize = 5;

pub fn render(frame: &mut Frame, app: &App, theme: &Theme) {
    let area = frame.area();
    let visible = app.visible_candidates();
    let candidate_count = visible.len();

    let heights = [
        Constraint::Length(1),                       // title
        Constraint::Length(1),                       // blank
        Constraint::Length(candidate_count as u16),  // candidates (1 line each in viewport)
        Constraint::Length(1),                       // blank
        Constraint::Length(1),                       // footer
    ];

    let chunks = Layout::vertical(heights).split(area);

    render_title(frame, chunks[0], app, theme);
    render_candidates(frame, chunks[2], app, theme);
    render_footer(frame, chunks[4], app, theme);
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

    // Scroll indicator: "3/120 matches" or "120 matches"
    let total = app.total_matches();
    let count_text = if total > app.viewport_size {
        format!("{}-{}/{total}", app.scroll_offset + 1, (app.scroll_offset + app.viewport_size).min(total))
    } else {
        format!("{total} match{}", if total == 1 { "" } else { "es" })
    };

    let left_len: usize = spans.iter().map(|s| s.content.len()).sum();
    let padding = (area.width as usize).saturating_sub(left_len + count_text.len());

    spans.push(Span::raw(" ".repeat(padding)));
    spans.push(Span::styled(count_text, Style::default().fg(theme.match_count)));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_candidates(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let visible = app.visible_candidates();
    let term_width = area.width as usize;

    let constraints: Vec<Constraint> = visible.iter().map(|_| Constraint::Length(1)).collect();
    let rows = Layout::vertical(constraints).split(area);

    for (row_idx, (is_selected, candidate)) in visible.iter().enumerate() {
        let path_str = display_with_tilde(&candidate.path);

        let (parent_part, base_part) = split_path(&path_str);

        let source_tag = match candidate.source {
            ResultSource::Link => ("link", theme.selected_indicator),
            ResultSource::History => ("hist", theme.title_key),
            ResultSource::Filesystem => ("scan", theme.match_count),
        };

        let indicator = if *is_selected { "  ▸  " } else { "     " };
        let indicator_style = if *is_selected {
            Style::default().fg(theme.selected_indicator)
        } else {
            Style::default()
        };

        let bg = if *is_selected {
            Style::default().bg(theme.selected_bg)
        } else {
            Style::default()
        };

        let invalid_prefix = if candidate.valid { "" } else { "✗ " };
        let tag_text = format!(" [{tag}]", tag = source_tag.0);

        let display_parent;
        let display_base = base_part;
        let available = term_width.saturating_sub(INDICATOR_WIDTH + invalid_prefix.len() + tag_text.len() + 2);

        if parent_part.len() + base_part.len() > available {
            let base_budget = available.min(base_part.len());
            let parent_budget = available.saturating_sub(base_budget);
            if parent_budget > 4 {
                // Truncate from the left, respecting char boundaries
                let target_len = parent_budget - 1; // -1 for '…'
                let start = parent_part
                    .char_indices()
                    .rev()
                    .find_map(|(i, _)| {
                        if parent_part.len() - i <= target_len { Some(i) } else { None }
                    })
                    .unwrap_or(0);
                display_parent = format!("…{}", &parent_part[start..]);
            } else {
                display_parent = String::new();
            }
        } else {
            display_parent = parent_part.to_string();
        }

        let content_len = INDICATOR_WIDTH + invalid_prefix.len()
            + display_parent.len() + display_base.len() + tag_text.len();
        let padding = term_width.saturating_sub(content_len);

        let mut spans = vec![Span::styled(indicator, indicator_style)];
        if !invalid_prefix.is_empty() {
            spans.push(Span::styled(invalid_prefix, Style::default().fg(theme.invalid_mark)));
        }
        spans.push(Span::styled(display_parent, bg.fg(theme.path_parent)));
        spans.push(Span::styled(display_base.to_string(), bg.fg(theme.path_basename).add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(" ".repeat(padding.max(1)), bg));
        spans.push(Span::styled(tag_text, bg.fg(source_tag.1)));

        frame.render_widget(Paragraph::new(Line::from(spans)), rows[row_idx]);
    }
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let line = if app.filter_mode {
        Line::from(vec![
            Span::styled("  ⏎", Style::default().fg(theme.footer_key)),
            Span::styled(" jump   ", Style::default().fg(theme.footer_desc)),
            Span::styled("esc", Style::default().fg(theme.footer_key)),
            Span::styled(" clear filter", Style::default().fg(theme.footer_desc)),
        ])
    } else {
        Line::from(vec![
            Span::styled("  ↑↓", Style::default().fg(theme.footer_key)),
            Span::styled(" select   ", Style::default().fg(theme.footer_desc)),
            Span::styled("⏎", Style::default().fg(theme.footer_key)),
            Span::styled(" jump   ", Style::default().fg(theme.footer_desc)),
            Span::styled("/", Style::default().fg(theme.footer_key)),
            Span::styled(" filter   ", Style::default().fg(theme.footer_desc)),
            Span::styled("esc", Style::default().fg(theme.footer_key)),
            Span::styled(" cancel", Style::default().fg(theme.footer_desc)),
        ])
    };

    frame.render_widget(Paragraph::new(line), area);
}

fn split_path(path_str: &str) -> (&str, &str) {
    match path_str.rfind('/') {
        Some(pos) => (&path_str[..=pos], &path_str[pos + 1..]),
        None => ("", path_str),
    }
}
