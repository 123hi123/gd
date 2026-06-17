use ratatui::style::Color;

pub struct Theme {
    pub title_gd: Color,
    pub title_key: Color,
    pub match_count: Color,
    pub selected_indicator: Color,
    pub selected_bg: Color,
    /// Basename on the highlighted row — the brightest text in the picker.
    pub path_basename: Color,
    /// Basename on the other (dimmed) rows — readable but receding, so the
    /// selected row alone pops out (Treisman: one exclusive bright singleton).
    pub path_basename_dim: Color,
    /// Parent path on dimmed rows.
    pub path_parent: Color,
    /// Parent path on the highlighted row — lifted so the *whole* selected
    /// path reads as one bright block, not just the basename.
    pub path_parent_bright: Color,
    pub footer_key: Color,
    pub footer_desc: Color,
    pub filter_prompt: Color,
    pub invalid_mark: Color,
}

impl Theme {
    pub fn default_theme() -> Self {
        Self {
            title_gd: Color::Rgb(203, 166, 247),     // mauve
            title_key: Color::Rgb(249, 226, 175),     // yellow
            match_count: Color::Rgb(108, 112, 134),   // overlay0
            selected_indicator: Color::Rgb(166, 227, 161), // green
            // Lavender-tinted bar (toward mauve) — hue + luminance is the
            // strongest pop-out cue; a neutral-grey bar reads only as "less
            // black" in this already-grey UI. The green gutter is complementary
            // to this purple, so it stands out harder for free.
            selected_bg: Color::Rgb(84, 74, 116),
            path_basename: Color::Rgb(205, 214, 244), // text
            path_basename_dim: Color::Rgb(166, 173, 200), // subtext0
            path_parent: Color::Rgb(108, 112, 134),   // overlay0 — dimmed, recedes
            path_parent_bright: Color::Rgb(186, 194, 222), // subtext1
            footer_key: Color::Rgb(203, 166, 247),    // mauve
            footer_desc: Color::Rgb(108, 112, 134),   // overlay0
            filter_prompt: Color::Rgb(137, 220, 235), // sky
            invalid_mark: Color::Rgb(243, 139, 168),  // red
        }
    }

    pub fn fallback() -> Self {
        Self {
            title_gd: Color::Magenta,
            title_key: Color::Yellow,
            match_count: Color::DarkGray,
            selected_indicator: Color::Green,
            // 16-colour terminals get a hue'd (blue) bar where supported; the
            // ▌ gutter is the load-bearing selection marker when it barely shows.
            selected_bg: Color::Blue,
            path_basename: Color::White,
            path_basename_dim: Color::Gray,
            path_parent: Color::DarkGray,
            path_parent_bright: Color::Gray,
            footer_key: Color::Magenta,
            footer_desc: Color::DarkGray,
            filter_prompt: Color::Cyan,
            invalid_mark: Color::Red,
        }
    }
}
