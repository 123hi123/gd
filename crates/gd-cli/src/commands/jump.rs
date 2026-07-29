use anyhow::Result;
use gd_core::db::{KeyStore, ResultSource, SearchResult};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::{Path, PathBuf};
use std::process;

/// Weight applied to the nucleo match score when scoring *history* dirs in the
/// fuzzy fallbacks. Every dir surfaced here is already a loose match, so usage
/// history decides the order — match quality is only a tiebreaker (see "Search
/// priority" in CLAUDE.md: selected/visited history is ranked by count). nucleo
/// totals run ~0..500; this keeps their contribution below a single-visit
/// frecency step (`0.5 * min_decay 0.25 = 0.125`), so a more-used dir can never
/// be displaced by a slightly better-matching but less-used one.
const HISTORY_MATCH_TIEBREAK: f64 = 0.0001;

/// Frecency floor for a history dir in the fuzzy / typo fallbacks, encoding the
/// ranking tiers from CLAUDE.md: a *selected* dir lands in the top band (ranked
/// by selection count), a *visited-but-never-selected* dir lands in a middle
/// band that still outranks every filesystem/index match (which top out around
/// the low tens before boosts). Match quality is added separately as a small
/// tiebreaker, so it never reorders these bands.
#[allow(clippy::cast_precision_loss)]
fn history_base(selections: u64, visits: u64, decay: f64) -> f64 {
    const SELECTED_TIER: f64 = 100_000.0;
    const VISITED_TIER: f64 = 1_000.0;
    if selections > 0 {
        SELECTED_TIER + (selections as f64 * 10.0 + visits as f64) * decay
    } else {
        VISITED_TIER + visits as f64 * decay
    }
}

pub fn run(store: &mut KeyStore, query: &str) -> Result<()> {
    // A query containing '/' is a filesystem path, not a basename search (gd
    // only ever matches basenames). Resolve it directly, *before* splitting into
    // keywords — otherwise a path with a space (e.g. "qwen 協議/foo") splits into
    // >1 keyword, skips this branch, and falls through to a fuzzy search that can
    // never match a full path.
    if query.contains('/') {
        let path = PathBuf::from(query);
        let resolved = if path.is_absolute() {
            path
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        };
        if resolved.is_dir() {
            let target = std::fs::canonicalize(&resolved).unwrap_or(resolved);
            store.record_selection(&target);
            store.save()?;
            println!("{}", target.display());
        } else {
            eprintln!("gd: not a directory: {query}");
            process::exit(1);
        }
        return Ok(());
    }

    let keywords: Vec<&str> = query.split_whitespace().collect();

    if keywords.len() <= 1 {
        if let Some(path) = store.get_link(query) {
            if path.exists() {
                store.record_selection(&path);
                store.save()?;
                println!("{}", path.display());
                return Ok(());
            }
        }

        if let Ok(cwd) = std::env::current_dir() {
            let local = cwd.join(query);
            if local.is_dir() {
                let target = std::fs::canonicalize(&local).unwrap_or(local);
                store.record_selection(&target);
                store.save()?;
                println!("{}", target.display());
                return Ok(());
            }
        }
    }

    let mut results = gather_results(store, &keywords);

    if results.is_empty() {
        results = fuzzy_fallback(store, &keywords);
    }

    if results.is_empty() {
        results = typo_fallback(store, &keywords);
    }

    if results.is_empty() {
        eprintln!("gd: no matches for '{query}'.");
        process::exit(3);
    }

    dedup_results(&mut results);
    apply_cwd_proximity(&mut results, keywords.len() <= 1);
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    let selected = if is_interactive() {
        let candidates = results
            .iter()
            .map(|r| gd_core::db::Candidate {
                path: r.path.clone(),
                score: r.score,
                source: r.source.clone(),
            })
            .collect::<Vec<_>>();

        let mode = crate::tui::LayoutMode::from_setting(store.get_setting("layout").as_deref());
        let lang = crate::i18n::Lang::resolve(store.get_setting("language").as_deref());
        match crate::tui::pick(query, &candidates, mode, lang)? {
            Some(path) => path,
            None => process::exit(130),
        }
    } else {
        results[0].path.clone()
    };

    store.record_selection(&selected);
    store.save()?;

    println!("{}", selected.display());
    Ok(())
}

fn gather_results(store: &KeyStore, keywords: &[&str]) -> Vec<SearchResult> {
    let mut results = Vec::new();

    if keywords.len() <= 1 {
        let query = keywords.first().copied().unwrap_or("");
        let query_lower = query.to_lowercase();

        if let Some(path) = store.get_link(query) {
            if path.exists() {
                results.push(SearchResult {
                    path,
                    score: f64::MAX,
                    source: ResultSource::Link,
                });
            }
        }

        let mut history = store.search_history(query);
        for r in &mut history {
            let boost = store.boost_for(&r.path);
            r.score *= boost;
        }
        results.extend(history);

        let home = dirs::home_dir();
        let index_paths = if store.has_index() {
            store.search_index(query)
        } else {
            scan_fd_fallback(query)
        };

        for path in index_paths {
            let basename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();

            let mut rank: f64 = if basename == query_lower {
                5000.0
            } else if basename.starts_with(&query_lower) {
                10.0
            } else {
                0.1
            };

            if let Some(ref h) = home {
                if let Ok(rel) = path.strip_prefix(h) {
                    let depth = rel.components().count();
                    if depth == 1 {
                        rank += 100.0;
                    } else if depth <= 3 {
                        #[allow(clippy::cast_precision_loss)]
                        {
                            rank += 10.0 / depth as f64;
                        }
                    }
                }
            }

            rank *= store.boost_for(&path);

            results.push(SearchResult {
                path,
                score: rank,
                source: ResultSource::Filesystem,
            });
        }
    } else {
        let last_kw = keywords.last().unwrap().to_lowercase();

        let mut history = store.search_history_multi(keywords);
        for r in &mut history {
            let boost = store.boost_for(&r.path);
            r.score *= boost;
        }
        results.extend(history);

        let home = dirs::home_dir();
        let index_paths = if store.has_index() {
            store.search_index_multi(keywords)
        } else {
            Vec::new()
        };

        for path in index_paths {
            let basename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_lowercase();

            let mut rank: f64 = if basename == last_kw {
                5000.0
            } else if basename.starts_with(&last_kw) {
                10.0
            } else {
                0.1
            };

            if let Some(ref h) = home {
                if let Ok(rel) = path.strip_prefix(h) {
                    let depth = rel.components().count();
                    if depth == 1 {
                        rank += 100.0;
                    } else if depth <= 3 {
                        #[allow(clippy::cast_precision_loss)]
                        {
                            rank += 10.0 / depth as f64;
                        }
                    }
                }
            }

            rank *= store.boost_for(&path);

            results.push(SearchResult {
                path,
                score: rank,
                source: ResultSource::Filesystem,
            });
        }
    }

    results
}

fn fuzzy_fallback(store: &KeyStore, keywords: &[&str]) -> Vec<SearchResult> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let now = gd_core::frecency::now_secs();
    let home = dirs::home_dir();
    let mut results = Vec::new();

    let boosts = store.list_boosts();
    let boost_for = |path: &Path| -> f64 {
        for (boosted_dir, multiplier) in &boosts {
            if path.starts_with(boosted_dir) {
                return *multiplier;
            }
        }
        1.0
    };

    if keywords.len() <= 1 {
        let query = keywords.first().copied().unwrap_or("");
        let pattern =
            Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);

        for (path, entry) in store.all_history() {
            if !path.exists() {
                continue;
            }
            let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let matched: Vec<(&str, u32)> =
                pattern.match_list(std::iter::once(basename), &mut matcher);
            if let Some(&(_, score)) = matched.first() {
                if score > 0 {
                    let decay =
                        gd_core::frecency::decay_factor(now.saturating_sub(entry.last_access));
                    let base = history_base(entry.selections, entry.visits, decay);
                    results.push(SearchResult {
                        path: path.clone(),
                        score: base * 0.5 + f64::from(score) * HISTORY_MATCH_TIEBREAK,
                        source: ResultSource::History,
                    });
                }
            }
        }

        if store.has_index() {
            for (path_str, basename) in store.all_index_entries() {
                let matched: Vec<(&str, u32)> =
                    pattern.match_list(std::iter::once(basename.as_str()), &mut matcher);
                if let Some(&(_, score)) = matched.first() {
                    if score > 0 {
                        let path = PathBuf::from(&path_str);
                        let mut rank = f64::from(score) * 0.01;
                        if let Some(ref h) = home {
                            if let Ok(rel) = path.strip_prefix(h) {
                                let depth = rel.components().count();
                                if depth == 1 {
                                    rank += 10.0;
                                } else if depth <= 3 {
                                    #[allow(clippy::cast_precision_loss)]
                                    {
                                        rank += 1.0 / depth as f64;
                                    }
                                }
                            }
                        }
                        rank *= boost_for(&path);
                        results.push(SearchResult {
                            path,
                            score: rank,
                            source: ResultSource::Filesystem,
                        });
                    }
                }
            }
        }
    } else {
        let patterns: Vec<Pattern> = keywords
            .iter()
            .map(|kw| {
                Pattern::new(kw, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy)
            })
            .collect();

        for (path, entry) in store.all_history() {
            if !path.exists() {
                continue;
            }
            let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if let Some((total_score, matched)) =
                fuzzy_match_words(keywords, &patterns, basename, &mut matcher)
            {
                let decay =
                    gd_core::frecency::decay_factor(now.saturating_sub(entry.last_access));
                let base = history_base(entry.selections, entry.visits, decay);
                // soft-AND: partial matches (matched < keywords) are allowed from
                // history only, demoted by (matched / total)^2 so a full match always
                // outranks a partial one.
                #[allow(clippy::cast_precision_loss)]
                let penalty = {
                    let ratio = matched as f64 / keywords.len() as f64;
                    ratio * ratio
                };
                results.push(SearchResult {
                    path: path.clone(),
                    score: (base * 0.5 + f64::from(total_score) * HISTORY_MATCH_TIEBREAK) * penalty,
                    source: ResultSource::History,
                });
            }
        }

        if store.has_index() {
            for (path_str, basename) in store.all_index_entries() {
                if let Some((total_score, matched)) =
                    fuzzy_match_words(keywords, &patterns, &basename, &mut matcher)
                {
                    // index has ~246k entries vs ~73 in history: surfacing partial
                    // matches here floods the picker (e.g. "open" alone hits 745 dirs),
                    // so require all keywords to match for index results.
                    if matched < keywords.len() {
                        continue;
                    }
                    let path = PathBuf::from(&path_str);
                    let mut rank = f64::from(total_score) * 0.01;
                    if let Some(ref h) = home {
                        if let Ok(rel) = path.strip_prefix(h) {
                            let depth = rel.components().count();
                            if depth == 1 {
                                rank += 10.0;
                            } else if depth <= 3 {
                                #[allow(clippy::cast_precision_loss)]
                                {
                                    rank += 1.0 / depth as f64;
                                }
                            }
                        }
                    }
                    rank *= boost_for(&path);
                    results.push(SearchResult {
                        path,
                        score: rank,
                        source: ResultSource::Filesystem,
                    });
                }
            }
        }
    }

    results
}

/// Match each keyword against the words of `basename` (split on `-`, `_`, `.`, space).
///
/// Returns `(summed_score, matched_count)`, or `None` if no keyword matched at all.
/// A keyword that matches neither fuzzily nor within its edit-distance threshold is
/// simply skipped rather than failing the whole basename (soft-AND) — the caller
/// decides whether a partial match (`matched_count < keywords.len()`) is acceptable.
fn fuzzy_match_words(
    keywords: &[&str],
    patterns: &[Pattern],
    basename: &str,
    matcher: &mut Matcher,
) -> Option<(u32, usize)> {
    let words: Vec<&str> = basename
        .split(|c: char| c == '-' || c == '_' || c == '.' || c == ' ')
        .filter(|s| !s.is_empty())
        .collect();
    let mut total = 0u32;
    let mut matched = 0usize;
    for (kw, pattern) in keywords.iter().zip(patterns.iter()) {
        let mut best_fuzzy = 0u32;
        let mut best_edit = usize::MAX;
        for word in &words {
            let hits: Vec<(&str, u32)> =
                pattern.match_list(std::iter::once(*word), matcher);
            if let Some(&(_, s)) = hits.first() {
                best_fuzzy = best_fuzzy.max(s);
            }
            best_edit = best_edit.min(damerau_levenshtein(kw, word));
        }
        let max_dist = edit_distance_threshold(kw);
        if best_fuzzy > 0 {
            total += best_fuzzy;
            matched += 1;
        } else if best_edit <= max_dist {
            total += 40u32.saturating_sub(best_edit as u32 * 15);
            matched += 1;
        }
    }
    if matched == 0 {
        None
    } else {
        Some((total, matched))
    }
}

fn edit_distance_threshold(keyword: &str) -> usize {
    match keyword.chars().count() {
        0..=2 => 0,
        3..=4 => 1,
        _ => 2,
    }
}

fn damerau_levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let (len_a, len_b) = (a.len(), b.len());
    if len_a == 0 {
        return len_b;
    }
    if len_b == 0 {
        return len_a;
    }
    let mut d = vec![vec![0usize; len_b + 1]; len_a + 1];
    for i in 0..=len_a {
        d[i][0] = i;
    }
    for j in 0..=len_b {
        d[0][j] = j;
    }
    for i in 1..=len_a {
        for j in 1..=len_b {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1
                && j > 1
                && a[i - 1] == b[j - 2]
                && a[i - 2] == b[j - 1]
            {
                d[i][j] = d[i][j].min(d[i - 2][j - 2] + 1);
            }
        }
    }
    d[len_a][len_b]
}

fn typo_fallback(store: &KeyStore, keywords: &[&str]) -> Vec<SearchResult> {
    let now = gd_core::frecency::now_secs();
    let home = dirs::home_dir();
    let mut results = Vec::new();

    let boosts = store.list_boosts();
    let boost_for = |path: &Path| -> f64 {
        for (boosted_dir, multiplier) in &boosts {
            if path.starts_with(boosted_dir) {
                return *multiplier;
            }
        }
        1.0
    };

    let query_joined = keywords.join("-");

    for (path, entry) in store.all_history() {
        if !path.exists() {
            continue;
        }
        let basename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let dist = damerau_levenshtein(&query_joined, basename);
        let threshold = edit_distance_threshold(&query_joined);
        if dist <= threshold {
            let max_len = query_joined.len().max(basename.len());
            let sim = 1.0 - (dist as f64 / max_len as f64);
            let decay = gd_core::frecency::decay_factor(now.saturating_sub(entry.last_access));
            let base = history_base(entry.selections, entry.visits, decay);
            results.push(SearchResult {
                path: path.clone(),
                score: base * 0.3 + sim * HISTORY_MATCH_TIEBREAK,
                source: ResultSource::History,
            });
        }
    }

    if store.has_index() {
        for (path_str, basename) in store.all_index_entries() {
            let dist = damerau_levenshtein(&query_joined, &basename);
            let threshold = edit_distance_threshold(&query_joined);
            if dist <= threshold {
                let max_len = query_joined.len().max(basename.len());
                let sim = 1.0 - (dist as f64 / max_len as f64);
                let path = PathBuf::from(&path_str);
                let mut rank = sim * 10.0;
                if let Some(ref h) = home {
                    if let Ok(rel) = path.strip_prefix(h) {
                        let depth = rel.components().count();
                        if depth == 1 {
                            rank += 5.0;
                        } else if depth <= 3 {
                            #[allow(clippy::cast_precision_loss)]
                            {
                                rank += 1.0 / depth as f64;
                            }
                        }
                    }
                }
                rank *= boost_for(&path);
                results.push(SearchResult {
                    path,
                    score: rank,
                    source: ResultSource::Filesystem,
                });
            }
        }
    }

    results
}

fn scan_fd_fallback(query: &str) -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    let output = std::process::Command::new("fd")
        .args([
            "--type", "d",
            "--max-depth", "6",
            "--hidden", "--no-ignore",
            "--exclude", ".git",
            "--exclude", "node_modules",
            "--exclude", ".cache",
            "--exclude", "target",
            "--max-results", "20",
            query,
        ])
        .arg(&home)
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let path = PathBuf::from(line.trim());
            if path.is_dir() { Some(path) } else { None }
        })
        .collect()
}

fn dedup_results(results: &mut Vec<SearchResult>) {
    let mut seen = std::collections::HashSet::new();
    results.retain(|r| seen.insert(r.path.clone()));
}

/// Current-directory proximity boost (plus a freshly-created floor).
///
/// A result that lives *under* the directory the user is standing in is almost
/// always more relevant than an equally-named dir elsewhere ("I'm in this project
/// right now, so its `src` beats some other project's `src`"). Lift every strict
/// descendant of the cwd to ~1.5 selections — between a once-selected dir (100_010)
/// and a twice-selected one (100_020); see the SELECTED_TIER math in db.rs /
/// `history_base`. So a fresh cwd-descendant outranks a dir selected once elsewhere,
/// but an established habit (selected ≥2×) still wins.
///
/// Uses `.max` (a floor, not an add) so a cwd-descendant that already carries richer
/// history keeps its real, higher score and is never dragged down. No decay is
/// applied: "I am standing here right now" is itself the freshest possible signal.
///
/// **Freshly-created floor.** When the query is a *single keyword* (a weak,
/// "you-know-what-I-mean" signal) and a cwd-descendant was created/touched within
/// the last few minutes, lift it all the way to `FRESH_TIER` — above any realistic
/// selection history, below only an explicit `gd link`. This is the "`md foo`, then
/// immediately `gd f` to jump in" case: the just-made `foo` should win over some old
/// `f*` habit. The signal is *recency*, not query length, so it self-expires (an
/// hour later `foo` ranks by its real history again) and a multi-keyword — i.e. more
/// specific — query never triggers it. Cost is a single `stat()` on the handful of
/// cwd-descendants already in the result set.
fn apply_cwd_proximity(results: &mut [SearchResult], single_keyword: bool) {
    // 100_000 (SELECTED_TIER) + 15 == 1.5 selection steps of 10 each.
    const CWD_PROXIMITY: f64 = 100_015.0;
    // Above the whole selection tier (100_000 + selections*10*decay, realistically a
    // few thousand at most) yet below an explicit link (f64::MAX). "I made this
    // seconds ago and I'm standing in its parent" is the strongest signal short of an
    // alias.
    const FRESH_TIER: f64 = 200_000.0;
    // How recently a dir must have been created/touched to count as "fresh".
    const FRESH_WINDOW_SECS: u64 = 300; // 5 minutes

    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let cwd = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let now = gd_core::frecency::now_secs();
    for r in results.iter_mut() {
        if r.path != cwd && r.path.starts_with(&cwd) {
            r.score = r.score.max(CWD_PROXIMITY);
            if single_keyword {
                if let Some(age) = dir_age_secs(&r.path, now) {
                    if age <= FRESH_WINDOW_SECS {
                        r.score = r.score.max(FRESH_TIER);
                    }
                }
            }
        }
    }
}

/// Seconds elapsed since `path`'s directory mtime, or `None` if it can't be read
/// or its mtime is in the future (clock skew). A just-`mkdir`'d dir has mtime ≈ now,
/// so a small age is a cheap "freshly created" proxy without touching the DB schema.
fn dir_age_secs(path: &Path, now: u64) -> Option<u64> {
    let mtime = std::fs::metadata(path).ok()?.modified().ok()?;
    let mtime_secs = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    now.checked_sub(mtime_secs)
}

fn is_interactive() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(2) != 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}
