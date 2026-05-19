use ratatui::style::Color;

pub struct Theme {
    pub title_gd: Color,
    pub title_key: Color,
    pub match_count: Color,
    pub selected_indicator: Color,
    pub selected_bg: Color,
    pub path_basename: Color,
    pub path_parent: Color,
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
            selected_bg: Color::Rgb(49, 50, 68),      // surface0
            path_basename: Color::Rgb(205, 214, 244), // text
            path_parent: Color::Rgb(127, 132, 156),   // overlay1
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
            selected_bg: Color::Reset,
            path_basename: Color::White,
            path_parent: Color::DarkGray,
            footer_key: Color::Magenta,
            footer_desc: Color::DarkGray,
            filter_prompt: Color::Cyan,
            invalid_mark: Color::Red,
        }
    }
}
