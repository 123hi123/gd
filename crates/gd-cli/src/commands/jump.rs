use anyhow::Result;
use gd_core::db::{KeyStore, ResultSource, SearchResult};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::path::{Path, PathBuf};
use std::process;

pub fn run(store: &mut KeyStore, query: &str) -> Result<()> {
    if let Some(path) = store.get_link(query) {
        if path.exists() {
            store.record_selection(&path);
            store.save()?;
            println!("{}", path.display());
            return Ok(());
        }
    }

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

    let mut results = gather_results(store, query);

    if results.is_empty() {
        results = fuzzy_fallback(store, query);
    }

    if results.is_empty() {
        eprintln!("gd: no matches for '{query}'.");
        process::exit(3);
    }

    dedup_results(&mut results);
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

        match crate::tui::pick(query, &candidates)? {
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

fn gather_results(store: &KeyStore, query: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
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

    results
}

fn fuzzy_fallback(store: &KeyStore, query: &str) -> Vec<SearchResult> {
    let pattern = Pattern::new(query, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy);
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

    for (path, entry) in store.all_history() {
        if !path.exists() {
            continue;
        }
        let basename = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let matched: Vec<(&str, u32)> =
            pattern.match_list(std::iter::once(basename), &mut matcher);
        if let Some(&(_, score)) = matched.first() {
            if score > 0 {
                let decay = gd_core::frecency::decay_factor(now.saturating_sub(entry.last_access));
                const SELECTED_TIER: f64 = 100_000.0;
                let base = if entry.selections > 0 {
                    SELECTED_TIER + (entry.selections as f64 * 10.0 + entry.visits as f64) * decay
                } else {
                    entry.visits as f64 * decay
                };
                results.push(SearchResult {
                    path: path.clone(),
                    score: base * 0.5 + f64::from(score),
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
