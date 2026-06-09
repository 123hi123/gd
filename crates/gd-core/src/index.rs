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
        let path_str = path.to_string_lossy();
        let basename_lower = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "INSERT INTO dirs (path, basename_lower, in_index)
             VALUES (?1, ?2, 1)
             ON CONFLICT(path) DO UPDATE SET
               in_index = 1,
               basename_lower = excluded.basename_lower",
        ) {
            stmt.execute(params![path_str.as_ref(), basename_lower]).ok();
        }
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
        let old_like = escape_like(&old_str);
        // OR REPLACE: if a stale entry already occupies the destination path
        // (e.g. a deleted dir that kept its history), drop it so the primary-key
        // move can land instead of silently failing on the UNIQUE constraint.
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "UPDATE OR REPLACE dirs
             SET path = ?2 || substr(path, length(?1) + 1),
                 basename_lower = CASE WHEN path = ?1 THEN ?3 ELSE basename_lower END
             WHERE path = ?1 OR path LIKE ?4 || '/%' ESCAPE '\\'",
        ) {
            return stmt
                .execute(params![old_str.as_ref(), new_str.as_ref(), new_base, old_like])
                .unwrap_or(0);
        }
        0
    }

    /// Remove `path` and its entire subtree (prefix match). Entries with history
    /// are kept but marked out-of-index. Used when a directory is renamed into an
    /// excluded location, which produces no per-child delete events.
    pub fn remove_subtree(&self, path: &Path) {
        let path_str = path.to_string_lossy();
        let like = escape_like(&path_str);
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "DELETE FROM dirs
             WHERE (path = ?1 OR path LIKE ?2 || '/%' ESCAPE '\\')
               AND visits = 0 AND selections = 0",
        ) {
            stmt.execute(params![path_str.as_ref(), like]).ok();
        }
        if let Ok(mut stmt) = self.conn.prepare_cached(
            "UPDATE dirs SET in_index = 0
             WHERE path = ?1 OR path LIKE ?2 || '/%' ESCAPE '\\'",
        ) {
            stmt.execute(params![path_str.as_ref(), like]).ok();
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

/// Escape SQL LIKE wildcards (`%` `_`) and the escape char itself so a literal
/// path can be used safely as a prefix pattern. Pairs with `ESCAPE '\'`.
fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}
