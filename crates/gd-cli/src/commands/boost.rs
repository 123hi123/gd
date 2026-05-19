use anyhow::{Context, Result};
use gd_core::db::KeyStore;
use gd_core::path::display_with_tilde;
use std::path::PathBuf;

pub fn add(store: &mut KeyStore, path: Option<&PathBuf>, weight: f64) -> Result<()> {
    let target = match path {
        Some(p) => p.clone(),
        None => std::env::current_dir().context("cannot determine current directory")?,
    };

    store.add_boost(&target, weight)?;
    store.save()?;

    let canonical = std::fs::canonicalize(&target)?;
    eprintln!("boosted {} (×{weight})", display_with_tilde(&canonical));
    Ok(())
}

pub fn remove(store: &mut KeyStore, path: &std::path::Path) -> Result<()> {
    store.remove_boost(path)?;
    store.save()?;
    eprintln!("removed boost for {}", display_with_tilde(path));
    Ok(())
}
