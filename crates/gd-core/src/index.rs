use rusqlite::{params, Connection};
use std::io;
use std::path::{Path, PathBuf};

pub struct PathIndex {
    conn: Connection,
}

impl PathIndex {
    pub fn open(data_dir: &Path) -> Self {
        let db_path = data_dir.join("gd.db");
        std::fs::create_dir_all(data_dir).ok();
        let conn = Connection::open(&db_path).expect("failed to open gd.db");
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 30000;
             PRAGMA synchronous = NORMAL;",
        )
        .expect("failed to set pragmas");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS dirs (
                path TEXT PRIMARY KEY,
                basename_lower TEXT NOT NULL,
                visits INTEGER NOT NULL DEFAULT 0,
                selections INTEGER NOT NULL DEFAULT 0,
                last_access INTEGER NOT NULL DEFAULT 0,
                in_index INTEGER NOT NULL DEFAULT 0
            );",
        )
        .expect("failed to create dirs table");
        Self { conn }
    }

    pub fn add(&self, path: PathBuf) {
        let Some((path_str, basename_lower)) = utf8_key(&path) else {
            return;
        };
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "INSERT INTO dirs (path, basename_lower, in_index)
             VALUES (?1, ?2, 1)
             ON CONFLICT(path) DO UPDATE SET
               in_index = 1,
               basename_lower = excluded.basename_lower",
        ) {
            stmt.execute(params![path_str, basename_lower]).ok();
        }
    }

    /// 只在「索引裡缺席」時才寫入:不存在的路徑 INSERT;存在但
    /// in_index = 0 的列(曾被刪除/移出、因留有歷史而保住的)修回 1;
    /// 其餘情況零寫入 — 不碰列、不長 WAL。回傳是否有實際變更。
    ///
    /// 給降級模式的 catchup 用:全樹補掃時,每個已索引目錄的成本是
    /// 一次 B-tree 主鍵探測(讀),而不是一次 upsert(寫),WAL 才不會
    /// 每輪被幾十萬筆無效寫入撐大再 checkpoint。
    pub fn add_if_missing(&self, path: PathBuf) -> bool {
        let Some((path_str, basename_lower)) = utf8_key(&path) else {
            return false;
        };
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "INSERT INTO dirs (path, basename_lower, in_index)
             VALUES (?1, ?2, 1)
             ON CONFLICT(path) DO UPDATE SET in_index = 1
             WHERE dirs.in_index = 0",
        ) {
            return stmt
                .execute(params![path_str, basename_lower])
                .map(|n| n > 0)
                .unwrap_or(false);
        }
        false
    }

    pub fn remove(&self, path: &Path) {
        let path_str = path.to_string_lossy();
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "DELETE FROM dirs WHERE path = ?1 AND visits = 0 AND selections = 0",
        ) {
            stmt.execute(params![path_str.as_ref()]).ok();
        }
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "UPDATE dirs SET in_index = 0 WHERE path = ?1",
        ) {
            stmt.execute(params![path_str.as_ref()]).ok();
        }
    }

    /// Move `old` and its entire subtree under `new` with a single prefix-rewrite
    /// UPDATE. History columns (visits/selections/last_access) are preserved in
    /// place — no filesystem rescan. Returns the number of rows moved.
    pub fn rename(&self, old: &Path, new: &Path) -> usize {
        let old_str = old.to_string_lossy();
        let new_str = new.to_string_lossy();
        let new_base = new
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        let (lo, hi) = subtree_bounds(&old_str);
        // OR REPLACE: if a stale entry already occupies the destination path
        // (e.g. a deleted dir that kept its history), drop it so the primary-key
        // move can land instead of silently failing on the UNIQUE constraint.
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "UPDATE OR REPLACE dirs
             SET path = ?2 || substr(path, length(?1) + 1),
                 basename_lower = CASE WHEN path = ?1 THEN ?3 ELSE basename_lower END
             WHERE path = ?1 OR (path >= ?4 AND path < ?5)",
        ) {
            return stmt
                .execute(params![
                    old_str.as_ref(),
                    new_str.as_ref(),
                    new_base,
                    lo,
                    hi
                ])
                .unwrap_or(0);
        }
        0
    }

    /// Remove `path` and its entire subtree. Entries with history are kept but
    /// marked out-of-index.
    ///
    /// 兩個用途:(a) 目錄被 rename 進排除區(不會有逐個子項的 delete 事件);
    /// (b) 目錄被刪除 — rmdir 要求目錄是空的,所以索引裡任何殘留的子項必然是
    /// 「漏收的 delete 事件」。`rm -rf` 就會漏:子項的事件要靠父目錄的 file
    /// handle 還原路徑,而父目錄往往在 daemon 讀到佇列前就已經被刪掉,handle
    /// 解不開 → 事件靜默丟棄。刪父時順手清整個子樹,這個洞就補起來了。
    pub fn remove_subtree(&self, path: &Path) {
        let path_str = path.to_string_lossy();
        let (lo, hi) = subtree_bounds(&path_str);
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "DELETE FROM dirs
             WHERE (path = ?1 OR (path >= ?2 AND path < ?3))
               AND visits = 0 AND selections = 0",
        ) {
            stmt.execute(params![path_str.as_ref(), lo, hi]).ok();
        }
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "UPDATE dirs SET in_index = 0
             WHERE path = ?1 OR (path >= ?2 AND path < ?3)",
        ) {
            stmt.execute(params![path_str.as_ref(), lo, hi]).ok();
        }
    }

    pub fn len(&self) -> usize {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM dirs WHERE in_index = 1",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn has_data(&self) -> bool {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM dirs WHERE in_index = 1)",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false)
    }

    pub fn begin_bulk(&self) {
        self.conn.execute_batch("BEGIN").ok();
    }

    /// 分批提交:COMMIT 後立刻 BEGIN,把單次持鎖時間壓到毫秒級。
    /// 刻意「不」做 `wal_checkpoint` — checkpoint 只屬於 `end_bulk`,
    /// 掃描中每批都 checkpoint 會把 WAL 反覆截斷成同步 I/O 風暴。
    pub fn commit_batch(&self) {
        self.conn.execute_batch("COMMIT").ok();
        self.conn.execute_batch("BEGIN").ok();
    }

    pub fn end_bulk(&self) {
        self.conn.execute_batch("COMMIT").ok();
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .ok();
    }

    pub fn flush(&self) -> io::Result<()> {
        if !self.conn.is_autocommit() {
            self.conn
                .execute_batch("COMMIT")
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        }
        Ok(())
    }

    pub fn mark_all_not_indexed(&self) {
        self.conn
            .execute("UPDATE dirs SET in_index = 0", [])
            .ok();
    }

    pub fn cleanup_stale(&self) {
        self.conn
            .execute(
                "DELETE FROM dirs WHERE in_index = 0 AND visits = 0 AND selections = 0",
                [],
            )
            .ok();
    }
}

/// 路徑含非 UTF-8 位元組時回 None。`to_string_lossy` 會把無效位元組換成
/// U+FFFD 寫進 primary key,產生一筆永遠對不回真實檔案、也永遠搜不到的
/// 壞資料(`basename_lower` 還會變空字串)。寧可不索引,不要寫壞資料。
///
/// 只有 `add` / `add_if_missing` 用它。`remove` / `remove_subtree` /
/// `rename` 是清理路徑,對既有的壞列還是要能刪,所以不套這層保護。
fn utf8_key(path: &Path) -> Option<(&str, String)> {
    let path_str = path.to_str()?;
    // file_name() 為 None 只可能是 "/" 或以 ".." 結尾,不是編碼問題,
    // 沿用原本的空 basename 行為。
    let basename_lower = match path.file_name() {
        Some(name) => name.to_str()?.to_lowercase(),
        None => String::new(),
    };
    Some((path_str, basename_lower))
}

/// 子樹的半開區間 `[lo, hi)`,用來取代 `path LIKE prefix || '/%'`。
///
/// 為什麼不用 LIKE:`path` 是 TEXT PRIMARY KEY(BINARY collation),但 SQLite
/// 預設的 LIKE 對 ASCII 大小寫不敏感,所以**無法**把前綴 LIKE 優化成索引範圍
/// 掃描 — 實測 EXPLAIN QUERY PLAN 是 `SCAN dirs`,36.8 萬列每次全表掃 36ms。
/// 換成明確的範圍比較就變成 `SEARCH dirs USING INDEX sqlite_autoindex_dirs_1
/// (path>? AND path<?)`。這在「每個 delete 事件都要清子樹」的場景是必要的,
/// 否則 `rm -rf` 大樹會變成幾千次全表掃。
///
/// 上界取 `prefix + "0"`:分隔符 `/` 是 0x2F,下一個位元組就是 `'0'`(0x30),
/// 所以 `[prefix + "/", prefix + "0")` 恰好涵蓋所有 `prefix/...` 的路徑,
/// 且不會誤收 `prefix-sibling` 這種同前綴但不同目錄的兄弟項。
fn subtree_bounds(prefix: &str) -> (String, String) {
    (format!("{prefix}/"), format!("{prefix}0"))
}
