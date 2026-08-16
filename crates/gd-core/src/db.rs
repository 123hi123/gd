use crate::error::Error;
use crate::frecency;
use rusqlite::{params, Connection};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct KeyStore {
    conn: Connection,
}

impl KeyStore {
    pub fn open(data_dir: Option<&Path>) -> Result<Self, Error> {
        let dir = match data_dir {
            Some(d) => d.to_path_buf(),
            None => default_data_dir(),
        };
        let db_path = dir.join("gd.db");

        std::fs::create_dir_all(&dir)?;

        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 30000;
             PRAGMA synchronous = NORMAL;",
        )?;
        init_schema(&conn)?;
        migrate_from_json(&conn, &dir)?;

        Ok(Self { conn })
    }

    pub fn save(&self) -> Result<(), Error> {
        Ok(())
    }

    // --- Links ---

    pub fn add_link(&mut self, alias: &str, path: &Path) -> Result<(), Error> {
        let canonical = crate::path::normalize(path).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("cannot resolve path '{}': {e}", path.display()),
            ))
        })?;
        self.conn.execute(
            "INSERT OR REPLACE INTO links (alias, path) VALUES (?1, ?2)",
            params![alias, canonical.to_string_lossy().as_ref()],
        )?;
        Ok(())
    }

    pub fn remove_link(&mut self, alias: &str) -> Result<(), Error> {
        let changes = self.conn.execute(
            "DELETE FROM links WHERE alias = ?1",
            params![alias],
        )?;
        if changes == 0 {
            return Err(Error::KeyNotFound(alias.to_string()));
        }
        Ok(())
    }

    pub fn get_link(&self, alias: &str) -> Option<PathBuf> {
        self.conn
            .query_row(
                "SELECT path FROM links WHERE alias = ?1",
                params![alias],
                |row| {
                    let s: String = row.get(0)?;
                    Ok(PathBuf::from(s))
                },
            )
            .ok()
    }

    pub fn list_links(&self) -> BTreeMap<String, PathBuf> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT alias, path FROM links ORDER BY alias")
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                let alias: String = row.get(0)?;
                let path: String = row.get(1)?;
                Ok((alias, PathBuf::from(path)))
            })
            .unwrap();
        rows.filter_map(Result::ok).collect()
    }

    /// 查詢時發現路徑已不存在的「順手修正」:純索引列(無任何歷史)直接
    /// 刪掉;帶歷史的列只標 in_index = 0 保留 — 它可能只是外接碟/網路掛載
    /// 暫時不在,徹底清除是手動 `gd clean` 的職責。這讓死路徑第一次被查到
    /// 就退場,索引不再依賴任何定期全掃來清屍體。
    pub fn retire_missing(&self, path: &Path) {
        self.retire_missing_batch(std::slice::from_ref(&path.to_path_buf()));
    }

    /// 一次交易退場一整批死路徑。查詢路徑上一次可能撞到數千筆死列,
    /// 逐筆各自成交易的話會發出上千次 fsync,並且每筆都可能撞上 daemon
    /// 掃描的寫鎖各吃一次 `busy_timeout`。
    pub fn retire_missing_batch(&self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        // 一個 IMMEDIATE 交易包住整批:只 fsync 一次、只搶一次寫鎖。
        let began = self.conn.execute_batch("BEGIN IMMEDIATE").is_ok();
        for path in paths {
            let s = path.to_string_lossy();
            if let Ok(mut stmt) = self.conn.prepare_cached(
                "DELETE FROM dirs WHERE path = ?1 AND visits = 0 AND selections = 0",
            ) {
                stmt.execute(params![s.as_ref()]).ok();
            }
            if let Ok(mut stmt) = self
                .conn
                .prepare_cached("UPDATE dirs SET in_index = 0 WHERE path = ?1")
            {
                stmt.execute(params![s.as_ref()]).ok();
            }
        }
        if began {
            self.conn.execute_batch("COMMIT").ok();
        }
    }

    // --- Settings ---

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .ok()
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<(), Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
        Ok(())
    }

    // --- Boosts ---

    pub fn add_boost(&mut self, path: &Path, multiplier: f64) -> Result<(), Error> {
        let canonical = crate::path::normalize(path).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("cannot resolve path '{}': {e}", path.display()),
            ))
        })?;
        self.conn.execute(
            "INSERT OR REPLACE INTO boosts (path, multiplier) VALUES (?1, ?2)",
            params![canonical.to_string_lossy().as_ref(), multiplier],
        )?;
        Ok(())
    }

    pub fn remove_boost(&mut self, path: &Path) -> Result<(), Error> {
        let path_str = path.to_string_lossy();
        let changes = self.conn.execute(
            "DELETE FROM boosts WHERE path = ?1",
            params![path_str.as_ref()],
        )?;
        if changes == 0 {
            if let Ok(canonical) = crate::path::normalize(path) {
                let c_str = canonical.to_string_lossy();
                let c2 = self.conn.execute(
                    "DELETE FROM boosts WHERE path = ?1",
                    params![c_str.as_ref()],
                )?;
                if c2 > 0 {
                    return Ok(());
                }
            }
            return Err(Error::KeyNotFound(path.display().to_string()));
        }
        Ok(())
    }

    pub fn list_boosts(&self) -> BTreeMap<PathBuf, f64> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path, multiplier FROM boosts ORDER BY path")
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                let path: String = row.get(0)?;
                let mult: f64 = row.get(1)?;
                Ok((PathBuf::from(path), mult))
            })
            .unwrap();
        rows.filter_map(Result::ok).collect()
    }

    pub fn boost_for(&self, path: &Path) -> f64 {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path, multiplier FROM boosts")
            .unwrap();
        let mut rows = stmt.query([]).unwrap();
        while let Ok(Some(row)) = rows.next() {
            let boosted: String = row.get(0).unwrap();
            let mult: f64 = row.get(1).unwrap();
            if path.starts_with(&boosted) {
                return mult;
            }
        }
        1.0
    }

    // --- History ---

    pub fn record_visit(&mut self, path: &Path) {
        let now = frecency::now_secs();
        // 已知限制:非 UTF-8 路徑在這裡仍走 to_string_lossy,無效位元組會被
        // 換成 U+FFFD。歷史是使用者真的走過的目錄,像 index.rs 那樣直接跳過
        // 會讓 gd 對這些目錄完全失憶,比留一筆近似鍵更糟,故維持現行行為。
        let path_str = path.to_string_lossy();
        let path_str = trim_trailing_slash(&path_str);
        let basename_lower = basename_lower_of(path_str);
        self.conn
            .execute(
                "INSERT INTO dirs (path, basename_lower, visits, selections, last_access)
                 VALUES (?1, ?2, 1, 0, ?3)
                 ON CONFLICT(path) DO UPDATE SET
                   visits = visits + 1,
                   last_access = excluded.last_access,
                   basename_lower = excluded.basename_lower",
                params![path_str, basename_lower, now],
            )
            .ok();
    }

    pub fn record_selection(&mut self, path: &Path) {
        let now = frecency::now_secs();
        // 同 record_visit:非 UTF-8 路徑維持 lossy,不跳過。
        let path_str = path.to_string_lossy();
        let path_str = trim_trailing_slash(&path_str);
        let basename_lower = basename_lower_of(path_str);
        self.conn
            .execute(
                "INSERT INTO dirs (path, basename_lower, visits, selections, last_access)
                 VALUES (?1, ?2, 0, 1, ?3)
                 ON CONFLICT(path) DO UPDATE SET
                   selections = selections + 1,
                   last_access = excluded.last_access,
                   basename_lower = excluded.basename_lower",
                params![path_str, basename_lower, now],
            )
            .ok();
    }

    pub fn search_history(&self, query: &str) -> Vec<SearchResult> {
        let query_lower = query.to_lowercase();
        let now = frecency::now_secs();
        let pattern = format!("%{query_lower}%");

        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT path, basename_lower, visits, selections, last_access
                 FROM dirs
                 WHERE basename_lower LIKE ?1
                   AND (visits > 0 OR selections > 0)",
            )
            .unwrap();

        let mut results: Vec<SearchResult> = stmt
            .query_map(params![pattern], |row| {
                let path_str: String = row.get(0)?;
                let basename_lower: String = row.get(1)?;
                let visits: u64 = row.get(2)?;
                let selections: u64 = row.get(3)?;
                let last_access: u64 = row.get(4)?;
                Ok((
                    PathBuf::from(path_str),
                    basename_lower,
                    visits,
                    selections,
                    last_access,
                ))
            })
            .unwrap()
            .filter_map(Result::ok)
            .filter(|(path, _, _, _, _)| path.exists())
            .map(|(path, basename_lower, visits, selections, last_access)| {
                const SELECTED_TIER: f64 = 100_000.0;
                let decay = frecency::decay_factor(now.saturating_sub(last_access));
                let frecency_score = if selections > 0 {
                    SELECTED_TIER + (selections as f64 * 10.0 + visits as f64) * decay
                } else {
                    visits as f64 * decay
                };
                let score = frecency_score + match_quality_tiebreak(&basename_lower, &query_lower);
                SearchResult {
                    path,
                    score,
                    source: ResultSource::History,
                }
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    pub fn search_history_multi(&self, keywords: &[&str]) -> Vec<SearchResult> {
        let now = frecency::now_secs();
        let ordered = keywords
            .iter()
            .map(|k| k.to_lowercase())
            .collect::<Vec<_>>()
            .join("%");
        let pattern = format!("%{ordered}%");
        let last_kw = keywords.last().copied().unwrap_or("").to_lowercase();

        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT path, basename_lower, visits, selections, last_access
                 FROM dirs
                 WHERE basename_lower LIKE ?1
                   AND (visits > 0 OR selections > 0)",
            )
            .unwrap();

        let mut results: Vec<SearchResult> = stmt
            .query_map(params![pattern], |row| {
                let path_str: String = row.get(0)?;
                let basename_lower: String = row.get(1)?;
                let visits: u64 = row.get(2)?;
                let selections: u64 = row.get(3)?;
                let last_access: u64 = row.get(4)?;
                Ok((
                    PathBuf::from(path_str),
                    basename_lower,
                    visits,
                    selections,
                    last_access,
                ))
            })
            .unwrap()
            .filter_map(Result::ok)
            .filter(|(path, _, _, _, _)| path.exists())
            .map(|(path, basename_lower, visits, selections, last_access)| {
                const SELECTED_TIER: f64 = 100_000.0;
                let decay = frecency::decay_factor(now.saturating_sub(last_access));
                let frecency_score = if selections > 0 {
                    SELECTED_TIER + (selections as f64 * 10.0 + visits as f64) * decay
                } else {
                    visits as f64 * decay
                };
                let score = frecency_score + match_quality_tiebreak(&basename_lower, &last_kw);
                SearchResult {
                    path,
                    score,
                    source: ResultSource::History,
                }
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    pub fn search_index_multi(&self, keywords: &[&str]) -> Vec<PathBuf> {
        let ordered = keywords
            .iter()
            .map(|k| k.to_lowercase())
            .collect::<Vec<_>>()
            .join("%");
        let pattern = format!("%{ordered}%");

        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT path FROM dirs
                 WHERE in_index = 1 AND basename_lower LIKE ?1",
            )
            .unwrap();
        stmt.query_map(params![pattern], |row| {
            let s: String = row.get(0)?;
            Ok(PathBuf::from(s))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect()
    }

    // --- Index queries (replaces index::search_file / index::index_exists) ---

    pub fn has_index(&self) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM dirs WHERE in_index = 1)",
                [],
                |row| row.get(0),
            )
            .unwrap_or(false)
    }

    pub fn search_index(&self, query: &str) -> Vec<PathBuf> {
        let query_lower = query.to_lowercase();
        let pattern = format!("%{query_lower}%");
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT path FROM dirs
                 WHERE in_index = 1 AND basename_lower LIKE ?1",
            )
            .unwrap();
        stmt.query_map(params![pattern], |row| {
            let s: String = row.get(0)?;
            Ok(PathBuf::from(s))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect()
    }

    pub fn all_index_entries(&self) -> Vec<(String, String)> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path, basename_lower FROM dirs WHERE in_index = 1")
            .unwrap();
        stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .filter_map(Result::ok)
        .collect()
    }

    /// 串流迭代索引列,不把 36.8 萬列一次具現成 Vec。
    /// callback 收到 (path, `basename_lower`) 的借用,想留就自己複製。
    pub fn for_each_index_entry<F: FnMut(&str, &str)>(&self, mut f: F) {
        let Ok(mut stmt) = self
            .conn
            .prepare_cached("SELECT path, basename_lower FROM dirs WHERE in_index = 1")
        else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) else {
            return;
        };
        for row in rows.flatten() {
            f(&row.0, &row.1);
        }
    }

    // --- Clean ---

    /// 徹底清掃死路徑:links、有歷史的列、以及**純索引列**全部 stat 一次。
    ///
    /// 舊版只掃 links + `all_history()` (全庫幾百列),36 萬筆純索引列一列都沒碰,
    /// 於是明明近三成索引是死的,gd clean 卻回報「nothing to clean」。
    /// 舊版還會對死的歷史列先 `UPDATE ... visits = 0, selections = 0` 再
    /// `DELETE ... AND in_index = 0`:碰到 `in_index = 1` 的死列就變成歷史被洗掉、
    /// 列卻刪不掉,從此 `all_history()` 再也撈不到它 —— 永久不可回收。現在一律
    /// 無條件 DELETE,不再有洗白那一步。
    ///
    /// 判定死活只靠 stat,不看 `in_index` (它的語意是「daemon 的索引目前
    /// 包含這條路徑」,不是「路徑還活著」)。
    pub fn clean(&mut self) -> CleanReport {
        let mut report = CleanReport {
            removed_links: Vec::new(),
            removed_history: Vec::new(),
            removed_index: 0,
            scanned: 0,
        };

        let links = self.list_links();
        for (alias, path) in &links {
            if !path.exists() {
                report.removed_links.push((alias.clone(), path.clone()));
            }
        }
        for (alias, _) in &report.removed_links {
            self.conn
                .execute("DELETE FROM links WHERE alias = ?1", params![alias])
                .ok();
        }

        let total: usize = self
            .conn
            .query_row("SELECT COUNT(*) FROM dirs", [], |row| row.get(0))
            .unwrap_or(0);

        // 第一階段:streaming 掃全表 stat,只把「死的」收進記憶體。
        // (36.8 萬列一次具現成 Vec 會吃掉幾十 MB;死列通常遠少於總數。)
        // stmt 借用了 self.conn,得在開 transaction 前 drop,所以整段包成 block。
        let mut dead: Vec<(String, bool)> = Vec::new();
        {
            let Ok(mut stmt) = self
                .conn
                .prepare("SELECT path, visits, selections FROM dirs")
            else {
                return report;
            };
            let Ok(rows) = stmt.query_map([], |row| {
                let path: String = row.get(0)?;
                let visits: u64 = row.get(1)?;
                let selections: u64 = row.get(2)?;
                Ok((path, visits > 0 || selections > 0))
            }) else {
                return report;
            };
            for (path, has_history) in rows.flatten() {
                report.scanned += 1;
                if report.scanned % 5000 == 0 {
                    // 36.8 萬次 stat 會跑好一陣子,沒進度使用者會以為當掉了。
                    eprint!("\rgd clean: scanned {}/{total}...", report.scanned);
                }
                if !Path::new(&path).exists() {
                    dead.push((path, has_history));
                }
            }
        }
        if report.scanned >= 5000 {
            // 尾端補空白蓋掉上一行進度殘留的 "..",最後務必換行。
            eprintln!("\rgd clean: scanned {}/{total}.  ", report.scanned);
        }

        // 第二階段:一個 transaction 刪完整批。
        if !dead.is_empty() {
            let began = self.conn.execute_batch("BEGIN IMMEDIATE").is_ok();
            for (path, has_history) in &dead {
                let deleted = self
                    .conn
                    .prepare_cached("DELETE FROM dirs WHERE path = ?1")
                    .and_then(|mut stmt| stmt.execute(params![path]))
                    .unwrap_or(0);
                if deleted == 0 {
                    continue;
                }
                if *has_history {
                    report.removed_history.push(PathBuf::from(path));
                } else {
                    report.removed_index += 1;
                }
            }
            if began {
                self.conn.execute_batch("COMMIT").ok();
            }
        }

        report
    }

    pub fn export_json(&self) -> Result<String, Error> {
        #[derive(Serialize)]
        struct ExportDb {
            version: u32,
            links: BTreeMap<String, PathBuf>,
            history: BTreeMap<PathBuf, HistoryEntry>,
            boosts: BTreeMap<PathBuf, f64>,
        }
        let db = ExportDb {
            version: 2,
            links: self.list_links(),
            history: self.all_history(),
            boosts: self.list_boosts(),
        };
        Ok(serde_json::to_string_pretty(&db)?)
    }

    pub fn all_history(&self) -> BTreeMap<PathBuf, HistoryEntry> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT path, visits, selections, last_access
                 FROM dirs WHERE visits > 0 OR selections > 0",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                let path: String = row.get(0)?;
                let visits: u64 = row.get(1)?;
                let selections: u64 = row.get(2)?;
                let last_access: u64 = row.get(3)?;
                Ok((
                    PathBuf::from(path),
                    HistoryEntry {
                        visits,
                        selections,
                        last_access,
                    },
                ))
            })
            .unwrap();
        rows.filter_map(Result::ok).collect()
    }

    pub fn history_count(&self) -> usize {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM dirs WHERE visits > 0 OR selections > 0",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }

    pub fn link_count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM links", [], |row| row.get(0))
            .unwrap_or(0)
    }
}

// --- Public types ---

/// `gd clean` 的成果報告。
#[derive(Debug, Clone, Default)]
pub struct CleanReport {
    /// 指向不存在路徑、已刪除的 link
    pub removed_links: Vec<(String, PathBuf)>,
    /// 有歷史(visits/selections > 0)且路徑已不存在、已刪除的列
    pub removed_history: Vec<PathBuf>,
    /// 純索引列(無歷史)且路徑已不存在、已刪除的筆數
    pub removed_index: usize,
    /// 實際 stat 過的列數(讓 CLI 能說「掃了 N 筆」)
    pub scanned: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryEntry {
    pub visits: u64,
    pub selections: u64,
    pub last_access: u64,
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: PathBuf,
    pub score: f64,
    pub source: ResultSource,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub score: f64,
    pub source: ResultSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultSource {
    Link,
    History,
    Filesystem,
}

/// Tiebreaker added to a *history* dir's frecency score: among dirs the user has
/// used equally often, an exact basename match edges out a mere prefix match, and a
/// prefix match edges out a loose substring match. Deliberately kept far below a
/// single visit step (`1 * min_decay 0.25 = 0.25`), so it can NEVER reorder dirs
/// that differ in selection/visit count — usage history decides the order, name
/// match only breaks otherwise-exact ties. Mirrors the `HISTORY_MATCH_TIEBREAK`
/// discipline already used in the fuzzy/typo fallbacks (see commands/jump.rs).
///
/// Previously this added a full +10000 (exact) / +500 (prefix), which let match
/// quality swamp the pick count: a prefix match like `qwen2api-rs` could never
/// overtake an exact `qwen2api` no matter how many times it was selected (it would
/// have needed ~950 more selections to close the 9500-point gap).
pub fn match_quality_tiebreak(basename_lower: &str, query_lower: &str) -> f64 {
    if basename_lower == query_lower {
        0.01
    } else if basename_lower.starts_with(query_lower) {
        0.005
    } else {
        0.0
    }
}

// --- Internal helpers ---

/// 去掉尾端斜線。DB 實測有 `/home/joe/文件/tools/gcpcontrol` 和同名帶斜線的
/// 版本並存,同一個目錄的 selections 被拆成 101 + 11 兩份。
///
/// 刻意不用 `crate::path::normalize` — 那是 canonicalize:會解 symlink、
/// 也會對不存在的路徑直接失敗,語意改動太大。這裡只要純字串正規化。
fn trim_trailing_slash(s: &str) -> &str {
    let trimmed = s.trim_end_matches('/');
    // 根目錄 "/" 不能被砍成空字串。
    if trimmed.is_empty() {
        "/"
    } else {
        trimmed
    }
}

fn basename_lower_of(path_str: &str) -> String {
    Path::new(path_str)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gd")
}

fn init_schema(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dirs (
            path TEXT PRIMARY KEY,
            basename_lower TEXT NOT NULL,
            visits INTEGER NOT NULL DEFAULT 0,
            selections INTEGER NOT NULL DEFAULT 0,
            last_access INTEGER NOT NULL DEFAULT 0,
            in_index INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS links (
            alias TEXT PRIMARY KEY,
            path TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS boosts (
            path TEXT PRIMARY KEY,
            multiplier REAL NOT NULL
        );
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );",
    )?;
    Ok(())
}

fn migrate_from_json(conn: &Connection, dir: &Path) -> Result<(), Error> {
    let json_path = dir.join("db.json");
    if !json_path.exists() {
        return Ok(());
    }

    use serde::Deserialize;

    #[derive(Deserialize)]
    struct OldDb {
        #[serde(default)]
        links: BTreeMap<String, PathBuf>,
        #[serde(default)]
        history: BTreeMap<PathBuf, OldEntry>,
        #[serde(default)]
        boosts: BTreeMap<PathBuf, f64>,
    }

    #[derive(Deserialize)]
    struct OldEntry {
        visits: u64,
        selections: u64,
        last_access: u64,
    }

    let content = std::fs::read_to_string(&json_path)?;
    let old: OldDb = serde_json::from_str(&content)?;

    conn.execute_batch("BEGIN")?;

    for (path, entry) in &old.history {
        let path_str = path.to_string_lossy();
        let basename_lower = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        conn.execute(
            "INSERT OR REPLACE INTO dirs (path, basename_lower, visits, selections, last_access)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                path_str.as_ref(),
                basename_lower,
                entry.visits,
                entry.selections,
                entry.last_access
            ],
        )?;
    }

    for (alias, path) in &old.links {
        conn.execute(
            "INSERT OR REPLACE INTO links (alias, path) VALUES (?1, ?2)",
            params![alias, path.to_string_lossy().as_ref()],
        )?;
    }

    for (path, mult) in &old.boosts {
        conn.execute(
            "INSERT OR REPLACE INTO boosts (path, multiplier) VALUES (?1, ?2)",
            params![path.to_string_lossy().as_ref(), mult],
        )?;
    }

    conn.execute_batch("COMMIT")?;

    let migrated = json_path.with_extension("json.migrated");
    std::fs::rename(&json_path, &migrated).ok();

    eprintln!(
        "gd: migrated db.json → SQLite ({} history, {} links, {} boosts)",
        old.history.len(),
        old.links.len(),
        old.boosts.len()
    );

    Ok(())
}
