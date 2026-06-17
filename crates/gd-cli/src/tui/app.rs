use gd_core::db::{Candidate, ResultSource};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::cell::Cell;
use std::path::PathBuf;

/// Columns the highlighted row's path scrolls per ←/→ press — roughly one
/// short path segment, so a couple of presses reveals a hidden ancestor.
const H_SCROLL_STEP: usize = 8;

pub enum Action {
    Select(PathBuf),
    Cancel,
    Continue,
}

/// How the picker arranges its candidates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LayoutMode {
    /// Inline at the prompt (fzf `--height --layout=reverse` style): the picker
    /// renders in a compact block right where you invoked `gd`, with the query
    /// echo and the best match on the *top* line — the row your eyes are already
    /// on — and the rest of the list flowing downward. Doesn't take over the
    /// screen; the surrounding output stays visible above it.
    #[default]
    Inline,
    /// Best match on the viewport's center line, the rest fanning out
    /// alternately above and below it. Keeps the eye on one spot. Full-screen.
    Centered,
    /// Classic top-down list: best match on top, scrolling downward. Full-screen.
    Traditional,
}

impl LayoutMode {
    /// Resolve the stored `layout` setting. Unset or unknown → the default
    /// (inline). `bottom` is an accepted alias for the earlier name of this mode.
    pub fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("centered") => Self::Centered,
            Some("traditional") => Self::Traditional,
            _ => Self::Inline,
        }
    }

    /// Whether this mode renders inline (anchored at the prompt) rather than
    /// taking over the screen with the alternate buffer.
    pub fn is_inline(self) -> bool {
        matches!(self, LayoutMode::Inline)
    }
}

pub struct App {
    pub query: String,
    pub all_candidates: Vec<CandidateView>,
    /// Candidate indices in rank order, best first. Drives filtering and the
    /// match count; `order` is the version actually drawn.
    pub filtered: Vec<usize>,
    /// Draw order, top to bottom. In centered mode this is `filtered` reordered
    /// center-out; in traditional mode it is `filtered` unchanged.
    pub order: Vec<usize>,
    /// Selection: an index into `order`.
    pub cursor: usize,
    /// Window start (index into `order`) for traditional scrolling. Unused in
    /// centered mode, where the selection is always pinned to the center line.
    pub scroll_offset: usize,
    pub viewport_size: usize,
    pub mode: LayoutMode,
    pub filter_mode: bool,
    pub filter_query: String,
    /// Horizontal scroll (in display columns) of the *highlighted* row's path,
    /// panning toward the start to reveal a hidden prefix. 0 = default view.
    /// Reset whenever the selection or filter changes.
    pub h_scroll: usize,
    /// Max useful `h_scroll` for the current selection, written by the renderer
    /// (which alone knows the available width) and read back here to clamp ←/→
    /// and to decide whether to show the scroll hint.
    pub sel_max_hscroll: Cell<usize>,
    matcher: Matcher,
}

pub struct CandidateView {
    pub path: PathBuf,
    pub valid: bool,
    pub source: ResultSource,
}

/// Reorder a rank-sorted list (best first) into a center-out spread: the best
/// match lands in the middle, the 2nd one row up, the 3rd one row down, the 4th
/// two rows up, and so on outward. Returns the new order plus the index of the
/// best match within it (the initial cursor / center line).
fn build_spread(filtered: &[usize]) -> (Vec<usize>, usize) {
    let mut ups = Vec::new(); // odd ranks (1, 3, 5 …) — drawn above center
    let mut downs = Vec::new(); // even ranks (2, 4, 6 …) — drawn below center
    for (rank, &idx) in filtered.iter().enumerate() {
        match rank {
            0 => {}
            r if r % 2 == 1 => ups.push(idx),
            _ => downs.push(idx),
        }
    }
    let center = ups.len();
    let mut spread = Vec::with_capacity(filtered.len());
    spread.extend(ups.into_iter().rev());
    if let Some(&best) = filtered.first() {
        spread.push(best);
    }
    spread.extend(downs);
    (spread, center)
}

impl App {
    pub fn new(
        query: String,
        candidates: &[Candidate],
        viewport_size: usize,
        mode: LayoutMode,
    ) -> Self {
        let all_candidates: Vec<CandidateView> = candidates
            .iter()
            .map(|c| CandidateView {
                path: c.path.clone(),
                valid: c.path.exists(),
                source: c.source.clone(),
            })
            .collect();

        let mut app = Self {
            query,
            filtered: (0..all_candidates.len()).collect(),
            all_candidates,
            order: Vec::new(),
            cursor: 0,
            scroll_offset: 0,
            viewport_size,
            mode,
            filter_mode: false,
            filter_query: String::new(),
            h_scroll: 0,
            sel_max_hscroll: Cell::new(0),
            matcher: Matcher::new(Config::DEFAULT),
        };
        app.rebuild_view();
        app
    }

    /// Recompute `order`/`cursor`/`scroll_offset` from `filtered` for the
    /// current mode. Called on construction and after every refilter.
    fn rebuild_view(&mut self) {
        match self.mode {
            LayoutMode::Centered => {
                let (order, center) = build_spread(&self.filtered);
                self.order = order;
                self.cursor = center;
            }
            // Inline shares traditional's top-down rank order (best first); it
            // only differs in the rendering surface (an inline viewport).
            LayoutMode::Traditional | LayoutMode::Inline => {
                self.order = self.filtered.clone();
                self.cursor = 0;
            }
        }
        self.scroll_offset = 0;
        self.h_scroll = 0;
    }

    /// Keep the cursor inside the visible window `[scroll_offset, +viewport_size)`
    /// by sliding the window the minimum amount. Used by the windowed modes.
    fn clamp_window(&mut self) {
        if self.cursor < self.scroll_offset {
            self.scroll_offset = self.cursor;
        } else if self.cursor >= self.scroll_offset + self.viewport_size {
            self.scroll_offset = self.cursor + 1 - self.viewport_size;
        }
    }

    /// Pan the highlighted row's path toward its start, revealing a hidden
    /// prefix. Clamped to the max the renderer reported for this selection.
    pub fn hscroll_left(&mut self) {
        self.h_scroll = (self.h_scroll + H_SCROLL_STEP).min(self.sel_max_hscroll.get());
    }

    /// Pan back toward the tail (basename); stops at the default view.
    pub fn hscroll_right(&mut self) {
        self.h_scroll = self.h_scroll.saturating_sub(H_SCROLL_STEP);
    }

    pub fn total_matches(&self) -> usize {
        self.filtered.len()
    }

    /// Rank (1-based position in the best-first list) of the highlighted row,
    /// for the "3/120" indicator. 0 when there are no matches.
    pub fn selected_rank(&self) -> usize {
        self.order
            .get(self.cursor)
            .and_then(|&idx| self.filtered.iter().position(|&f| f == idx))
            .map_or(0, |r| r + 1)
    }

    /// Number of non-empty candidate rows to draw (traditional mode sizes its
    /// list region to this so the footer sits right under the last entry).
    pub fn visible_count(&self) -> usize {
        self.order
            .len()
            .saturating_sub(self.scroll_offset)
            .min(self.viewport_size)
    }

    /// One entry per viewport row, top to bottom. `None` is an empty row.
    pub fn visible_rows(&self) -> Vec<Option<(bool, &CandidateView)>> {
        match self.mode {
            LayoutMode::Centered => self.rows_centered(),
            // Inline reuses the top-down traditional window — the only
            // difference from Traditional is the inline rendering surface.
            LayoutMode::Traditional | LayoutMode::Inline => self.rows_traditional(),
        }
    }

    /// Centered: the cursor sits on the middle line and the spread fans out
    /// around it; the ends are padded with blanks so it stays dead center.
    fn rows_centered(&self) -> Vec<Option<(bool, &CandidateView)>> {
        let center = self.viewport_size / 2;
        (0..self.viewport_size)
            .map(|row| {
                // order index = cursor + (row - center); rows above the center
                // map below the cursor and vice versa. Out of range → blank.
                let si = if row >= center {
                    self.cursor.checked_add(row - center)
                } else {
                    self.cursor.checked_sub(center - row)
                };
                match si {
                    Some(i) if i < self.order.len() => {
                        Some((i == self.cursor, &self.all_candidates[self.order[i]]))
                    }
                    _ => None,
                }
            })
            .collect()
    }

    /// Traditional: a contiguous window from `scroll_offset`, top-packed.
    fn rows_traditional(&self) -> Vec<Option<(bool, &CandidateView)>> {
        (0..self.viewport_size)
            .map(|row| {
                let i = self.scroll_offset + row;
                if i < self.order.len() {
                    Some((i == self.cursor, &self.all_candidates[self.order[i]]))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn move_up(&mut self) {
        self.h_scroll = 0;
        match self.mode {
            LayoutMode::Centered => {
                self.cursor = self.cursor.saturating_sub(1);
            }
            LayoutMode::Traditional => {
                let total = self.order.len();
                if total == 0 {
                    return;
                }
                if self.cursor == 0 {
                    self.cursor = total - 1;
                    self.scroll_offset = total.saturating_sub(self.viewport_size);
                } else {
                    self.cursor -= 1;
                    if self.cursor < self.scroll_offset {
                        self.scroll_offset = self.cursor;
                    }
                }
            }
            // ↑ moves toward a better rank; no wrap, so the best is a ceiling
            // you settle on at the top.
            LayoutMode::Inline => {
                self.cursor = self.cursor.saturating_sub(1);
                self.clamp_window();
            }
        }
    }

    pub fn move_down(&mut self) {
        self.h_scroll = 0;
        match self.mode {
            LayoutMode::Centered => {
                if self.cursor + 1 < self.order.len() {
                    self.cursor += 1;
                }
            }
            LayoutMode::Traditional => {
                let total = self.order.len();
                if total == 0 {
                    return;
                }
                self.cursor = (self.cursor + 1) % total;
                if self.cursor == 0 {
                    self.scroll_offset = 0;
                } else if self.cursor >= self.scroll_offset + self.viewport_size {
                    self.scroll_offset = self.cursor + 1 - self.viewport_size;
                }
            }
            // ↓ moves toward a worse rank; clamps at the last entry (no wrap).
            LayoutMode::Inline => {
                if self.cursor + 1 < self.order.len() {
                    self.cursor += 1;
                    self.clamp_window();
                }
            }
        }
    }

    pub fn select_current(&self) -> Action {
        if let Some(&orig) = self.order.get(self.cursor) {
            let cv = &self.all_candidates[orig];
            if cv.valid {
                return Action::Select(cv.path.clone());
            }
        }
        Action::Continue
    }

    pub fn enter_filter(&mut self) {
        self.filter_mode = true;
    }

    pub fn exit_filter(&mut self) {
        if self.filter_mode {
            self.filter_query.clear();
            self.filter_mode = false;
            self.refilter();
        }
    }

    pub fn filter_push(&mut self, ch: char) {
        self.filter_query.push(ch);
        self.refilter();
    }

    pub fn filter_pop(&mut self) {
        self.filter_query.pop();
        self.refilter();
    }

    pub fn filter_clear(&mut self) {
        self.filter_query.clear();
        self.refilter();
    }

    fn refilter(&mut self) {
        if self.filter_query.is_empty() {
            self.filtered = (0..self.all_candidates.len()).collect();
        } else {
            let pattern = Pattern::new(
                &self.filter_query,
                CaseMatching::Ignore,
                Normalization::Smart,
                AtomKind::Fuzzy,
            );

            self.filtered = self
                .all_candidates
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    let haystack = c.path.to_string_lossy();
                    let matches: Vec<(&str, u32)> = pattern
                        .match_list(std::iter::once(haystack.as_ref()), &mut self.matcher);
                    matches
                        .first()
                        .is_some_and(|(_text, score)| *score > 0)
                        || haystack
                            .to_lowercase()
                            .contains(&self.filter_query.to_lowercase())
                })
                .map(|(i, _)| i)
                .collect();
        }

        self.rebuild_view();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn candidates(n: usize) -> Vec<Candidate> {
        (0..n)
            .map(|i| Candidate {
                path: PathBuf::from(format!("/x/d{i}")),
                score: (n - i) as f64,
                source: ResultSource::History,
            })
            .collect()
    }

    fn paths(app: &App) -> Vec<Option<(bool, PathBuf)>> {
        app.visible_rows()
            .into_iter()
            .map(|slot| slot.map(|(sel, c)| (sel, c.path.clone())))
            .collect()
    }

    /// rank 0 sits at the center; the rest fan out 1 up, 2 down, 3 up, 4 down…
    #[test]
    fn spread_is_center_out() {
        assert_eq!(build_spread(&[0]), (vec![0], 0));
        assert_eq!(build_spread(&[0, 1, 2]), (vec![1, 0, 2], 1));
        assert_eq!(build_spread(&[0, 1, 2, 3, 4]), (vec![3, 1, 0, 2, 4], 2));
        assert_eq!(
            build_spread(&[0, 1, 2, 3, 4, 5]),
            (vec![5, 3, 1, 0, 2, 4], 3)
        );
    }

    /// Centered: the best match opens highlighted on the center line, with the
    /// 2nd above it and the 3rd below it.
    #[test]
    fn centered_best_match_opens_in_middle() {
        let app = App::new("q".into(), &candidates(5), 5, LayoutMode::Centered);
        let rows = paths(&app);
        assert_eq!(rows[2], Some((true, PathBuf::from("/x/d0")))); // center = best
        assert_eq!(rows[1], Some((false, PathBuf::from("/x/d1")))); // one up = 2nd
        assert_eq!(rows[3], Some((false, PathBuf::from("/x/d2")))); // one down = 3rd
        assert_eq!(rows[0], Some((false, PathBuf::from("/x/d3")))); // two up = 4th
        assert_eq!(rows[4], Some((false, PathBuf::from("/x/d4")))); // two down = 5th
    }

    /// Centered: a short list keeps the highlight centered, padding the ends.
    #[test]
    fn centered_short_list_pads_to_center() {
        let app = App::new("q".into(), &candidates(1), 5, LayoutMode::Centered);
        let rows = paths(&app);
        assert!(rows[0].is_none() && rows[1].is_none());
        assert_eq!(rows[2], Some((true, PathBuf::from("/x/d0")))); // lone match centered
        assert!(rows[3].is_none() && rows[4].is_none());
    }

    #[test]
    fn centered_cursor_clamps_at_both_ends() {
        let mut app = App::new("q".into(), &candidates(3), 5, LayoutMode::Centered);
        for _ in 0..5 {
            app.move_up();
        }
        assert_eq!(app.cursor, 0);
        for _ in 0..10 {
            app.move_down();
        }
        assert_eq!(app.cursor, app.order.len() - 1);
    }

    /// Traditional: best match on top, list flowing downward.
    #[test]
    fn traditional_is_top_packed() {
        let app = App::new("q".into(), &candidates(3), 5, LayoutMode::Traditional);
        let rows = paths(&app);
        assert_eq!(rows[0], Some((true, PathBuf::from("/x/d0")))); // best on top, selected
        assert_eq!(rows[1], Some((false, PathBuf::from("/x/d1"))));
        assert_eq!(rows[2], Some((false, PathBuf::from("/x/d2"))));
        assert!(rows[3].is_none()); // blanks below
        assert_eq!(app.visible_count(), 3);
    }

    /// Traditional: down wraps from the last entry back to the top.
    #[test]
    fn traditional_down_wraps() {
        let mut app = App::new("q".into(), &candidates(3), 5, LayoutMode::Traditional);
        app.move_up(); // from top, wraps to last
        assert_eq!(app.cursor, 2);
        app.move_down(); // wraps back to top
        assert_eq!(app.cursor, 0);
    }

    /// Inline: best match on the *first* row (where your cursor already is), the
    /// rest flowing downward; blanks pad the bottom — the classic top-down
    /// window rendered in a compact inline block.
    #[test]
    fn inline_best_match_on_top() {
        let app = App::new("q".into(), &candidates(3), 5, LayoutMode::Inline);
        let rows = paths(&app);
        assert_eq!(rows[0], Some((true, PathBuf::from("/x/d0")))); // best, selected, top
        assert_eq!(rows[1], Some((false, PathBuf::from("/x/d1"))));
        assert_eq!(rows[2], Some((false, PathBuf::from("/x/d2"))));
        assert!(rows[3].is_none() && rows[4].is_none(), "blanks pad the bottom");
    }

    /// Inline: ↓ walks toward worse ranks, ↑ back toward the best; both clamp
    /// (no wrap), so the selection never jumps across the whole list.
    #[test]
    fn inline_moves_clamp_without_wrap() {
        let mut app = App::new("q".into(), &candidates(3), 5, LayoutMode::Inline);
        assert_eq!(app.cursor, 0); // opens on the best
        app.move_up(); // already best → clamps
        assert_eq!(app.cursor, 0);
        app.move_down(); // → rank 1
        assert_eq!(app.cursor, 1);
        app.move_down(); // → rank 2 (worst)
        assert_eq!(app.cursor, 2);
        app.move_down(); // clamps at the worst — no wrap
        assert_eq!(app.cursor, 2);
        app.move_up(); // back toward the best
        assert_eq!(app.cursor, 1);
    }
}
