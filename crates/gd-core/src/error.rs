use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("key '{0}' not found")]
    KeyNotFound(String),

    #[error("no matches found for '{0}'")]
    NoMatches(String),

    #[error("link already exists: '{0}' → {1}")]
    LinkExists(String, PathBuf),

    #[error("database I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database parse error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}
