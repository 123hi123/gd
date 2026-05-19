use gd_core::db::{Candidate, ResultSource};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::PathBuf;

pub enum Action {
    Select(PathBuf),
    Cancel,
    Continue,
}

pub struct App {
    pub query: String,
    pub all_candidates: Vec<CandidateView>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub viewport_size: usize,
    pub filter_mode: bool,
    pub filter_query: String,
    matcher: Matcher,
}

pub struct CandidateView {
    pub path: PathBuf,
    pub valid: bool,
    pub source: ResultSource,
}

impl App {
    pub fn new(query: String, candidates: &[Candidate], viewport_size: usize) -> Self {
        let all_candidates: Vec<CandidateView> = candidates
            .iter()
            .map(|c| CandidateView {
                path: c.path.clone(),
                valid: c.path.exists(),
                source: c.source.clone(),
            })
            .collect();

        let filtered: Vec<usize> = (0..all_candidates.len()).collect();

        Self {
            query,
            all_candidates,
            filtered,
            selected: 0,
            scroll_offset: 0,
            viewport_size,
            filter_mode: false,
            filter_query: String::new(),
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    pub fn total_matches(&self) -> usize {
        self.filtered.len()
    }

    pub fn visible_candidates(&self) -> Vec<(bool, &CandidateView)> {
        self.filtered
            .iter()
            .skip(self.scroll_offset)
            .take(self.viewport_size)
            .enumerate()
            .map(|(viewport_idx, &orig_idx)| {
                let is_selected = self.scroll_offset + viewport_idx == self.selected;
                (is_selected, &self.all_candidates[orig_idx])
            })
            .collect()
    }

    pub fn move_up(&mut self) {
        let total = self.filtered.len();
        if total == 0 {
            return;
        }
        if self.selected == 0 {
            self.selected = total - 1;
            // Scroll to bottom
            self.scroll_offset = total.saturating_sub(self.viewport_size);
        } else {
            self.selected -= 1;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            }
        }
    }

    pub fn move_down(&mut self) {
        let total = self.filtered.len();
        if total == 0 {
            return;
        }
        self.selected = (self.selected + 1) % total;
        if self.selected == 0 {
            // Wrapped to top
            self.scroll_offset = 0;
        } else if self.selected >= self.scroll_offset + self.viewport_size {
            self.scroll_offset = self.selected + 1 - self.viewport_size;
        }
    }

    pub fn select_current(&self) -> Action {
        if let Some(&orig_idx) = self.filtered.get(self.selected) {
            let cv = &self.all_candidates[orig_idx];
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

        self.selected = 0;
        self.scroll_offset = 0;
    }
}
