use crate::error::Error;
use crate::frecency;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub visits: u64,
    pub selections: u64,
    pub last_access: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Database {
    pub version: u32,
    pub links: BTreeMap<String, PathBuf>,
    pub history: BTreeMap<PathBuf, HistoryEntry>,
    #[serde(default)]
    pub boosts: BTreeMap<PathBuf, f64>,
}

impl Default for Database {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            links: BTreeMap::new(),
            history: BTreeMap::new(),
            boosts: BTreeMap::new(),
        }
    }
}

pub struct KeyStore {
    db_path: PathBuf,
    db: Database,
}

impl KeyStore {
    pub fn open(data_dir: Option<&Path>) -> Result<Self, Error> {
        let db_path = match data_dir {
            Some(dir) => dir.join("db.json"),
            None => default_data_dir().join("db.json"),
        };

        let db = if db_path.exists() {
            let content = fs::read_to_string(&db_path)?;
            serde_json::from_str(&content)?
        } else {
            Database::default()
        };

        Ok(Self { db_path, db })
    }

    pub fn save(&self) -> Result<(), Error> {
        if let Some(parent) = self.db_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp_path = self.db_path.with_extension("json.tmp");
        let mut file = fs::File::create(&tmp_path)?;
        file.lock_exclusive()?;

        let json = serde_json::to_string_pretty(&self.db)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        drop(file);

        fs::rename(&tmp_path, &self.db_path)?;
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
        self.db.links.insert(alias.to_string(), canonical);
        Ok(())
    }

    pub fn remove_link(&mut self, alias: &str) -> Result<(), Error> {
        if self.db.links.remove(alias).is_none() {
            return Err(Error::KeyNotFound(alias.to_string()));
        }
        Ok(())
    }

    pub fn get_link(&self, alias: &str) -> Option<&PathBuf> {
        self.db.links.get(alias)
    }

    pub fn list_links(&self) -> &BTreeMap<String, PathBuf> {
        &self.db.links
    }

    // --- Boosts ---

    pub fn add_boost(&mut self, path: &Path, multiplier: f64) -> Result<(), Error> {
        let canonical = crate::path::normalize(path).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("cannot resolve path '{}': {e}", path.display()),
            ))
        })?;
        self.db.boosts.insert(canonical, multiplier);
        Ok(())
    }

    pub fn remove_boost(&mut self, path: &Path) -> Result<(), Error> {
        if self.db.boosts.remove(path).is_none() {
            let canonical = crate::path::normalize(path).ok();
            if let Some(ref c) = canonical {
                if self.db.boosts.remove(c).is_some() {
                    return Ok(());
                }
            }
            return Err(Error::KeyNotFound(path.display().to_string()));
        }
        Ok(())
    }

    pub fn list_boosts(&self) -> &BTreeMap<PathBuf, f64> {
        &self.db.boosts
    }

    pub fn boost_for(&self, path: &Path) -> f64 {
        for (boosted_dir, multiplier) in &self.db.boosts {
            if path.starts_with(boosted_dir) {
                return *multiplier;
            }
        }
        1.0
    }

    // --- History (cd hook) ---

    pub fn record_visit(&mut self, path: &Path) {
        let now = frecency::now_secs();
        let entry = self.db.history.entry(path.to_path_buf()).or_insert(HistoryEntry {
            visits: 0,
            selections: 0,
            last_access: now,
        });
        entry.visits += 1;
        entry.last_access = now;
    }

    pub fn record_selection(&mut self, path: &Path) {
        let now = frecency::now_secs();
        let entry = self.db.history.entry(path.to_path_buf()).or_insert(HistoryEntry {
            visits: 0,
            selections: 0,
            last_access: now,
        });
        entry.selections += 1;
        entry.last_access = now;
    }

    pub fn search_history(&self, query: &str) -> Vec<SearchResult> {
        let query_lower = query.to_lowercase();
        let now = frecency::now_secs();

        let mut results: Vec<SearchResult> = self
            .db
            .history
            .iter()
            .filter_map(|(path, entry)| {
                let basename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                let basename_lower = basename.to_lowercase();

                if !basename_lower.contains(&query_lower) || !path.exists() {
                    return None;
                }

                const SELECTED_TIER: f64 = 100_000.0;

                let decay = frecency::decay_factor(now.saturating_sub(entry.last_access));
                let frecency_score = if entry.selections > 0 {
                    SELECTED_TIER + (entry.selections as f64 * 10.0 + entry.visits as f64) * decay
                } else {
                    entry.visits as f64 * decay
                };

                let score = frecency_score + match_quality_bonus(&basename_lower, &query_lower);

                Some(SearchResult {
                    path: path.clone(),
                    score,
                    source: ResultSource::History,
                })
            })
            .collect();

        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results
    }

    // --- Clean ---

    pub fn clean(&mut self) -> (Vec<(String, PathBuf)>, Vec<PathBuf>) {
        let mut removed_links = Vec::new();
        let mut removed_history = Vec::new();

        let dead_links: Vec<String> = self
            .db
            .links
            .iter()
            .filter(|(_, path)| !path.exists())
            .map(|(alias, path)| {
                removed_links.push((alias.clone(), path.clone()));
                alias.clone()
            })
            .collect();
        for alias in dead_links {
            self.db.links.remove(&alias);
        }

        let dead_paths: Vec<PathBuf> = self
            .db
            .history
            .keys()
            .filter(|path| !path.exists())
            .cloned()
            .collect();
        for path in &dead_paths {
            removed_history.push(path.clone());
            self.db.history.remove(path);
        }

        (removed_links, removed_history)
    }

    pub fn export_json(&self) -> Result<String, Error> {
        Ok(serde_json::to_string_pretty(&self.db)?)
    }

    pub fn history_count(&self) -> usize {
        self.db.history.len()
    }

    pub fn link_count(&self) -> usize {
        self.db.links.len()
    }
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

fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gd")
}
