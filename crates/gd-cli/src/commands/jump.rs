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

    // 端出去之前驗證前段結果的存在性,死路徑順手從 DB 退場(lazy 修正)。
    prune_dead_top(store, &mut results, 50);

    if results.is_empty() {
        eprintln!("gd: no matches for '{query}'.");
        process::exit(3);
    }

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

    // prune 只驗前段;深處撈出來的、或 TUI 停留期間被刪掉的(競態)在
    // 這裡把關:退場 + 明確報錯,而不是讓 shell cd 去撞牆。
    if !selected.exists() {
        store.retire_missing(&selected);
        eprintln!("gd: directory no longer exists: {}", selected.display());
        process::exit(1);
    }

    store.record_selection(&selected);
    store.save()?;

    println!("{}", selected.display());
    Ok(())
}

/// 驗證存在性時最多 stat 幾次。死列很多的時候(實測一次查詢可以撞到近 4000
/// 條死路徑)光是湊滿 `k` 個活結果就要 stat 幾千次,這在互動熱路徑上太貴。
/// 上限到了就停止驗證,剩下的一律保留 —— 由使用者選取後的最終存在性檢查
/// (run() 裡的 `!selected.exists()`)把關,真正選到死路徑仍然不會 cd 去撞牆。
const MAX_PRUNE_STATS: usize = 300;

/// 只驗證排序後前 `k` 名的存在性 — stat 成本只花在「真的會端給使用者」的
/// 結果上,絕不掃整個 DB。排在 k 名之後、或超出 `MAX_PRUNE_STATS` 的不驗,
/// 由選取時的最終檢查把關。
///
/// 驗到的死路徑先收集起來,迴圈結束後用 `retire_missing_batch` **一次交易**
/// 寫回:逐筆 `retire_missing` 各自成交易,撞上 daemon 掃描的寫鎖時每一筆
/// 都要吃一次 busy_timeout。
fn prune_dead_top(store: &KeyStore, results: &mut Vec<SearchResult>, k: usize) {
    let mut live = 0usize;
    let mut stats = 0usize;
    let mut dead: Vec<PathBuf> = Vec::new();

    results.retain(|r| {
        if live >= k || stats >= MAX_PRUNE_STATS {
            return true;
        }
        stats += 1;
        if r.path.exists() {
            live += 1;
            true
        } else {
            dead.push(r.path.clone());
            false
        }
    });

    if !dead.is_empty() {
        store.retire_missing_batch(&dead);
    }
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

/// fallback 掃索引時最多留下幾筆。索引有 ~37 萬列,而 picker 最多也只看得到
/// 前面幾十筆,把每個鬆散命中都具現成 Vec 是純浪費(實測打錯字的查詢 RSS
/// 衝到 80MB、耗時 0.25s,全查不到更是 0.5s)。
const FALLBACK_INDEX_LIMIT: usize = 200;

/// 第二階段取回 path 的方式切換點:候選數 ≤ 這個值就逐筆 rowid 主鍵探測,
/// 超過就改成循序掃一次索引列。
///
/// 兩條路的結果完全一樣,只是成本曲線不同:主鍵探測是 O(候選數 × log n) 的
/// 隨機 I/O,循序掃是固定成本(實測全撈 path 88 ms)。候選少時探測遠比全掃
/// 便宜;候選多到上萬筆時,隨機探測會反過來比循序掃貴。
///
/// **第一階段絕對不能只留前 N 名候選**(曾經這樣寫過,是錯的):最終分數是
/// `(basename 分數 + depth_bonus) * boost`,而 basename 分數的值域只有 0~2,
/// `depth_bonus` 卻是 +10、boost 是 ×5 —— 也就是說最終排序幾乎由 depth/boost
/// 決定,跟 basename 分數近乎無關。實測查 "cnfig" 時真正的第一名 `~/.config`
/// (basename 分數 1.05,連前 800 名都排不進,卻因為 depth +10 而總分 11.05)
/// 會被砍掉。所以第一階段留下全部命中,只是它們是 (f64, i64) 而不是路徑字串:
/// 26.7 萬筆全中也才 4 MB,而且實務上命中數是幾百到幾千。
const CANDIDATE_PROBE_LIMIT: usize = 20_000;

/// 串流版的「只留分數最高的 N 筆」。超過 2N 就排一次序砍回 N,攤提下來是
/// O(n) 且記憶體固定,不需要把整個索引具現化。
///
/// 分數與 payload 拆開存,是為了兩段式:排序鍵在第二階段才算得出來,而
/// payload 只是 `PathBuf`,不必為了排序先組出完整的 `SearchResult`。
struct TopN<T> {
    items: Vec<(f64, T)>,
    limit: usize,
}

impl<T> TopN<T> {
    fn new(limit: usize) -> Self {
        Self {
            items: Vec::with_capacity(limit * 2),
            limit,
        }
    }

    fn push(&mut self, score: f64, item: T) {
        self.items.push((score, item));
        if self.items.len() >= self.limit * 2 {
            self.trim();
        }
    }

    fn trim(&mut self) {
        self.items
            .sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        self.items.truncate(self.limit);
    }

    fn into_vec(mut self) -> Vec<(f64, T)> {
        self.trim();
        self.items
    }
}

/// fallback 的第二階段:把第一階段命中的 `(basename 分數, rowid)` 候選取回
/// path,補上 `depth_bonus` 與 boost 算出最終分數,再用 `TopN` 收斂到
/// `FALLBACK_INDEX_LIMIT` 名。
///
/// 第一階段的分數就是最終分數裡「只跟 basename 有關」的那一項(fuzzy 是
/// `nucleo * 0.01`,typo 是 `sim * 10.0`),這裡直接加 depth、乘 boost,算式
/// 與舊版逐列就地計算時逐位元相同;候選也沒有先砍過 —— 所以**留下來的 200 筆
/// 分數多重集與舊版完全相同**(對真實 DB 的 14 組查詢逐一比對過)。
///
/// 同分時留下哪幾筆也與舊版一致 —— 前提是推進 `TopN` 的順序要是 rowid 順序
/// (即舊版掃主表的順序),所以下面探測前會先把 rowid 排序,別把那行拿掉:
/// 第一階段是走索引掃的,順序是 (`basename_lower`, rowid),照那個順序推進去
/// 會換一批同分者留下來(實測 200 筆裡有 108~200 筆重疊,分數分佈相同)。
///
/// 唯一的行為差異:第一、二階段之間若有列被 daemon 刪掉,那一筆會靜默消失
/// (舊版單趟掃描沒有這個空窗)。那種列本來就是死的,無所謂。
fn resolve_candidates(
    store: &KeyStore,
    candidates: Vec<(f64, i64)>,
    home: Option<&PathBuf>,
    depth_top: f64,
    depth_ratio: f64,
    boost_for: &impl Fn(&Path) -> f64,
) -> Vec<SearchResult> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let mut top: TopN<PathBuf> = TopN::new(FALLBACK_INDEX_LIMIT);
    let score_of = |path: &Path, base: f64| -> f64 {
        (base + depth_bonus(path, home, depth_top, depth_ratio)) * boost_for(path)
    };

    if candidates.len() <= CANDIDATE_PROBE_LIMIT {
        // 候選少:逐筆 rowid 主鍵探測,完全不必再碰全表。
        let mut base: std::collections::HashMap<i64, f64> =
            std::collections::HashMap::with_capacity(candidates.len());
        let mut rowids: Vec<i64> = Vec::with_capacity(candidates.len());
        for (score, rowid) in candidates {
            base.insert(rowid, score);
            rowids.push(rowid);
        }
        // 依 rowid 排序後再探測,有兩個作用:(a) 探測順序沿著 B-tree 往前走,
        // 不是亂跳;(b) 更重要 —— 推進 `TopN` 的順序因此等同「掃主表」的順序,
        // 同分時留下的那幾筆就與舊版(以及下面的循序分支)完全一致。
        // 第一階段是走索引掃的,順序是 (basename_lower, rowid),沒有這行排序,
        // 同分的候選會換一批人留下來。
        rowids.sort_unstable();
        for (rowid, path) in store.paths_by_rowid(&rowids) {
            if let Some(&b) = base.get(&rowid) {
                let rank = score_of(&path, b);
                top.push(rank, path);
            }
        }
    } else {
        // 候選多到上萬筆:改成循序掃一次(成本等同舊版),用 rowid 對照。
        let base: std::collections::HashMap<i64, f64> =
            candidates.into_iter().map(|(s, id)| (id, s)).collect();
        store.for_each_index_path(|rowid, path_str| {
            if let Some(&b) = base.get(&rowid) {
                let path = PathBuf::from(path_str);
                let rank = score_of(&path, b);
                top.push(rank, path);
            }
        });
    }

    top.into_vec()
        .into_iter()
        .map(|(score, path)| SearchResult {
            path,
            score,
            source: ResultSource::Filesystem,
        })
        .collect()
}

/// 索引列的深度加分(離家目錄越淺越可能是使用者要的)。fuzzy / typo 兩個
/// fallback 都用同一套規則,抽出來免得三份 copy-paste 走鐘。
fn depth_bonus(path: &Path, home: Option<&PathBuf>, top: f64, ratio: f64) -> f64 {
    let Some(h) = home else { return 0.0 };
    let Ok(rel) = path.strip_prefix(h) else {
        return 0.0;
    };
    let depth = rel.components().count();
    if depth == 1 {
        top
    } else if depth <= 3 {
        #[allow(clippy::cast_precision_loss)]
        {
            ratio / depth as f64
        }
    } else {
        0.0
    }
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

        // 死路徑先收集,迴圈跑完一次交易退場(逐筆各自成交易太貴)。
        let mut dead: Vec<PathBuf> = Vec::new();
        for (path, entry) in store.all_history() {
            if !path.exists() {
                dead.push(path);
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
        if !dead.is_empty() {
            store.retire_missing_batch(&dead);
        }

        if store.has_index() {
            // 兩階段之間必須是同一個快照:rowid 會被 SQLite 回收,daemon 又
            // 隨時在 delete + insert(細節見 KeyStore::read_snapshot)。
            let _snap = store.read_snapshot();
            // 第一階段:串流掃索引的 basename(covering index,不碰 38 MB 的
            // 主表),命中的只記 (分數, rowid) —— 16 bytes,不建 PathBuf。
            let mut candidates: Vec<(f64, i64)> = Vec::new();
            store.for_each_index_basename(|rowid, basename| {
                let matched: Vec<(&str, u32)> =
                    pattern.match_list(std::iter::once(basename), &mut matcher);
                if let Some(&(_, score)) = matched.first() {
                    if score > 0 {
                        candidates.push((f64::from(score) * 0.01, rowid));
                    }
                }
            });
            // 第二階段:取回候選的 path,補 depth/boost,收斂到前 N 名。
            results.extend(resolve_candidates(
                store,
                candidates,
                home.as_ref(),
                10.0,
                1.0,
                &boost_for,
            ));
        }
    } else {
        let patterns: Vec<Pattern> = keywords
            .iter()
            .map(|kw| {
                Pattern::new(kw, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy)
            })
            .collect();

        // 同上:死路徑收集起來一次退場。
        let mut dead: Vec<PathBuf> = Vec::new();
        for (path, entry) in store.all_history() {
            if !path.exists() {
                dead.push(path);
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
        if !dead.is_empty() {
            store.retire_missing_batch(&dead);
        }

        if store.has_index() {
            // 同上,兩階段共用一個快照(見 KeyStore::read_snapshot)。
            let _snap = store.read_snapshot();
            // 同上,兩段式:先只看 basename 收候選,再取 path 算完整分數。
            let mut candidates: Vec<(f64, i64)> = Vec::new();
            store.for_each_index_basename(|rowid, basename| {
                if let Some((total_score, matched)) =
                    fuzzy_match_words(keywords, &patterns, basename, &mut matcher)
                {
                    // index has ~246k entries vs ~73 in history: surfacing partial
                    // matches here floods the picker (e.g. "open" alone hits 745 dirs),
                    // so require all keywords to match for index results.
                    if matched < keywords.len() {
                        return;
                    }
                    candidates.push((f64::from(total_score) * 0.01, rowid));
                }
            });
            results.extend(resolve_candidates(
                store,
                candidates,
                home.as_ref(),
                10.0,
                1.0,
                &boost_for,
            ));
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

/// `damerau_levenshtein` 的長度定義 —— 預篩必須跟 DP 用同一把尺,否則會誤篩。
///
/// 那個函式是先 `to_lowercase()` 再對 **char** 序列做 DP(不是位元組),所以
/// 這裡回傳的是「小寫化之後的字元數」。直接 `s.chars().count()` 不夠精確:
/// 少數字元小寫化會變長(例如 'İ' U+0130 → "i̇" 兩個 char),用原字串的字元數
/// 會低估長度,理論上可能把一個真的在門檻內的候選誤擋掉。
///
/// 不配置字串,逐字元累加小寫化後的長度。(`str::to_lowercase` 與逐字元版
/// 唯一的差異是希臘 sigma 的收尾特例,而那是 1→1,不影響長度。)
fn lowercase_char_len(s: &str) -> usize {
    s.chars().map(|c| c.to_lowercase().count()).sum()
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
    // 迴圈不變量,提到外面算一次(值與舊版逐圈重算完全相同)。
    let threshold = edit_distance_threshold(&query_joined);
    let query_len = lowercase_char_len(&query_joined);

    // 死路徑收集起來,迴圈跑完一次交易退場。
    let mut dead: Vec<PathBuf> = Vec::new();
    for (path, entry) in store.all_history() {
        if !path.exists() {
            dead.push(path);
            continue;
        }
        let basename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        // 長度預篩:編輯距離 ≤ threshold 的必要條件是長度差 ≤ threshold,
        // 擋掉的候選連 O(mn) 的 DP 都不用進。詳見 lowercase_char_len。
        if lowercase_char_len(basename).abs_diff(query_len) > threshold {
            continue;
        }
        let dist = damerau_levenshtein(&query_joined, basename);
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
    if !dead.is_empty() {
        store.retire_missing_batch(&dead);
    }

    if store.has_index() {
        // 兩階段共用一個快照(見 KeyStore::read_snapshot)。
        let _snap = store.read_snapshot();
        // 串流 + top-N + 兩段式:typo fallback 是「全查不到」時才走的路,更不能
        // 為它把整個索引的 path 搬進記憶體(實測 RSS 81MB / 0.5s)。
        let mut candidates: Vec<(f64, i64)> = Vec::new();
        store.for_each_index_basename(|rowid, basename| {
            // 同 history 迴圈的長度預篩:26.7 萬個 basename 每個都跑一次
            // O(mn) DP 是這條路徑最大的單一開銷,絕大多數連進 DP 都不必。
            if lowercase_char_len(basename).abs_diff(query_len) > threshold {
                return;
            }
            let dist = damerau_levenshtein(&query_joined, basename);
            if dist <= threshold {
                let max_len = query_joined.len().max(basename.len());
                #[allow(clippy::cast_precision_loss)]
                let sim = 1.0 - (dist as f64 / max_len as f64);
                candidates.push((sim * 10.0, rowid));
            }
        });
        results.extend(resolve_candidates(
            store,
            candidates,
            home.as_ref(),
            5.0,
            1.0,
            &boost_for,
        ));
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
