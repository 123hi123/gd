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
