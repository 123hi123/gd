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

    // 這裡刻意沒有單一路徑版的 boost 查詢:呼叫端一律 `list_boosts()` 撈整張
    // 表(就幾列)再在記憶體裡比對。曾經有過 per-path 的 `boost_for`,結果被
    // 放進逐列迴圈,一次查詢跑了上萬次 SQL。
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

    /// **不要改下面那條 SQL 的 SELECT 欄位清單。**
    ///
    /// `path, basename_lower, visits, selections, last_access` + `WHERE
    /// basename_lower LIKE ? AND (visits > 0 OR selections > 0)` 恰好被部分索引
    /// `idx_dirs_history` 完全覆蓋(見 `create_query_indexes`),EXPLAIN 是
    /// `SCAN dirs USING COVERING INDEX idx_dirs_history`:只掃那 283 筆有歷史的
    /// 列,不碰 26.7 萬列的主表(實測 22.6 ms → 1.8 ms)。
    /// 多 SELECT 任何一個不在索引裡的欄位(例如 `in_index`)就會退回全表掃 —
    /// 靜默地慢 12 倍,沒有任何測試會發現。
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

    /// 同 `search_history`:**SELECT 的欄位清單不要動**,它靠
    /// `idx_dirs_history` 覆蓋才不必掃主表。
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

    /// 多關鍵字版的 `search_index`。兩段式的理由與 `search_index` 相同,見那裡。
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
                "SELECT path FROM dirs WHERE rowid IN
                   (SELECT rowid FROM dirs
                     WHERE in_index = 1 AND basename_lower LIKE ?1)",
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

    /// 兩段式:內層子查詢**只碰 `basename_lower` / `in_index` / rowid**,
    /// 因此整個過濾階段走 `idx_dirs_basename` 的 covering scan(6.5 MB);
    /// 外層才用 rowid 去主表把通過過濾的少數列的 `path` 取回來。
    ///
    /// 不要「簡化」成 `SELECT path FROM dirs WHERE in_index = 1 AND
    /// basename_lower LIKE ?1` —— 那樣 SELECT 引用了 `path`,索引就不再
    /// covering,SQLite 會退回掃 38 MB 的主表(實測 25 ms vs 16.8 ms,
    /// 而且加不加索引都一樣慢)。回傳值語意完全相同,差別只在執行計畫。
    pub fn search_index(&self, query: &str) -> Vec<PathBuf> {
        let query_lower = query.to_lowercase();
        let pattern = format!("%{query_lower}%");
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT path FROM dirs WHERE rowid IN
                   (SELECT rowid FROM dirs
                     WHERE in_index = 1 AND basename_lower LIKE ?1)",
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

    /// 開一個唯讀快照,在它活著的期間所有讀取都看到同一個版本的 DB。
    ///
    /// **fuzzy / typo fallback 的兩階段之間一定要有它。** 第一階段記下 rowid、
    /// 第二階段拿 rowid 換 path,而 `SQLite` 的 rowid 會回收 —— 刪掉目前最大
    /// rowid 的那一列之後,下一次 INSERT 就會拿到同一個 rowid。daemon 隨時在
    /// 做 delete + insert(`remove_subtree` 後面接一個新目錄的 `add` 就是),
    /// 所以沒有快照的話,第二階段可能拿到一條**完全沒有 match 過查詢**的路徑,
    /// 還把別人的分數貼上去。這種錯誤特別惡劣:那條路徑是剛剛才被建立的,
    /// 一定存在,所以 `prune_dead_top` 與選取後的 `exists()` 都攔不住,
    /// 使用者會直接被 cd 到一個莫名其妙的目錄。
    ///
    /// WAL 模式下的讀交易不擋寫入者,daemon 照常寫它的;代價只是這段期間
    /// (幾十毫秒)不能 checkpoint。
    ///
    /// 用 `BEGIN DEFERRED`:快照是在第一次「讀」的時候才建立的,所以不會為了
    /// 還沒開始的工作先卡住任何東西。
    pub fn read_snapshot(&self) -> ReadSnapshot<'_> {
        let began = self.conn.execute_batch("BEGIN DEFERRED").is_ok();
        ReadSnapshot {
            conn: &self.conn,
            began,
        }
    }

    /// 串流迭代索引列的 basename,**只碰 covering index、不碰主表**。
    ///
    /// 這是 fuzzy / typo fallback 的第一階段(過濾):先只看 basename 決定誰是
    /// 候選,再用 `paths_by_rowid` 把那少數候選的 `path` 取回來。rowid 本來就
    /// 隱含存在於每個索引項裡,所以 `SELECT rowid, basename_lower` 仍然是
    /// covering scan —— 掃 6.5 MB 的 `idx_dirs_basename`,而不是 38 MB 的主表
    /// (實測全撈 36.7 ms vs 帶 path 的 88.1 ms)。
    ///
    /// **千萬不要為了方便在這個 SQL 裡加上 `path`** —— 那一個欄位就會讓它退回
    /// 掃主表,而且不會有任何測試失敗、只是每次 fallback 慢一倍。
    ///
    /// 順帶一提,走索引也就代表**列的順序是 (`basename_lower`, rowid),不是
    /// rowid 順序**。呼叫端若拿順序當同分時的先後,結果會跟掃主表的版本不同
    /// (分數本身不受影響)。
    ///
    /// callback 收到 (rowid, `basename_lower`) 的借用,想留就自己複製。
    pub fn for_each_index_basename<F: FnMut(i64, &str)>(&self, mut f: F) {
        let Ok(mut stmt) = self
            .conn
            .prepare_cached("SELECT rowid, basename_lower FROM dirs WHERE in_index = 1")
        else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }) else {
            return;
        };
        for row in rows.flatten() {
            f(row.0, &row.1);
        }
    }

    /// 串流迭代索引列的 (rowid, path)。這是第二階段的**大集合**走法:候選多到
    /// 逐筆 rowid 探測不划算時,一次循序掃過去、用 rowid 對照候選集合。
    /// 成本與舊版的 `for_each_index_entry` 相同(實測全撈 88 ms),差別只在
    /// 不再需要 `basename_lower` —— 匹配在第一階段就做完了。
    pub fn for_each_index_path<F: FnMut(i64, &str)>(&self, mut f: F) {
        let Ok(mut stmt) = self
            .conn
            .prepare_cached("SELECT rowid, path FROM dirs WHERE in_index = 1")
        else {
            return;
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        }) else {
            return;
        };
        for row in rows.flatten() {
            f(row.0, &row.1);
        }
    }

    /// 依 rowid 批次取回路徑(`for_each_index_basename` 之後的第二階段用)。
    /// 回傳順序不保證,呼叫端自己配對/排序。查不到的 rowid 直接略過
    /// (併發下該列可能已被 daemon 刪掉)。
    ///
    /// 逐筆 rowid 主鍵查而不是組一條動態 `IN (...)` 字串:候選數是幾百,
    /// 每筆是一次 O(log n) 的 B-tree 探測,而動態字串會讓每次查詢的 SQL 都
    /// 不同,`prepare_cached` 完全失效。
    pub fn paths_by_rowid(&self, rowids: &[i64]) -> Vec<(i64, PathBuf)> {
        if rowids.is_empty() {
            return Vec::new();
        }
        let Ok(mut stmt) = self
            .conn
            .prepare_cached("SELECT path FROM dirs WHERE rowid = ?1")
        else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(rowids.len());
        for &rowid in rowids {
            if let Ok(path) = stmt.query_row(params![rowid], |row| {
                let s: String = row.get(0)?;
                Ok(PathBuf::from(s))
            }) {
                out.push((rowid, path));
            }
        }
        out
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

/// `KeyStore::read_snapshot` 的守衛。活著的期間,同一個連線上的所有讀取都看到
/// 同一個版本的 DB;drop 時結束交易。
///
/// 只讀不寫 —— 交易裡不要做任何寫入,否則 drop 時的 COMMIT 會把它一起送出去
/// (而呼叫端根本沒打算開一個寫交易)。
pub struct ReadSnapshot<'a> {
    conn: &'a rusqlite::Connection,
    /// `BEGIN` 有沒有成功。失敗通常代表這個連線上已經有交易在跑,那就不能
    /// 由我們來 COMMIT —— 會把別人的交易提早結束掉。
    began: bool,
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.began {
            self.conn.execute_batch("COMMIT").ok();
        }
    }
}

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
    // 索引純粹是效能設施,不是正確性的前提:少了它每次查詢慢幾十毫秒,但答案
    // 一模一樣。所以建不起來就算了,絕不能讓 `gd` 整個開不起來 —— 第一次建立
    // 是真寫入(36.8 萬列、6.5 MB、實測 152 ms 的寫鎖),若當下剛好撞上 daemon
    // 的掃描或 `gd clean` 而超過 busy_timeout,`?` 會讓每一次 `gd` 都失敗。
    if let Err(e) = create_query_indexes(conn) {
        eprintln!("gd: could not create query indexes ({e}); queries will be slower");
    }
    Ok(())
}

/// `dirs` 的兩個查詢索引。**同樣的兩行也存在於 `index.rs` 的 `PathIndex::open`** —
/// CLI 與 daemon 開的是同一個 DB,誰先開誰建,`IF NOT EXISTS` 讓它冪等。
/// 改這裡請同步改那裡。刻意不放在任何長交易裡:建索引要寫鎖,包進 bulk
/// transaction 會把持鎖時間從毫秒級拉到秒級,擋住另一邊的查詢。
///
/// **核心前提(改任何查詢前先讀這段)**:`SQLite` 只有在「查詢引用到的欄位全部
/// 都在索引裡」時才會走 covering index,否則它會拿索引找到 rowid、再回頭讀
/// 主表。`dirs` 表本體 38.8 MB、主鍵索引 35 MB,而 `idx_dirs_basename` 只有
/// 6.5 MB —— 差別就是掃 6.5 MB 還是 38.8 MB。實測:`SELECT path FROM dirs
/// WHERE in_index = 1 AND basename_lower LIKE ?` 加了索引仍要 25 ms(因為
/// SELECT 了 `path`,不是 covering);改成先用子查詢只取 rowid 再取 path 才
/// 降到 16.8 ms。
///
/// 也就是說:**在這些查詢的過濾階段多 SELECT 一個欄位,就會靜默退化回全表掃,
/// 而且不會有任何測試失敗**。要加欄位請先跑 EXPLAIN QUERY PLAN 確認還是
/// `COVERING INDEX`。
fn create_query_indexes(conn: &Connection) -> Result<(), Error> {
    conn.execute_batch(
        // 部分索引(WHERE 子句):只涵蓋「有歷史」的列。實測全庫 26.7 萬列裡
        // 只有 283 列有歷史,所以這個索引只有 24 KB,卻讓 search_history /
        // search_history_multi / history_count / all_history 從「在 26.7 萬列
        // 裡全表掃出那 283 列」變成掃一個 24 KB 的索引(22.6 ms → 1.8 ms)。
        // 欄位清單刻意涵蓋 search_history 用到的全部五欄,少一欄就不是 covering。
        "CREATE INDEX IF NOT EXISTS idx_dirs_history
            ON dirs(basename_lower, path, visits, selections, last_access)
            WHERE visits > 0 OR selections > 0;

        -- 覆蓋索引:讓「只看 basename 的過濾階段」不必碰 38 MB 的主表。
        -- 欄位順序不要改:basename_lower 必須在前,這樣它才能同時服務
        -- 前綴/範圍形式的查詢;in_index 只是為了把過濾條件也蓋進索引,
        -- 讓 SQLite 不必為了判斷 in_index 而回主表取列。
        CREATE INDEX IF NOT EXISTS idx_dirs_basename ON dirs(basename_lower, in_index);",
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
