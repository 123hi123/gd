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
        let path_str = path.to_string_lossy();
        let basename_lower = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        self.conn
            .execute(
                "INSERT INTO dirs (path, basename_lower, visits, selections, last_access)
                 VALUES (?1, ?2, 1, 0, ?3)
                 ON CONFLICT(path) DO UPDATE SET
                   visits = visits + 1,
                   last_access = excluded.last_access,
                   basename_lower = excluded.basename_lower",
                params![path_str.as_ref(), basename_lower, now],
            )
            .ok();
    }

    pub fn record_selection(&mut self, path: &Path) {
        let now = frecency::now_secs();
        let path_str = path.to_string_lossy();
        let basename_lower = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_lowercase();
        self.conn
            .execute(
                "INSERT INTO dirs (path, basename_lower, visits, selections, last_access)
                 VALUES (?1, ?2, 0, 1, ?3)
                 ON CONFLICT(path) DO UPDATE SET
                   selections = selections + 1,
                   last_access = excluded.last_access,
                   basename_lower = excluded.basename_lower",
                params![path_str.as_ref(), basename_lower, now],
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
                let score = frecency_score + match_quality_bonus(&basename_lower, &query_lower);
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
                let score = frecency_score + match_quality_bonus(&basename_lower, &last_kw);
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

    // --- Clean ---

    pub fn clean(&mut self) -> (Vec<(String, PathBuf)>, Vec<PathBuf>) {
        let mut removed_links = Vec::new();
        let mut removed_history = Vec::new();

        let links = self.list_links();
        for (alias, path) in &links {
            if !path.exists() {
                removed_links.push((alias.clone(), path.clone()));
            }
        }
        for (alias, _) in &removed_links {
            self.conn
                .execute("DELETE FROM links WHERE alias = ?1", params![alias])
                .ok();
        }

        let history = self.all_history();
        for (path, _) in &history {
            if !path.exists() {
                removed_history.push(path.clone());
            }
        }
        for path in &removed_history {
            let s = path.to_string_lossy();
            self.conn
                .execute(
                    "UPDATE dirs SET visits = 0, selections = 0, last_access = 0 WHERE path = ?1",
                    params![s.as_ref()],
                )
                .ok();
            self.conn
                .execute(
                    "DELETE FROM dirs WHERE path = ?1 AND in_index = 0",
                    params![s.as_ref()],
                )
                .ok();
        }

        (removed_links, removed_history)
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

pub fn match_quality_bonus(basename_lower: &str, query_lower: &str) -> f64 {
    if basename_lower == query_lower {
        10000.0
    } else if basename_lower.starts_with(query_lower) {
        500.0
    } else {
        0.0
    }
}

// --- Internal helpers ---

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
