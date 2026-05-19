use anyhow::Result;
use gd_core::db::KeyStore;
use std::path::Path;

pub fn run(store: &mut KeyStore, path: &Path) -> Result<()> {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        store.record_visit(&canonical);
        store.save()?;
    }
    Ok(())
}
